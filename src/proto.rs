//! Wire protocol between the background core and the window/tray frontend.
//!
//! The two processes are the same executable; they talk over a per-user local
//! socket (a Unix domain socket, or a named pipe on Windows) one JSON document
//! per line:
//!
//! ```text
//! {"v":1,"msg":{"t":"cmd","m":"Connect"}}
//! {"v":1,"msg":{"t":"ev","m":{"State":"Online"}}}
//! ```
//!
//! `v` is the protocol version. A frontend built against a different version is
//! refused by the core with a log line instead of being mis-parsed, which is
//! what makes it safe to upgrade the two halves independently.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::i18n::Lang;
use crate::logline::Line;
use crate::session::State;

/// Bumped whenever a [`Cmd`] or [`Ev`] variant changes shape or meaning.
pub const VERSION: u8 = 1;

/// What the frontend asks the core to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cmd {
  Connect,
  Disconnect,
  Refresh,
  /// Language for the core's own user-facing strings.
  SetLang(Lang),
  SetCredentials {
    username: String,
    password: String,
  },
  SetAutoConnect(bool),
  /// Ask for one status dump (the CLI `status` command).
  Status,
  /// Ask for the core's own status only (no portal query).
  CoreInfo,
  /// Report the access-controller id the core is currently using.
  AcId,
  /// The frontend is going away. The core keeps running: it owns the session.
  Quit,
  /// The frontend wants the core to stop: the status page's *Stop* button was
  /// used (tray *Quit* deliberately only ends the window). The core ends the
  /// session itself instead of lingering unseen.
  ///
  /// Additive on purpose, so [`VERSION`] stays put: a core built before this
  /// variant existed skips the line it cannot parse, which is exactly the old
  /// "keep running" behaviour.
  Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToastKind {
  Ok,
  Err,
  Info,
}

/// Where the machine sits, as far as logging in is concerned. The core probes
/// this and the UI greys *Connect* out when an attempt could not possibly work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoginEnv {
  /// A campus portal answered: logging in makes sense.
  Campus,
  /// The OS authenticates this Wi-Fi at layer 2 (802.1X); no portal involved.
  EnterpriseWifi(String),
  /// Nothing answered: no cable, no Wi-Fi, no route, DNS broken.
  NoNetwork,
  /// A captive portal that is not the campus one (hotel, airport, ...).
  OtherPortal,
  /// The internet works directly, so there is nothing to log in to.
  NoPortal,
}

/// The core's own state, as reported by [`Cmd::CoreInfo`].
///
/// The same value is embedded in the `core` half of an [`Ev::Status`] dump, so
/// the CLI and the window describe the service with one set of fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfo {
  pub version: String,
  pub pid: u32,
  pub uptime_secs: u64,
  pub state: State,
  pub env: LoginEnv,
  pub ac_id: String,
  pub host: String,
  pub auto_connect: bool,
  pub wanted_online: bool,
  pub failures: u32,
  pub frontends: usize,
  pub endpoint: String,
}

/// What the core tells the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Ev {
  State(State),
  Info(Value),
  Log(Line),
  /// `Some` = a failure to surface; `None` = clear the banner.
  Error(Option<String>),
  /// Transient bottom-strip feedback for an action's outcome.
  Notify {
    msg: String,
    kind: ToastKind,
  },
  /// What the core's network probe last saw; drives *Connect* availability.
  Env(LoginEnv),
  /// Answer to [`Cmd::Status`]: one object with the core's own state under
  /// `core` and the portal's session/traffic dump under `portal`.
  Status(Value),
  /// Answer to [`Cmd::CoreInfo`].
  Core(CoreInfo),
  /// Answer to [`Cmd::AcId`].
  AcId(String),
}

/// One line on the wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct Wire {
  pub v: u8,
  pub msg: Msg,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", content = "m", rename_all = "lowercase")]
pub enum Msg {
  Cmd(Cmd),
  Ev(Ev),
}

impl Wire {
  pub fn cmd(cmd: Cmd) -> Self {
    Self {
      v: VERSION,
      msg: Msg::Cmd(cmd),
    }
  }

  pub fn ev(ev: Ev) -> Self {
    Self {
      v: VERSION,
      msg: Msg::Ev(ev),
    }
  }

  /// One NDJSON line, newline included.
  pub fn line(&self) -> Result<String, String> {
    let mut s = serde_json::to_string(self).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
  }

  pub fn parse(text: &str) -> Result<Self, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn command_roundtrips_through_a_line() {
    let wire = Wire::cmd(Cmd::SetCredentials {
      username: "alice".into(),
      password: "secret".into(),
    });
    let text = wire.line().unwrap();
    assert!(!text.contains('\n') || text.ends_with('\n'));
    assert!(text.starts_with("{\"v\":1,"), "{text}");
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Cmd(Cmd::SetCredentials { username, password }) => {
        assert_eq!(username, "alice");
        assert_eq!(password, "secret");
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }

  #[test]
  fn shutdown_is_a_plain_command_older_cores_skip() {
    let text = Wire::cmd(Cmd::Shutdown).line().unwrap();
    assert_eq!(
      text.trim_end(),
      r#"{"v":1,"msg":{"t":"cmd","m":"Shutdown"}}"#
    );
    assert!(matches!(
      Wire::parse(text.trim_end()).unwrap().msg,
      Msg::Cmd(Cmd::Shutdown)
    ));
  }

  #[test]
  fn events_roundtrip_including_the_network_verdict() {
    for ev in [
      Ev::State(State::Online),
      Ev::Env(LoginEnv::EnterpriseWifi("Tsinghua-Secure".into())),
      Ev::Env(LoginEnv::NoPortal),
      Ev::Error(None),
      Ev::Error(Some("boom".into())),
      Ev::Log(Line::warn("careful")),
      Ev::Notify {
        msg: "ok".into(),
        kind: ToastKind::Ok,
      },
    ] {
      let text = Wire::ev(ev).line().unwrap();
      let back = Wire::parse(text.trim_end()).unwrap();
      assert_eq!(back.v, VERSION);
      assert!(matches!(back.msg, Msg::Ev(_)), "{text}");
    }
  }

  #[test]
  fn a_foreign_version_is_still_parsed_so_the_core_can_refuse_it() {
    let text = r#"{"v":99,"msg":{"t":"cmd","m":"Refresh"}}"#;
    let wire = Wire::parse(text).unwrap();
    assert_eq!(wire.v, 99);
  }

  #[test]
  fn the_core_status_roundtrips() {
    let info = CoreInfo {
      version: "0.1.3".into(),
      pid: 42,
      uptime_secs: 7,
      state: State::Online,
      env: LoginEnv::Campus,
      ac_id: "169".into(),
      host: "auth4.tsinghua.edu.cn".into(),
      auto_connect: true,
      wanted_online: true,
      failures: 0,
      frontends: 1,
      endpoint: "/run/user/1000/thunet/control.sock".into(),
    };
    let text = Wire::ev(Ev::Core(info)).line().unwrap();
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Ev(Ev::Core(back)) => {
        assert_eq!(back.version, "0.1.3");
        assert_eq!(back.ac_id, "169");
        assert_eq!(back.state, State::Online);
        assert_eq!(back.frontends, 1);
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }
}
