//! Wire protocol between the background core and the window/tray frontend.
//!
//! The two processes are the same executable; they talk over a machine-wide
//! local socket (a named pipe on Windows, an abstract Unix socket on Linux; a
//! per-user endpoint in portable mode), one JSON document per line:
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
///
/// While the app is pre-release the window and the service are always installed
/// from the same build, so this stays at 1 and the shapes may change freely.
/// Raise it the first time an old core can meet a new frontend: a released
/// build, or an upgrade that cannot restart both halves together.
pub const VERSION: u8 = 1;

/// What the frontend asks the core to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cmd {
  Connect,
  Disconnect,
  /// Re-read the portal and publish state/traffic.
  Refresh {
    /// A refresh the window started on its own (it just came to the front)
    /// exists to make the page current, so it neither logs nor toasts.
    #[serde(default)]
    quiet: bool,
  },
  /// Language for the core's own user-facing strings.
  SetLang(Lang),
  SetCredentials {
    username: String,
    password: String,
    /// Persist the pair in the core's state directory. `false` keeps it in
    /// memory only and removes any stored entry.
    #[serde(default)]
    remember: bool,
    /// Seal the password the core already holds, instead of one in `password`.
    ///
    /// A frontend never keeps the plaintext, so ticking *Remember password*
    /// after an Apply that did not save it leaves the field empty; the only
    /// pair left to seal is the one currently held. The core accepts that only
    /// while `username` still names the account the held password was proven
    /// for. `#[serde(default)]` keeps an older core (which ignores the field)
    /// parsing this message.
    #[serde(default)]
    keep_password: bool,
  },
  /// Sign in by itself right after the core starts (persisted by the core).
  /// A runtime preference only: it never changes whether the core tries to be
  /// online now.
  SetAutoConnect(bool),
  /// Keep a session alive after it was established: sign back in when it is
  /// lost behind the user's back, until the user disconnects (persisted by
  /// the core).
  SetKeepOnline(bool),
  /// Ask what credential the core holds without asking for the password
  /// itself. When `username`/`password` are supplied, the answer also says
  /// whether they are the stored pair, so a frontend can clear an *Unsaved*
  /// marker without ever seeing the plaintext.
  CheckCredentials {
    username: String,
    password: String,
  },
  /// Ask for one status dump (the CLI `status` command).
  Status,
  /// Ask for the core's own status only (no portal query).
  CoreInfo,
  /// Report the access-controller id the core is currently using.
  AcId,
  /// Ask a portable core to exit. A machine core refuses: stopping the service
  /// is the service manager's job, not a frontend's.
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

/// Which physical link carries the machine's default route.
///
/// A machine can have a cable and Wi-Fi up at the same time, and the portal
/// session belongs to one of them: knowing which link the OS actually sends on
/// is what makes an "online" verdict readable, and it is the first thing to
/// check when a session appears to come and go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkKind {
  /// No route at all: no cable, no Wi-Fi, or no address yet.
  #[default]
  None,
  /// Wired Ethernet.
  Ethernet,
  /// Wi-Fi.
  Wlan,
  /// Anything else: a tunnel, a virtual switch, a modem.
  Other,
}

/// The core's own state, as reported by [`Cmd::CoreInfo`].
///
/// The same value is embedded in the `core` half of an [`Ev::Status`] dump, so
/// the CLI and the window describe the service with one set of fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfo {
  pub version: String,
  /// Build stamp of the core (see `build.rs`), seconds since the epoch.
  ///
  /// `#[serde(default)]` lets a new frontend read an old core, where the field
  /// is absent (it then reads as 0 = unknown). Additive, so [`VERSION`] stays
  /// put.
  #[serde(default)]
  pub build_epoch: u64,
  pub pid: u32,
  pub uptime_secs: u64,
  pub state: State,
  pub env: LoginEnv,
  pub ac_id: String,
  pub host: String,
  /// The link the core's requests would leave on. Absent when the core is old
  /// enough not to know the field, which reads as "unknown".
  #[serde(default)]
  pub link: LinkKind,
  /// What the core would log in with. The password itself never leaves it.
  #[serde(default)]
  pub credential: CredentialState,
  /// The core's startup preference, as stored on the machine.
  pub auto_connect: bool,
  /// The core's stay-connected preference. A core built before this field
  /// existed reads as `false` here; the window then keeps showing its own
  /// stored value instead.
  #[serde(default)]
  pub keep_online: bool,
  /// Whether the core wants to be online right now. This is the runtime
  /// intent behind every automatic login; it is true after a start with
  /// auto connect on, and after a manual connect.
  pub wanted_online: bool,
  pub failures: u32,
  pub frontends: usize,
  pub endpoint: String,
}

/// What the core holds, without the password itself.
///
/// A machine core is the only holder of the sealed credential, so the window
/// has to be able to tell "saved" from "not saved" (and whether the text in
/// the field is the stored value) without ever receiving the plaintext. Every
/// field here is something the core can answer without revealing the secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialState {
  /// Account the core would log in with.
  pub username: String,
  /// A password is in memory, so a login can be attempted right now.
  pub has_password: bool,
  /// A sealed copy is on disk, so a restarted core keeps it.
  pub remembered: bool,
  /// Answer to [`Cmd::CheckCredentials`]: whether the candidate equals the
  /// stored pair. `None` for an unsolicited state report.
  #[serde(default)]
  pub matches: Option<bool>,
}

