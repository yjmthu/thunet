//! Session state machine: ensure online → poll the portal → re-login on a drop.
//!
//! This is the "auto connect + keep online" behaviour requested from the
//! shipped client, minus its GUI. It is deliberately UI-free so the same code
//! runs headless on a router or inside the tray app.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::CoreConfig;
use crate::logline::Line;
use crate::srun::{Client, NetEnv, NetError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
  /// The portal has no session for this address. Not "no internet": a campus
  /// link can carry traffic while the portal still reports nobody signed in.
  #[serde(rename = "Offline")]
  LoggedOut,
  Authenticating,
  /// The portal reports a session for this address, or this core just opened
  /// one. The wire keeps the old names, so a window and a service from
  /// different builds still understand each other.
  #[serde(rename = "Online")]
  LoggedIn,
}

/// Why the last login attempt failed, so the caller can decide how much of the
/// portal's answer to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureKind {
  #[default]
  Other,
  /// Wrong account or password: retrying cannot help, and the raw JSON is noise.
  Credentials,
  /// The portal could not route the login to the access controller named by
  /// `ac_id`, which is what an id borrowed from another link looks like.
  AcUnreachable,
}

pub struct Session {
  client: Client,
  cfg: CoreConfig,
  /// True when `ac_id` was auto-detected (so it must be refreshed per connect).
  auto_ac: bool,
  /// Diagnostics produced off the UI thread; drained into the window log.
  warnings: Vec<Line>,
  pub state: State,
  pub failures: u32,
  /// Set by `ensure_online`; read by the UI right after an `Err`.
  pub failure: FailureKind,
}

fn ecode(v: &serde_json::Value) -> String {
  v.get("ecode")
    .and_then(|e| e.as_str())
    .unwrap_or("no ecode")
    .to_string()
}

fn error_msg(v: &serde_json::Value) -> String {
  v.get("error_msg")
    .and_then(|e| e.as_str())
    .unwrap_or_default()
    .to_string()
}

#[allow(dead_code)] // some accessors are consumed only by the tray/UI layer
impl Session {
  pub fn new(mut cfg: CoreConfig) -> Self {
    // The account and the password are one credential: a name without a
    // password cannot sign in, and keeping it would leave a half-set pair that
    // `credential_state` reports and `--remember-password off` then "clears" -
    // erasing a name the user never meant to give up. The config file may keep
    // the name as a hint for the window's prompt; the core holds only complete
    // pairs.
    if cfg.password.is_empty() {
      cfg.username.clear();
    }
    // Three seconds: a portal poll that has not answered by then is treated as
    // silent, and waiting any longer only holds up the core's own loop. Logins
    // use `LOGIN_TIMEOUT` instead (see `Client::new`), so the challenge round
    // trip is not cut short by this.
    let client = Client::new(&cfg.host, cfg.https, Duration::from_secs(3));
    // `auto` must be resolved once: the AC id changes with the network.
    let auto_ac = cfg.ac_id.is_empty() || cfg.ac_id == "auto";
    let mut warnings = Vec::new();
    if auto_ac {
      // Detection needs the network, and this constructor also runs on the
      // core's startup path, which may be before the link is up (systemd unit
      // or logon task). Leave a placeholder; the first probe — or the retry
      // inside `ensure_online` — replaces it with what the portal reports.
      cfg.ac_id = "1".into();
      warnings.push(Line::debug("ac_id: auto, probing on the first connect"));
    }
    Self {
      client,
      cfg,
      auto_ac,
      warnings,
      state: State::LoggedOut,
      failures: 0,
      failure: FailureKind::Other,
    }
  }

  /// Take the pending diagnostic lines (startup + re-detection).
  pub fn take_warnings(&mut self) -> Vec<Line> {
    std::mem::take(&mut self.warnings)
  }

  /// Update the credentials used by the next login attempt.
  ///
  /// The pair is atomic here as well: an empty password clears the name with
  /// it, so no caller can leave the session holding half a credential.
  pub fn set_credentials(&mut self, username: String, password: String) {
    if password.is_empty() {
      self.cfg.username.clear();
    } else {
      self.cfg.username = username;
    }
    self.cfg.password = password;
  }

  pub fn client(&self) -> &Client {
    &self.client
  }

