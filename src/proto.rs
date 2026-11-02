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
/// from the same build, so this stays low and the shapes may change freely.
/// Raise it the first time an old core can meet a new frontend: a released
/// build, or an upgrade that cannot restart both halves together.
///
/// 2: [`Ev::Beat`] was added, and the window no longer takes a status answer as
/// the heartbeat.
/// 3: [`Ev::Env`] carries [`LinkEnv`] instead of the old flat verdict, and the
/// state variants travel under the names they have in code (`LoggedOut` where
/// the wire used to say `Offline`).
/// 4: [`Ev::Link`] was added: the core publishes the physical link and its
/// addresses instead of the window reading the interface itself.
pub const VERSION: u8 = 4;

/// What the frontend asks the core to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cmd {
  Connect,
  Disconnect,
  /// Re-read the portal and publish state/traffic.
  Refresh {
    /// A refresh the window started as a consequence of its own edit (the
    /// stored pair is asked about again after a refused one is taken back),
    /// not a click on *Refresh*: it neither logs nor toasts.
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

/// Whether the campus portal is behind this link, and who owns the session.
///
/// The probe answers three questions separately, and the model keeps them
/// separate because every rule downstream decomposes into them:
///
/// * [`LinkEnv::internet`] - the plain probes reached the internet without being
///   intercepted. This is what tells "the gateway is holding me back" (log in)
///   from "this link simply routes" (nothing to do).
/// * [`LinkEnv::portal`] - the campus portal is here, someone else's portal is,
///   or ours stayed silent.
/// * [`LinkEnv::os_session`] - the portal says the OS opened the session on this
///   address (802.1X), so it is not the portal's and its verbs do not apply.
///   The evidence is the answer's `domain` field, not the Wi-Fi name: a phone
///   sharing its own 802.1X session to this machine carries that session on an
///   address whose local Wi-Fi name says nothing about it.
///
/// [`State`] is deliberately *not* in here: whether a session exists is the
/// portal's answer, and it changes on a different schedule than the link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEnv {
  /// The machine has a route at all: a cable, a Wi-Fi association, a phone's
  /// link. False means there is nothing to ask and nothing to log in on.
  pub connected: bool,
  /// The plain-HTTP probes reached the internet directly: nothing intercepted
  /// them. False on a gateway that drops pre-login traffic, and on one that
  /// answers with another operator's login page.
  pub internet: bool,
  pub portal: Portal,
  /// The service the portal names for the session on this address, when that
  /// service is the OS's own (802.1X). A login or a logout here would act on
  /// something this app never opened.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub os_session: Option<String>,
}

/// What is behind the path to the internet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Portal {
  /// The campus portal answered about this link: its gateway intercepted a
  /// probe (`ac_id` names the access controller), or `/cgi-bin/rad_user_info`
  /// answered. `ac_id` is `None` when nothing intercepted, which is also when a
  /// login has no access controller to be addressed to.
  Ours { ac_id: Option<String> },
  /// A captive portal that is not the campus one (hotel, airport, ...).
  Foreign,
  /// The campus portal did not answer on this link. `internet` says whether
  /// anything else did.
  Silent,
}

impl LinkEnv {
  /// The verdict before any core has said anything, and again once the one that
  /// said it is gone.
  pub fn empty() -> Self {
    Self {
      connected: false,
      internet: false,
      portal: Portal::Silent,
      os_session: None,
    }
  }

  /// A link where a failed request to the campus portal is worth a red band.
  ///
  /// Only where the portal is ours: on any other link the band would repeat, in
  /// another tone, what the verdict band already says.
  pub fn portal_answers(&self) -> bool {
    matches!(self.portal, Portal::Ours { .. })
  }

  /// The OS owns the session here (802.1X), so the portal verbs are not ours to
  /// use - and neither is the account: a login would prove nothing.
  pub fn os_owns_session(&self) -> bool {
    self.os_session.is_some()
  }

