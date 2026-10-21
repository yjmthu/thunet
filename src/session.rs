//! Session state machine: ensure online → keepalive → re-login on failure.
//!
//! This is the "auto connect + keep online" behaviour requested from the
//! shipped client, minus its GUI. It is deliberately UI-free so the same code
//! runs headless on a router or inside the tray app.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::logline::Line;
use crate::srun::{Client, NetEnv};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
  Offline,
  Authenticating,
  Online,
}

/// Why the last login attempt failed, so the caller can decide how much of the
/// portal's answer to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureKind {
  #[default]
  Other,
  /// Wrong account or password: retrying cannot help, and the raw JSON is noise.
  Credentials,
}

pub struct Session {
  client: Client,
  cfg: Config,
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

#[allow(dead_code)] // some accessors are consumed only by the tray/UI layer
impl Session {
  pub fn new(mut cfg: Config) -> Self {
    let client = Client::new(&cfg.host, cfg.https, Duration::from_secs(5));
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
      state: State::Offline,
      failures: 0,
      failure: FailureKind::Other,
    }
  }

  /// Take the pending diagnostic lines (startup + re-detection).
  pub fn take_warnings(&mut self) -> Vec<Line> {
    std::mem::take(&mut self.warnings)
  }

  /// Update the credentials used by the next login attempt.
  pub fn set_credentials(&mut self, username: String, password: String) {
    self.cfg.username = username;
    self.cfg.password = password;
  }

  pub fn client(&self) -> &Client {
    &self.client
  }

  pub fn cfg(&self) -> &Config {
    &self.cfg
  }

  /// Query the portal and sync the local state with reality.
  pub fn refresh(&mut self) -> Result<serde_json::Value, String> {
    let v = self.client.status(&self.cfg.ac_id)?;
    let online = v.get("online").and_then(|b| b.as_bool()).unwrap_or(false);
    self.state = if online {
      State::Online
    } else {
      State::Offline
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
  pub fn ensure_online(&mut self) -> Result<String, String> {
    self.state = State::Authenticating;
    self.failure = FailureKind::Other;
    if self.client.is_online(&self.cfg.ac_id) {
      self.state = State::Online;
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
          self.state = State::Offline;
          return Err(format!("request to {} failed: {e}", self.cfg.host));
        }
      };

      if v.get("error").and_then(|e| e.as_str()) == Some("ok") {
        self.state = State::Online;
        self.failures = 0;
        return Ok("login ok".into());
      }
      // Some deployments answer with a policy-level error even though the
      // session was created. Credential errors never resolve.
      let code = ecode(&v);
      if crate::srun::is_credentials_error(&code) {
        self.failure = FailureKind::Credentials;
        self.state = State::Offline;
        return Err(self.rejected(&v));
      }
      let policy_like = code.starts_with("E28") || code == "no ecode" || code.is_empty();
      if !policy_like {
        self.state = State::Offline;
        return Err(self.rejected(&v));
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
        self.state = State::Online;
        self.failures = 0;
        return Ok(format!(
          "login ok (server reported {code}, verified online)"
        ));
      }
      self.state = State::Offline;
      return Err(self.rejected(&v));
    }
  }

  /// Touch the keepalive target; failure to reach the internet implies the
  /// session is gone, so the caller should re-authenticate.
  pub fn keepalive(&mut self) -> Result<(), String> {
    match self.client.touch(&self.cfg.keepalive_target) {
      Ok(_) => {
        self.failures = 0;
        Ok(())
      }
      Err(e) => {
        self.failures += 1;
        Err(e)
      }
    }
  }

  pub fn logout(&mut self) -> Result<String, String> {
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
      return Err("no account to log out: none is saved and the portal reports no session".into());
    }
    let v = self
      .client
      .login_logout(&username, None, &ip, &self.cfg.ac_id, true)?;
    if v.get("error").and_then(|e| e.as_str()) != Some("ok") {
      return Err(format!("logout rejected: {v}"));
    }
    self.state = State::Offline;
    Ok(format!("{v}"))
  }
}