  pub fn cfg(&self) -> &CoreConfig {
    &self.cfg
  }

  /// The core owns the credential, so it also maintains the flags that decide
  /// whether that credential is read back on the next start.
  pub fn cfg_mut(&mut self) -> &mut CoreConfig {
    &mut self.cfg
  }

  /// Query the portal and sync the local state with reality.
  pub fn refresh(&mut self) -> Result<serde_json::Value, NetError> {
    let v = self.client.status(&self.cfg.ac_id)?;
    let online = v.get("online").and_then(|b| b.as_bool()).unwrap_or(false);
    self.state = if online {
      State::LoggedIn
    } else {
      State::LoggedOut
    };
    if online {
      self.failures = 0;
    }
    Ok(v)
  }

  /// Cheap pre-check used before connect/disconnect actions.
  pub fn is_online_now(&self) -> bool {
    self.client.is_online(&self.cfg.ac_id)
  }

  /// Is the machine even attached to a network where logging in makes sense?
  pub fn net_env(&self) -> NetEnv {
    self.client.net_env()
  }

  /// What the campus portal says about this client: whether it has a live
  /// session for the address it sees, and that address.
  ///
  /// The address may be a router's or a VPN's rather than this machine's own:
  /// that is exactly what makes the answer useful behind NAT, where comparing
  /// it with the local default-route address can never match.
  pub fn portal_view(&self) -> Option<crate::srun::PortalView> {
    self.client.portal_view()
  }

  /// Adopt an ac_id found by the caller's probe (only when auto-detecting).
  pub fn set_ac_id(&mut self, ac_id: String) {
    if self.auto_ac && self.cfg.ac_id != ac_id {
      self.cfg.ac_id = ac_id;
    }
  }

  /// Build the "portal said no" message: reason + optional hint.
  fn rejected(&self, v: &serde_json::Value) -> String {
    format!(
      "login rejected (ac_id={}): {v}\n\
       hint: ac_id can change with the network; pass --ac-id to override",
      self.cfg.ac_id
    )
  }

  /// One full "make sure I am online" step.
  pub fn ensure_online(&mut self) -> Result<String, NetError> {
    self.state = State::Authenticating;
    self.failure = FailureKind::Other;
    if self.client.is_online(&self.cfg.ac_id) {
      self.state = State::LoggedIn;
      self.failures = 0;
      return Ok("already online".into());
    }
    let mut attempt = 0;
    loop {
      attempt += 1;
      // We are offline here, so a captive interception page is authoritative:
      // refresh the AC id on every reconnect (the network may have changed).
      if self.auto_ac {
        if let Ok((fresh, true)) = self.client.detect_ac_id() {
          if fresh != self.cfg.ac_id {
            let msg = format!("ac_id {} -> {fresh}", self.cfg.ac_id);
            self.warnings.push(Line::debug(msg));
            self.cfg.ac_id = fresh;
          }
        }
      }
      let v = match self.client.login_logout(
        &self.cfg.username,
        Some(&self.cfg.password),
        &self.cfg.ip,
        &self.cfg.ac_id,
        false,
      ) {
        Ok(v) => v,
        Err(e) => {
          // The request never reached the portal, so the state is unknown, not
          // "authenticating". Leaving it there kept the window and the tray
          // amber until a later status poll happened to succeed — which on a
          // dead network never happens.
          self.state = State::LoggedOut;
          // Keep the cause: the window words a dead network differently from a
          // portal that answered with something unusable.
          return Err(e.context(format!("request to {} failed", self.cfg.host)));
        }
      };

      if v.get("error").and_then(|e| e.as_str()) == Some("ok") {
        self.state = State::LoggedIn;
        self.failures = 0;
        return Ok("login ok".into());
      }
      // Some deployments answer with a policy-level error even though the
      // session was created. Credential errors never resolve.
      let code = ecode(&v);
      if crate::srun::is_credentials_error(&code) {
        self.failure = FailureKind::Credentials;
        self.state = State::LoggedOut;
        return Err(NetError::msg(self.rejected(&v)));
      }
      // Errors that are about the AC id (or that carry no code at all) may
      // still have created the session, so they are the ones worth re-trying
      // with a fresh id and then polling for. Everything else is final.
      let policy_like = code.starts_with("E28") || code == "no ecode" || code.is_empty();
      if !policy_like {
        self.state = State::LoggedOut;
        return Err(NetError::msg(self.rejected(&v)));
      }
      // The AC id changes with the network; re-detect once.
      if attempt == 1 {
        if let Ok((fresh, true)) = self.client.detect_ac_id() {
          if fresh != self.cfg.ac_id {
            let msg = format!("ac_id changed {} -> {fresh}, retrying", self.cfg.ac_id);
            self.warnings.push(Line::debug(msg));
            self.cfg.ac_id = fresh;
            continue;
          }
        }
      }
      if self
        .client
        .wait_online(&self.cfg.ac_id, Duration::from_secs(20))
      {
        self.state = State::LoggedIn;
        self.failures = 0;
        return Ok(format!(
          "login ok (server reported {code}, verified online)"
        ));
      }
      self.state = State::LoggedOut;
      // Both of these say the same thing: the portal could not hand the login
      // to an access controller for the id this client has, which on THU is
      // what a stale id from another link looks like. The raw JSON in the
      // banner would only hide that, so it gets its own kind.
      if crate::srun::is_ac_unreachable(&code, &error_msg(&v)) {
        self.failure = FailureKind::AcUnreachable;
        return Err(NetError::msg(format!(
          "the portal could not reach the access controller for ac_id={}: {v}",
          self.cfg.ac_id
        )));
      }
      return Err(NetError::msg(self.rejected(&v)));
    }
  }