  /// Is a login both meaningful and addressable here?
  ///
  /// Meaningful because the gateway is holding traffic back (`!internet`): on a
  /// link that already routes, a login would only authenticate an address the
  /// portal can see for reasons of its own - behind a NAT, a VPN gateway's
  /// egress - for everybody behind it. Addressable because the portal is ours
  /// and is not the OS's business.
  pub fn login_possible(&self) -> bool {
    self.portal_answers() && !self.os_owns_session() && !self.internet
  }

  /// May a logout be sent? The session must be one the portal owns, and one
  /// must exist (or be in flight, which is what *Disconnect* cancels).
  pub fn logout_possible(&self, state: State) -> bool {
    self.portal_answers()
      && !self.os_owns_session()
      && matches!(state, State::LoggedIn | State::LoggingIn)
  }

  /// Is a status poll of the portal worth making?
  ///
  /// Only where the portal can answer - a poll elsewhere is a guaranteed
  /// timeout whose answer is already known - with one exception: while a session
  /// is up the poll is the only thing watching the link, because the timer that
  /// re-probes the network stays off then.
  pub fn polls_portal(&self, state: State) -> bool {
    self.portal_answers() || state == State::LoggedIn
  }

  /// Does the core's account form apply on this link? It does wherever a login
  /// is possible, and also where one is merely pointless (a home network still
  /// keeps a saved credential for the next campus link). It does not where the
  /// session is the OS's.
  pub fn account_form_applies(&self) -> bool {
    !self.os_owns_session()
  }
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
  /// A phone's cellular link: the phone itself, or a computer whose route goes
  /// through one (a tethered phone). Named on its own because "Other" says
  /// nothing about the one link a campus client most often falls back to.
  Mobile,
  /// Wired Ethernet provided by a phone over USB (USB tethering). It reaches
  /// the OS as an ordinary Ethernet adapter, so the platform has to say which
  /// one it is.
  Usb,
  /// Anything else: a tunnel, a virtual switch, a modem.
  Other,
}

/// The physical link the core is using, and the addresses the machine holds on
/// it.
///
/// The core publishes this from its own watch so the window never reads the
/// interface behind its back: a local read could move the Link row before the
/// account and address the core clears with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkState {
  pub kind: LinkKind,
  /// The IPv4 address that link carries, when it has one.
  #[serde(default)]
  pub ip4: Option<std::net::IpAddr>,
  /// The IPv6 address that link carries, when it has one.
  #[serde(default)]
  pub ip6: Option<std::net::IpAddr>,
}

/// The portal's account of the session this machine is on, as the status page
/// shows it.
///
/// The core builds this from the portal's answer (`srun::portal_rows`) instead
/// of handing the JSON on: which field names the caller, which placeholder means
/// "no address", which `domain` is which campus service - all of that is
/// protocol knowledge, and one copy of it lives next to the request that reads
/// it. It is also what makes "does this answer still describe this link"
/// answerable in one place: an answer that does not keeps its account and
/// traffic and loses its addresses.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalRows {
  /// The address the portal sees this machine at, per family. `None` when the
  /// portal named none, or when the answer described a link that is gone.
  #[serde(default)]
  pub ip4: Option<std::net::IpAddr>,
  #[serde(default)]
  pub ip6: Option<std::net::IpAddr>,
  #[serde(default)]
  pub bytes_in: u64,
  #[serde(default)]
  pub bytes_out: u64,
  /// The account the portal says owns the session on that address. Empty when
  /// there is no session: the portal's "nobody logged in" answer names none.
  #[serde(default)]
  pub name: String,
  /// The service behind the session (the portal's `domain`), as it arrived.
  /// [`crate::srun::service_label`] turns it into the name the row shows.
  #[serde(default)]
  pub service: String,
  /// The account's online devices. Empty when the portal reports no detail
  /// (offline, or an answer without the field).
  #[serde(default)]
  pub devices: Vec<PortalDevice>,
  /// How many devices the account has online: the listed ones when there is a
  /// detail, otherwise the portal's own total.
  #[serde(default)]
  pub devices_total: usize,
}