/// Why the red banner is up.
///
/// The frontend turns these into words when it paints, so a banner that is
/// already on screen is re-worded by a language switch; the core only reports
/// which case happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
  /// The portal rejected the account/password pair, which only the user can
  /// fix. The raw JSON answer goes to the log pane instead.
  Credentials,
  /// The portal could not reach an access controller for the `ac_id` the core
  /// holds, which on THU means the id belongs to another link. Retrying it on
  /// the same link cannot work, so automatic attempts stop until the user
  /// connects again.
  AcUnreachable,
  /// The request never reached the portal. Only the kind travels: the OS text
  /// is localized by Windows, so the window words this itself.
  Network(crate::srun::NetCause),
  /// Text the core already worded (a disk or service error).
  Text(String),
}

impl std::fmt::Display for Failure {
  /// The CLI prints one line, so it takes the words of the OS language; the
  /// window words its banner itself instead, to follow its own setting.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let t = crate::i18n::Lang::system().texts();
    f.write_str(match self {
      Failure::Credentials => t.invalid_credentials,
      Failure::AcUnreachable => t.ac_unreachable,
      Failure::Network(cause) => t.net_cause(*cause),
      Failure::Text(s) => s,
    })
  }
}

/// What the core tells the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Ev {
  State(State),
  Info(Value),
  Log(Line),
  /// `Some` = a failure to surface; `None` = clear the banner.
  Error(Option<Failure>),
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
  /// Answer to [`Cmd::CheckCredentials`] and to every [`Cmd::SetCredentials`].
  Credentials(CredentialState),
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
      remember: true,
      keep_password: true,
    });
    let text = wire.line().unwrap();
    assert!(!text.contains('\n') || text.ends_with('\n'));
    assert!(text.starts_with("{\"v\":1,"), "{text}");
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Cmd(Cmd::SetCredentials {
        username,
        password,
        remember,
        keep_password,
      }) => {
        assert_eq!(username, "alice");
        assert_eq!(password, "secret");
        assert!(remember);
        assert!(keep_password);
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }

  /// A build that predates `keep_password` must still parse this message: the
  /// field is optional on the wire, and its absence means "no".
  #[test]
  fn a_command_without_keep_password_still_parses() {
    let text = r#"{"v":1,"msg":{"t":"cmd","m":{"SetCredentials":{"username":"alice","password":"secret","remember":true}}}}"#;
    match Wire::parse(text).unwrap().msg {
      Msg::Cmd(Cmd::SetCredentials {
        username,
        keep_password,
        ..
      }) => {
        assert_eq!(username, "alice");
        assert!(!keep_password, "an absent flag means a typed password");
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }

  #[test]
  fn events_roundtrip_including_the_network_verdict() {
    for ev in [
      Ev::State(State::Online),
      Ev::Env(LoginEnv::EnterpriseWifi("Tsinghua-Secure".into())),
      Ev::Env(LoginEnv::NoPortal),
      Ev::Error(None),
      Ev::Error(Some(Failure::Credentials)),
      Ev::Error(Some(Failure::Network(crate::srun::NetCause::Resolve))),
      Ev::Error(Some(Failure::Text("boom".into()))),
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
  fn a_credential_rejection_keeps_its_kind_on_the_wire() {
    // The wording belongs to the frontend, so the wire only carries the case;
    // a `Text` here would freeze the core's language into the banner.
    let text = Wire::ev(Ev::Error(Some(Failure::Credentials)))
      .line()
      .unwrap();
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Ev(Ev::Error(Some(Failure::Credentials))) => {}
      other => panic!("wrong variant: {other:?}"),
    }
  }

  #[test]
  fn a_foreign_version_is_still_parsed_so_the_core_can_refuse_it() {
    let text = r#"{"v":99,"msg":{"t":"cmd","m":{"Refresh":{"quiet":false}}}}"#;
    let wire = Wire::parse(text).unwrap();
    assert_eq!(wire.v, 99);
  }

  #[test]
  fn the_core_status_roundtrips() {
    let info = CoreInfo {
      version: "0.1.3".into(),
      build_epoch: 1_700_000_000,
      pid: 42,
      uptime_secs: 7,
      state: State::Online,
      env: LoginEnv::Campus,
      ac_id: "169".into(),
      host: "auth4.tsinghua.edu.cn".into(),
      link: LinkKind::Wlan,
      credential: CredentialState {
        username: "alice".into(),
        has_password: true,
        remembered: true,
        matches: None,
      },
      auto_connect: true,
      keep_online: true,
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

  #[test]
  fn a_core_that_does_not_report_the_link_still_parses() {
    // An updated window has to keep working against a service built before
    // this field existed; the row then reads as "no link" rather than the
    // whole answer failing to parse.
    let old = r#"{"version":"0.1.3","pid":1,"uptime_secs":2,"state":"Offline",
      "env":"Campus","ac_id":"1","host":"auth4.tsinghua.edu.cn",
      "credential":{"username":"alice","has_password":false,"remembered":false},
      "auto_connect":false,"wanted_online":false,"failures":0,"frontends":0,
      "endpoint":"x"}"#;
    let info: CoreInfo = serde_json::from_str(old).unwrap();
    assert_eq!(info.link, LinkKind::None);
    assert!(!info.keep_online, "an old core never reported it");
    assert_eq!(info.version, "0.1.3");
  }

  #[test]
  fn a_credential_answer_names_no_secret() {
    let state = CredentialState {
      username: "alice".into(),
      has_password: true,
      remembered: true,
      matches: Some(true),
    };
    let text = Wire::ev(Ev::Credentials(state)).line().unwrap();
    assert!(!text.contains("hunter2"), "{text}");
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Ev(Ev::Credentials(back)) => {
        assert!(back.has_password && back.remembered);
        assert_eq!(back.matches, Some(true));
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }
}