  pub fn logout(&mut self) -> Result<String, NetError> {
    // The portal knows which account owns the session on this address, and that
    // account wins: the saved one may belong to somebody else (shared machine)
    // or be missing entirely. `get_challenge` also rejects an empty username.
    let mut ip = self.cfg.ip.trim().to_string();
    let mut username = String::new();
    if let Some((user, addr)) = self.client.online_identity(&ip) {
      username = user;
      if ip.is_empty() {
        ip = addr;
      }
    }
    let saved = self.cfg.username.trim().to_string();
    if username.is_empty() {
      username = saved;
    } else if !saved.is_empty() && saved != username {
      self.warnings.push(Line::warn(format!(
        "logout: the portal reports {username}, but {saved} is saved; logging out {username}"
      )));
    }
    if username.is_empty() {
      return Err(NetError::msg(
        "no account to log out: none is saved and the portal reports no session",
      ));
    }
    let v = self
      .client
      .login_logout(&username, None, &ip, &self.cfg.ac_id, true)?;
    if v.get("error").and_then(|e| e.as_str()) != Some("ok") {
      // Do not put the whole portal answer in the log: it echoes client_ip and
      // online_ip, which the privacy toggle does not hide.
      let reason = v
        .get("error_msg")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .or_else(|| v.get("error").and_then(|e| e.as_str()))
        .unwrap_or("portal rejected the logout");
      return Err(NetError::msg(format!("logout rejected: {reason}")));
    }
    self.state = State::LoggedOut;
    Ok("ok".to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn named(username: &str, password: &str) -> CoreConfig {
    let mut cfg = CoreConfig::default();
    cfg.username = username.into();
    cfg.password = password.into();
    cfg
  }

  /// An account without a password cannot sign in, so the session does not
  /// keep it: the pair is one credential, and a half-set one must not reach
  /// `credential_state` or `--remember-password off`.
  #[test]
  fn a_name_without_a_password_is_dropped() {
    let session = Session::new(named("alice", ""));
    assert!(session.cfg().username.is_empty());
    assert!(session.cfg().password.is_empty());
  }

  #[test]
  fn a_complete_pair_is_kept() {
    let session = Session::new(named("alice", "pw"));
    assert_eq!(session.cfg().username, "alice");
    assert_eq!(session.cfg().password, "pw");
  }

  /// The same rule when a frontend sets the pair: an empty password takes the
  /// name with it instead of leaving half a credential behind.
  #[test]
  fn clearing_the_password_clears_the_name() {
    let mut session = Session::new(named("alice", "pw"));
    session.set_credentials("alice".into(), String::new());
    assert!(session.cfg().username.is_empty());
    assert!(session.cfg().password.is_empty());
  }
}