/// One device the account has online, as `/cgi-bin/rad_user_info` lists it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalDevice {
  /// Portal session id. Only keeps the order stable inside a group.
  #[serde(default)]
  pub id: String,
  #[serde(default)]
  pub ip4: Option<std::net::IpAddr>,
  #[serde(default)]
  pub ip6: Option<std::net::IpAddr>,
  #[serde(default)]
  pub os: String,
  /// The portal's own device class, e.g. `Smartphones/PDAs/Tablets`.
  #[serde(default)]
  pub class: String,
  /// This is the machine the window runs on, judged by the session's own
  /// addresses: it is listed first and marked.
  #[serde(default)]
  pub local: bool,
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
  pub env: LinkEnv,
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
  /// Whether the core itself starts with the machine: the service's start type,
  /// read from the service manager. A portable core is a thread of its window,
  /// so it always answers `false`. Absent on an old core (`false`).
  #[serde(default)]
  pub autostart: bool,
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
  /// What the core says about the machine on this link, in the shape the status
  /// page draws: the address, the account, the traffic, the devices. One
  /// snapshot written by one place, so a row cannot disagree with another (and
  /// an address cannot outlive the session that named it). Replayed to a
  /// frontend that attaches late, after the link it belongs to.
  Info(PortalRows),
  /// The physical link the core is using and the addresses on it. Sent whenever
  /// that changes, and replayed to a frontend that attaches late. The window
  /// draws the Link row from this and never reads the interface itself; the
  /// addresses in it are what the core's own decisions are made from, and the
  /// IP rows are drawn from [`Ev::Info`] instead, so the two are published
  /// together and cannot contradict each other.
  Link(LinkState),
  /// One status poll finished, whether or not the portal answered. The window
  /// walks the turtle from this beat; [`Ev::Info`] carries what the answer
  /// said.
  Beat,
  Log(Line),
  /// `Some` = a failure to surface; `None` = clear the banner.
  Error(Option<Failure>),
  /// Transient bottom-strip feedback for an action's outcome.
  Notify {
    msg: String,
    kind: ToastKind,
  },
  /// What the core's network probe last saw; drives *Connect* availability.
  Env(LinkEnv),
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
    assert!(text.starts_with(&format!("{{\"v\":{VERSION},")), "{text}");
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
      Ev::State(State::LoggedIn),
      Ev::Env(LinkEnv {
        connected: true,
        internet: true,
        portal: Portal::Silent,
        os_session: None,
      }),
      Ev::Env(LinkEnv {
        connected: true,
        internet: false,
        portal: Portal::Ours {
          ac_id: Some("169".into()),
        },
        os_session: Some("Tsinghua-Secure".into()),
      }),
      Ev::Link(LinkState {
        kind: LinkKind::Wlan,
        ip4: Some("192.0.2.10".parse().unwrap()),
        ip6: Some("2001:db8::7".parse().unwrap()),
      }),
      Ev::Link(LinkState {
        kind: LinkKind::None,
        ip4: None,
        ip6: None,
      }),
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
      state: State::LoggedIn,
      env: LinkEnv {
        connected: true,
        internet: true,
        portal: Portal::Ours { ac_id: None },
        os_session: None,
      },
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
      autostart: true,
      failures: 0,
      frontends: 1,
      endpoint: "/run/user/1000/thunet/control.sock".into(),
    };
    let text = Wire::ev(Ev::Core(info)).line().unwrap();
    match Wire::parse(text.trim_end()).unwrap().msg {
      Msg::Ev(Ev::Core(back)) => {
        assert_eq!(back.version, "0.1.3");
        assert_eq!(back.ac_id, "169");
        assert_eq!(back.state, State::LoggedIn);
        assert_eq!(back.frontends, 1);
      }
      other => panic!("wrong variant: {other:?}"),
    }
  }

  #[test]
  fn a_core_that_does_not_report_the_link_still_parses() {
    // An updated window has to keep working against a service built before
    // these fields existed; the row then reads as "no link" rather than the
    // whole answer failing to parse.
    let old = r#"{"version":"0.1.3","pid":1,"uptime_secs":2,"state":"LoggedOut",
      "env":{"connected":true,"internet":false,"portal":{"Ours":{"ac_id":"1"}}},
      "ac_id":"1","host":"auth4.tsinghua.edu.cn",
      "credential":{"username":"alice","has_password":false,"remembered":false},
      "auto_connect":false,"wanted_online":false,"failures":0,"frontends":0,
      "endpoint":"x"}"#;
    let info: CoreInfo = serde_json::from_str(old).unwrap();
    assert_eq!(info.link, LinkKind::None);
    assert_eq!(info.state, State::LoggedOut);
    assert!(info.env.portal_answers());
    assert_eq!(
      info.env.os_session, None,
      "an old core never reported 802.1X"
    );
    assert!(!info.keep_online, "an old core never reported it");
    assert!(!info.autostart, "an old core never reported it");
    assert_eq!(info.version, "0.1.3");
  }

  /// The verdict model in one table. Every rule downstream - the verbs, the
  /// poll, the band - reads these answers instead of re-deriving them, which is
  /// what kept four slightly different ``what does this link allow`` matches in
  /// the tree.
  #[test]
  fn the_link_env_answers_what_this_link_allows() {
    let ours = |internet: bool| LinkEnv {
      connected: true,
      internet,
      portal: Portal::Ours { ac_id: None },
      os_session: None,
    };
    let silent = |internet: bool| LinkEnv {
      connected: true,
      internet,
      portal: Portal::Silent,
      os_session: None,
    };
    let foreign = LinkEnv {
      connected: true,
      internet: false,
      portal: Portal::Foreign,
      os_session: None,
    };
    let secure = LinkEnv {
      connected: true,
      internet: true,
      portal: Portal::Ours { ac_id: None },
      os_session: Some("Tsinghua-Secure".into()),
    };

    // A gateway holding traffic back: a login is exactly what fixes it, and a
    // session there is one the portal owns.
    let blocked = ours(false);
    assert!(blocked.login_possible());
    assert!(blocked.logout_possible(State::LoggedIn));
    assert!(blocked.polls_portal(State::LoggedOut));
    assert!(blocked.portal_answers());
    assert!(blocked.account_form_applies());

    // The portal answers but the link routes anyway (a VPN's egress, a router
    // that is already signed in): a login would authenticate that shared
    // address for everybody behind it.
    let routed = ours(true);
    assert!(!routed.login_possible());
    assert!(routed.polls_portal(State::LoggedOut));
    assert!(routed.portal_answers());

    // Nothing of ours answered: no verbs, no poll, and a failed request is not
    // news - the verdict band already says it.
    for env in [silent(true), silent(false), foreign] {
      assert!(!env.login_possible(), "{env:?}");
      assert!(!env.logout_possible(State::LoggedIn), "{env:?}");
      assert!(!env.polls_portal(State::LoggedOut), "{env:?}");
      assert!(!env.portal_answers(), "{env:?}");
    }

    // A session that is up keeps its poll even where the portal is not ours:
    // while online the network probe timer is off, so the poll is the only
    // thing that would notice the machine moved.
    assert!(silent(true).polls_portal(State::LoggedIn));

    // 802.1X: the OS owns the session, so none of the portal verbs are ours,
    // and neither is the account form. The poll still runs - the portal row is
    // the only place the account and the traffic numbers come from.
    assert!(!secure.login_possible());
    assert!(!secure.logout_possible(State::LoggedIn));
    assert!(!secure.account_form_applies());
    assert!(secure.polls_portal(State::LoggedOut));
  }

  /// The state now travels under its own name: `Offline` was a word the core
  /// never meant (it says "the portal reports no session", not "no internet").
  #[test]
  fn the_state_variants_round_trip_by_their_own_names() {
    for (state, wire) in [
      (State::Unknown, "Unknown"),
      (State::LoggedOut, "LoggedOut"),
      (State::LoggingIn, "LoggingIn"),
      (State::LoggedIn, "LoggedIn"),
    ] {
      let text = Wire::ev(Ev::State(state)).line().unwrap();
      assert!(text.contains(&format!("\"State\":\"{wire}\"")), "{text}");
      match Wire::parse(text.trim_end()).unwrap().msg {
        Msg::Ev(Ev::State(back)) => assert_eq!(back, state),
        other => panic!("wrong variant: {other:?}"),
      }
    }
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
