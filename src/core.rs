//! The background core: session state machine, network probing, and the event
//! stream the frontend renders.
//!
//! This is the loop that used to live inside the window process. It talks to
//! the outside world through [`crate::ipc`]: commands arrive over a channel,
//! every event is published to whichever frontends are attached. It never
//! touches a window, a tray icon or a display server, so the same code runs at
//! boot (`thunet core` from a systemd system unit or the Windows service) with
//! nobody logged in.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::config::CoreConfig;
use crate::i18n::Lang;
use crate::ipc::Sink;
use crate::logline::Line;
use crate::proto::{Cmd, Ev, Failure, LinkEnv, LinkKind, LinkState, Portal, PortalRows, ToastKind};
use crate::session::{Session, State};
use crate::srun::{NetCause, NetError, Probe};

/// The status poll the window draws as a heartbeat: every 15 s while the
/// session is up, every 10 s while it is not.
///
/// This is the only timer left in the core, and it is not a guess: on a campus
/// link that poll *is* the keepalive (see `docs/protocol.md` §9), and it is what
/// answers whether a session exists. The network verdict is never polled: it is
/// re-derived when the link watch sees a change (below), when the user asks, or
/// when the window comes to the front.
pub(crate) const POLL_ONLINE: Duration = Duration::from_secs(15);
pub(crate) const POLL_OFFLINE: Duration = Duration::from_secs(10);

/// How many status polls in a row may fail before "online" stops being
/// claimed. One is a hiccup the next poll recovers from; two is ~30 s without
/// an answer, which is not a session the window should keep showing as up.
const STATUS_FAILURES: u32 = 2;

/// How often the same unanswered status poll is logged again, in wall time.
/// The failure repeats verbatim for as long as the machine sits on a link
/// where the portal is unreachable: log the first one, then a reminder this
/// often, so a long outage leaves a breadcrumb instead of owning the file
/// (159 identical lines were 57% of one session's log).
const STATUS_LOG_REPEAT: Duration = Duration::from_secs(600);

/// How long a route change has to hold before it counts while a session is up.
/// A DHCP renew or a Wi-Fi roam flips the local address for about a second;
/// acting on that took the window offline and started a fresh login for
/// nothing.
///
/// The published link waits the same time before it names a new address: one
/// reading of the kernel's route table in the middle of a switch still names
/// the network this machine is leaving, and showing it was the flash of an old
/// address after the rows had already been cleared.
const LINK_SETTLE: Duration = Duration::from_millis(1500);

/// Polls owed to a link that just came up, and the gap between them.
///
/// The portal's accounting row can appear a few seconds after the OS joined the
/// network (DHCP, 802.1X, the portal's own lag), so the first answer names no
/// session and the account row stays empty until the next scheduled poll - up to
/// ten seconds. Asking again at a short interval fills it as soon as the row
/// exists; the count is spent, so a link that never gets a session falls back to
/// the normal cadence instead of polling forever.
const LINK_CATCHUP_POLLS: u8 = 5;
const LINK_CATCHUP_STEP: Duration = Duration::from_secs(2);

/// How long one pass of the core loop sleeps when nothing is due. Deadlines are
/// read from the clock at the top of each pass, so this is the granularity of
/// every timer here; a link change wakes it early.
const LOOP_TICK: Duration = Duration::from_millis(400);

/// How long the core keeps trying to establish a session when nothing keeps it
/// online afterwards, measured in time the link is actually usable.
///
/// Booting is not instantaneous: Wi-Fi associates late, DHCP and DNS lag, and
/// the portal's access controller can take a moment. One attempt would give up
/// on a perfectly normal start, while retrying forever is the "stay online"
/// behaviour the user did not ask for. The same window covers a manual connect,
/// which is a request with the same shape: get online once.
///
/// Counting only link-up time keeps a machine that boots with Wi-Fi still off
/// from spending the budget on a network it never had.
const CONNECT_WINDOW_BUDGET: Duration = Duration::from_secs(300);

/// A cap on the whole window, link up or not, so a flapping link cannot keep it
/// alive indefinitely.
const CONNECT_WINDOW_DEADLINE: Duration = Duration::from_secs(900);

/// One network probe.
struct EnvProbe {
  env: LinkEnv,
  /// What the campus portal answered, when it was asked. Logged once per
  /// change: it is the evidence the campus verdict rests on, so a wrong
  /// "other network" is diagnosable from the log alone.
  portal_note: Option<String>,
}

/// Probe what the machine is attached to: the path to the internet, whether the
/// campus portal is behind it, and whether the OS owns the session.
fn probe_env(client: &crate::srun::Client, os_session: Option<String>) -> EnvProbe {
  // A route is not a link: the interface that carries it has to be up as well.
  // Without that, a driver that keeps an address on an adapter whose media went
  // away makes the window show a link that is not there.
  let connected = crate::srun::link_present();
  // The campus verdict is portal evidence, never the Wi-Fi name: a wired link, a
  // router, a VPN and an emulator behind a campus host have no campus SSID, and
  // a phone that shares its own 802.1X session to this machine has a name that
  // describes the phone's radio rather than the session. The link kind is not
  // consulted either: a tethered phone or a mobile link can carry a tunnel into
  // the campus just as well.
  let net = client.net_env();
  // Only the branches that can still be the campus network need the portal. A
  // link the campus gateway redirects already carries the ac_id, and a foreign
  // portal is not ours to ask.
  let ask_portal = matches!(net, Probe::Direct | Probe::NothingAnswered);
  let view = if ask_portal {
    client.portal_view()
  } else {
    None
  };
  let portal_note = if ask_portal {
    Some(match &view {
      None => "portal: no answer".to_string(),
      // The address stays out of the log on purpose: a log gets pasted into an
      // issue, and the address is the one thing there that identifies the
      // machine. `thunet status` still prints it for whoever asks.
      Some(view) if view.logged_in => "portal: answered, logged in".to_string(),
      Some(_) => "portal: answered, nobody logged in".to_string(),
    })
  } else {
    None
  };
  EnvProbe {
    env: classify(net, view.as_ref(), connected, os_session.as_deref()),
    portal_note,
  }
}

/// Turn the probes into the verdict the rest of the core acts on.
///
/// Three inputs, three answers, and every rule downstream - the verbs, the poll,
/// the band - is read off [`LinkEnv`] instead of being re-derived from a flat
/// list of cases:
///
/// * `connected` is a route lookup: is there a link at all.
/// * `internet` is the path: did a plain probe reach the internet without being
///   intercepted. It is what tells "the gateway is holding me back" (a login is
///   what fixes it) from "this link simply routes" (nothing to do).
/// * `portal` is the campus portal's presence: its gateway intercepted a probe,
///   it answered `rad_user_info`, someone else's portal answered, or nothing did.
///   It is the property every campus path shares - cable, campus Wi-Fi, a router
///   or VPN in between - and none of them has to carry a campus SSID.
/// * `os_session` is the portal's own word for who opened the session on this
///   address. It comes from the answer's `domain`, not from the Wi-Fi name: the
///   name describes the local radio, and a phone sharing its 802.1X session over
///   a hotspot has a radio whose name says nothing about the session.
fn classify(
  net: Probe,
  portal: Option<&crate::srun::PortalView>,
  connected: bool,
  os_session: Option<&str>,
) -> LinkEnv {
  let internet = matches!(net, Probe::Direct);
  let portal = match net {
    // The gateway named the access controller, which is also the only evidence
    // a login can be addressed to.
    Probe::Intercepted { ac_id } => Portal::Ours { ac_id: Some(ac_id) },
    Probe::ForeignPortal => Portal::Foreign,
    // Nothing intercepted, so the portal answering is the evidence: behind NAT,
    // a router or a VPN its answer still names this address.
    Probe::Direct => match portal {
      Some(_) => Portal::Ours { ac_id: None },
      None => Portal::Silent,
    },
    // The silent-drop case: the campus gateway drops pre-login traffic instead
    // of redirecting it, so the probes see nothing while a login is exactly what
    // is missing. `connected` keeps a dead interface from being called a
    // campus.
    Probe::NothingAnswered => match portal {
      Some(_) if connected => Portal::Ours { ac_id: None },
      _ => Portal::Silent,
    },
  };
  LinkEnv {
    connected,
    internet,
    portal,
    os_session: os_session.map(str::to_owned),
  }
}

/// How often a request in flight checks whether the link has moved.
const REQUEST_LINK_TICK: Duration = Duration::from_millis(100);

/// Run a portal request on its own thread and stop waiting for it when the link
/// moves.
///
/// `ureq` has no way to cancel a call already on the wire, so the request
/// finishes in the background and its answer is dropped: an answer from the
/// link this machine has just left must not be applied to the new one. The
/// caller gets `Err(())` and decides whether the new link needs another try.
fn await_request<T: Send + 'static>(
  link_gen: &AtomicU64,
  request: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ()> {
  let start = link_gen.load(Ordering::Acquire);
  let (tx, rx) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    let _ = tx.send(request());
  });
  loop {
    match rx.recv_timeout(REQUEST_LINK_TICK) {
      Ok(value) => return Ok(value),
      Err(RecvTimeoutError::Timeout) => {
        if link_gen.load(Ordering::Acquire) != start {
          return Err(());
        }
      }
      Err(RecvTimeoutError::Disconnected) => return Err(()),
    }
  }
}

/// The rows with no session behind them: whatever address the page is showing,
/// and nothing else.
///
/// Used wherever a session ends without the link doing anything - a logout, a
/// portal that stopped answering, a network with no campus portal. The address
/// is the current link's and only the link replaces it, so this leaves it alone.
fn rows_without_session(link: (Option<std::net::IpAddr>, Option<std::net::IpAddr>)) -> PortalRows {
  PortalRows {
    ip4: link.0,
    ip6: link.1,
    ..PortalRows::default()
  }
}

/// The core's own state, shared by the CLI dump and the window's service row.
fn core_info(
  started: Instant,
  session: &Session,
  link_env: &LinkEnv,
  wanted_online: bool,
  frontends: usize,
  credential: crate::proto::CredentialState,
) -> crate::proto::CoreInfo {
  crate::proto::CoreInfo {
    version: env!("CARGO_PKG_VERSION").to_string(),
    // The same stamp Settings shows, so the two halves can tell whether they
    // were compiled together.
    build_epoch: env!("THUNET_BUILD_EPOCH").parse().unwrap_or(0),
    pid: std::process::id(),
    uptime_secs: started.elapsed().as_secs(),
    state: session.state,
    env: link_env.clone(),
    ac_id: session.cfg().ac_id.clone(),
    host: session.cfg().host.clone(),
    // Read here rather than in the frontend so `thunet status` describes the
    // same link the core's own requests leave on.
    link: crate::srun::link_kind(),
    credential,
    auto_connect: session.cfg().auto_connect,
    keep_online: session.cfg().keep_online,
    wanted_online,
    // The core's own fact, not the window's: only a machine core can be started
    // by the service manager, and asking the OS from a portable core would
    // report the *installed* service while this core is a thread of a window.
    autostart: crate::ipc::is_machine() && crate::autostart::core_starts_at_boot(),
    failures: session.failures,
    frontends,
    endpoint: crate::ipc::endpoint_path(),
  }
}

/// Store one of the core's own preferences, so the next start uses it.
///
/// A preference the core cannot write would silently revert on the next start
/// - after an upgrade or a service restart - which is exactly how the window
/// and the core drifted apart before. Only the JSON is written: the sealed
/// credential is untouched, so a checkbox click cannot fail on the key.
fn save_preference(tx: &Sink, session: &Session, what: &str) {
  if let Err(e) = session.cfg().save_prefs_to(&crate::config::dir()) {
    tx.send(Ev::Log(Line::error(format!("cannot save {what}: {e}"))))
      .ok();
  }
}

/// What the core holds right now, with no way to read the password back out.
///
/// `remembered` is tracked by the worker rather than re-read from disk: it says
/// whether a sealed copy exists *and* [`Config::remember_password`] asks for it
/// to be loaded on the next start.
fn credential_state(
  session: &Session,
  remembered: bool,
  matches: Option<bool>,
) -> crate::proto::CredentialState {
  crate::proto::CredentialState {
    username: session.cfg().username.clone(),
    has_password: !session.cfg().password.is_empty(),
    remembered,
    matches,
  }
}

/// Publish the red banner, skipping a repeat of what is already on screen.
///
/// Every manual Connect after a rejection re-detects the same failure, and
/// sending it again would only make the window re-announce what it shows.
fn set_banner(tx: &Sink, banner: &mut Option<Failure>, next: Option<Failure>) {
  if *banner != next {
    *banner = next.clone();
    tx.send(Ev::Error(next)).ok();
  }
}

/// The banner for a failed portal request.
///
/// A network cause travels as a kind and the window words it in its own
/// language; anything else keeps the text the core already produced.
fn failure_of(label: &str, e: &NetError) -> Failure {
  match e.cause {
    NetCause::Other => Failure::Text(format!("{label}: {e}")),
    cause => Failure::Network(cause),
  }
}

/// Send whatever the last attempt had to say about the path to the portal.
///
/// A re-detected ac_id (`ac_id changed 240 -> 21, retrying`) describes the
/// *first*, failed attempt of the same call that then succeeded, so it belongs
/// in the log **before** the outcome line. Printed afterwards by the end of the
/// loop, the two lines read like the client logged in twice.
fn drain_warnings(tx: &Sink, session: &mut Session) {
  for w in session.take_warnings() {
    tx.send(Ev::Log(w)).ok();
  }
}

/// Replace the live session with one created by `next`, or leave it alone.
///
/// The portal hands out one session per address and proves a password only by
/// accepting a login, so trying another pair costs the current session: log
/// out, sign in with the new pair, and when the portal refuses it sign the
/// previous pair back in. Nothing reaches the disk here — the caller stores the
/// pair only on `Ok`, which is what keeps `credential.bin` on the credential
/// that is known to work and keeps a remote machine reachable.
fn switch_account(
  session: &mut Session,
  tx: &Sink,
  t: &crate::i18n::Texts,
  next: (&str, &str, bool),
) -> Result<(), Failure> {
  let (username, password, remember) = next;
  let previous = (
    session.cfg().username.clone(),
    session.cfg().password.clone(),
    session.cfg().remember_password,
  );
  tx.send(Ev::Notify {
    msg: t.switching_account.into(),
    kind: ToastKind::Info,
  })
  .ok();

  if let Err(e) = session.logout() {
    // Nothing was lost: the old session is still up and still the stored pair.
    tx.send(Ev::Log(Line::error(format!(
      "cannot switch the account, the logout failed: {e}"
    ))))
    .ok();
    return Err(failure_of(t.disconnect_failed, &e));
  }
  tx.send(Ev::Log(Line::info("logged out to switch the account")))
    .ok();

  if session.is_online_now() {
    // A "login" now would be a no-op the portal accepts without looking at the
    // password, which would store a pair that was never proven. Leave the old
    // one in place and let the user try again in a moment.
    tx.send(Ev::Log(Line::warn(
      "the portal still lists this address online; the account was not switched",
    )))
    .ok();
    return Err(Failure::Text(t.switch_pending.to_string()));
  }

  session.set_credentials(username.to_string(), password.to_string());
  session.cfg_mut().remember_password = remember;
  let outcome = session.ensure_online();
  drain_warnings(tx, session);
  match outcome {
    Ok(m) => {
      tx.send(Ev::Log(Line::info(format!("account switched: {m}"))))
        .ok();
      Ok(())
    }
    Err(e) => {
      let refused = session.failure == crate::session::FailureKind::Credentials;
      tx.send(Ev::Log(Line::error(format!(
        "the new credentials were not accepted: {e}"
      ))))
      .ok();
      // The machine has to stay reachable: this is exactly the case a remote
      // user cannot fix by hand.
      session.set_credentials(previous.0.clone(), previous.1.clone());
      session.cfg_mut().remember_password = previous.2;
      match session.ensure_online() {
        Ok(_) => tx
          .send(Ev::Log(Line::info(
            "signed back in with the previous account",
          )))
          .ok(),
        Err(e) => tx
          .send(Ev::Log(Line::warn(format!(
            "the previous account could not sign back in yet: {e}"
          ))))
          .ok(),
      };
      Err(if refused {
        Failure::Credentials
      } else {
        failure_of(t.login_failed, &e)
      })
    }
  }
}

/// One stable, short line for a status poll that got no answer.
///
/// The transport error's own text is often a whole paragraph: rustls appends a
/// docs URL to every `close_notify` report, and that single message was 57% of
/// one session's log. Known cases get a sentence; anything else is cut at its
/// first URL and capped, because the window needs the reason, not the link.
fn status_error_text(e: &(impl std::fmt::Display + ?Sized)) -> String {
  const CLOSE_NOTIFY: &str = "without sending TLS close_notify";

  let raw = e.to_string();
  if raw.contains(CLOSE_NOTIFY) {
    return "status: the portal closed the connection without close_notify".to_string();
  }

  let cut = raw
    .find(" https://")
    .or_else(|| raw.find(" http://"))
    .unwrap_or(raw.len());
  let mut text = String::from("status: ");
  text.extend(raw[..cut].trim_end().chars().take(160));
  text
}

pub fn worker(cfg: CoreConfig, rx: Receiver<Cmd>, tx: Sink) {
  let started = Instant::now();
  // The window's own setting when this process can see it: a portable core
  // reads the same file as the window, so its first toast already speaks the
  // language the page is drawn in. A machine core starts before any window and
  // reads the service account's file (usually absent), which means "auto"; the
  // window sends `SetLang` as soon as it attaches.
  let mut lang = Lang::from_setting(&crate::config::Config::load().lang);
  // `load_machine` only fills `password` when `remember_password` is set and a
  // sealed blob could be opened, so this pair is exactly "the next start will
  // find a credential".
  let mut remembered = cfg.remember_password && !cfg.password.is_empty();
  let mut session = Session::new(cfg);
  // The runtime intent: "this process should be online". Memory only - the
  // start seeds it from `auto_connect`, a manual connect sets it, and only a
  // manual disconnect or a rejected password clears it. Nothing else - a
  // logout at the portal, a kick, a timeout, a dead link - may change it, or
  // the core would silently undo what the user just did elsewhere.
  let mut want_online = session.cfg().auto_connect;
  // A core that was told to connect at start, but not to stay online, needs a
  // window to do it in: see [`CONNECT_WINDOW_BUDGET`]. With `keep_online` the
  // retry loop has no deadline, so the window is not used at all.
  let mut connect_budget = if want_online && !session.cfg().keep_online {
    CONNECT_WINDOW_BUDGET
  } else {
    Duration::ZERO
  };
  let mut connect_deadline = started + CONNECT_WINDOW_DEADLINE;
  let mut connect_tick = Instant::now();
  // Consecutive refresh failures. One is a hiccup the next one recovers from
  // (a dropped connection, a slow portal); it belongs in the log. A failure
  // that repeats is what the banner is for.
  let mut refresh_failures = 0u32;
  let mut last_status = Instant::now() - Duration::from_secs(60);
  // Consecutive status polls that got no answer at all.
  let mut status_failures = 0u32;
  // The last no-answer message and when it was logged, so a run of identical
  // failures is one line (plus the occasional reminder) rather than one per
  // poll.
  let mut last_status_log: Option<(String, Instant)> = None;
  // Ask the portal again in this pass: a new link or a new Wi-Fi name can
  // invalidate the answer the window is showing before the next period is due.
  let mut force_status = false;
  // Quick polls still owed to a link that just came up: see
  // [`LINK_CATCHUP_POLLS`].
  let mut catchup_polls = 0u8;
  // A manual refresh whose portal query failed, held until this pass has
  // re-derived the link's verdict: whether that failure is news depends on
  // whether the portal is on this link at all.
  let mut pending_refresh: Option<(bool, NetError)> = None;
  // What the window has been told. Starting at `Unknown` matches the session's
  // own start, so the first state is announced rather than assumed equal.
  let mut last_state = State::Unknown;
  // Backoff state so a rejected login does not hammer the portal every tick.
  let retry_delay = Duration::from_secs(session.cfg().reconnect_delay_secs.max(5));
  let mut next_attempt = Instant::now();
  let mut failures = 0u32;
  let mut last_failure = String::new();
  // Set when the user pressed "Connect", so a skipped attempt can be reported.
  let mut user_connect = false;
  // What the red banner shows, so the same failure is not re-announced and a
  // routine success (a status refresh) never wipes a rejection still true.
  let mut banner: Option<Failure> = None;
  // The portal rows the window is showing, kept in step with every `Ev::Info`
  // below. A link that moves without changing the transport has to keep the
  // account and the traffic of that answer while dropping the addresses in it,
  // and this is the only place that has them.
  let mut shown_info = PortalRows::default();
  // Link watch: `link_kind()` and `link_addrs()` walk the interface list and
  // the routing table, which is too much to ask sixty times a minute - and on a
  // phone that is a wakeup per second for a fact that moves a few times a day.
  // The OS reports the moves (Windows route/interface notifications, the Android
  // push), so this runs when one arrives and otherwise on the backstop below. A
  // rising edge (network just became usable) still retries immediately instead
  // of waiting out a backoff, so login starts as soon as Wi-Fi connects.
  // What the window has been told about the link. Published from this loop with
  // the rows it belongs to, so the two can never be read as separate facts.
  let mut shown_link: Option<LinkState> = None;
  let mut next_link_check = Instant::now();
  let mut link_ready = false;
  // What the last network probe saw, and the portal address whose route the link
  // watch compares. The verdict is never polled: `force_env` is set by the link
  // watch, by a route change toward the portal, and by the user, and it starts
  // true so the first pass probes at once. The initial verdict is deliberately
  // the pessimistic one.
  let mut last_env = LinkEnv {
    connected: false,
    internet: false,
    portal: Portal::Silent,
    os_session: None,
  };
  // The portal's address, resolved once per probe, and the local address this
  // machine would use to reach it. A tunnel that carries campus traffic changes
  // the second without touching the default route, which is the whole reason
  // the watch compares it.
  let mut portal_addr: Option<std::net::IpAddr> = None;
  let mut portal_route: Option<std::net::IpAddr> = None;
  // The path to the internet the last probe ran on. When it changes - a cable
  // pulled, Wi-Fi joined, a new address - the verdict describes a network the
  // machine has already left, so it is re-probed even while the session is up.
  // Both families, because a v6-only link has nothing in the first.
  let mut last_link: (Option<std::net::IpAddr>, Option<std::net::IpAddr>) = (None, None);
  // The route change being watched: the address and when it first appeared.
  // Only used to hold a move while a session is up.
  let mut pending_link: Option<(
    (Option<std::net::IpAddr>, Option<std::net::IpAddr>),
    Instant,
  )> = None;
  // The link the portal facts on screen were read on: the default-route address
  // in both families, and the local address used to reach the portal. Both
  // identify the path the portal sees, so a change in either makes the account,
  // the service name and the session addresses describe a link the machine has
  // already left.
  let mut info_path: (
    (Option<std::net::IpAddr>, Option<std::net::IpAddr>),
    Option<std::net::IpAddr>,
  ) = ((None, None), None);
  // The last transport the loop acted on: a new one means the kept-alive
  // sockets and the verdict belong to the link just left. The publication
  // itself happens on the link thread below.
  let mut last_link_kind: Option<LinkKind> = None;
  // Bumped by the link thread on every published change. A request in flight
  // compares it (see `await_request`) and is abandoned when the link moves.
  let link_gen = Arc::new(AtomicU64::new(0));
  let mut force_env = true;
  // The last thing the log said about the portal evidence: one line per change,
  // not per probe.
  let mut last_portal_note: Option<String> = None;

  for w in session.take_warnings() {
    tx.send(Ev::Log(w)).ok();
  }
  tx.send(Ev::Log(Line::debug(format!("lang: start {lang:?}"))))
    .ok();

  // The link is read and published here, in the same loop that decides what the
  // rows say. Two publishers were the problem: the link went out first and the
  // rows a pass later, so the page could read "no link" above an address. One
  // writer means the two are one statement, and the window draws them as one.
  // Waiting for the change is the watcher's job, so this costs no polling.
  let watcher = crate::linkwatch::Watcher::new();
  #[cfg(target_os = "android")]
  crate::android::install_waker(watcher.clone());

  // All state news goes through here, because `last_state` is what the window
  // has actually been told. `refresh()` and `ensure_online()` change the state
  // on their own, and an emission that skipped this bookkeeping made the tail
  // check below see "nothing changed" and stay silent: after a link switch the
  // page was told offline, then the poll found the same account online on the
  // new link, and the page kept saying offline while traffic came in.
  macro_rules! send_state {
    () => {{
      tx.send(Ev::State(session.state)).ok();
      last_state = session.state;
    }};
  }

  // Every portal answer goes out through here, so the copy this loop keeps
  // cannot drift from the one the window holds: the rows are one snapshot of
  // account, traffic and address, written by one place.
  macro_rules! send_info {
    ($value:expr) => {{
      let value = $value;
      shown_info = value.clone();
      tx.send(Ev::Info(value)).ok();
    }};
  }

  loop {
    // The mark comes first: anything the OS reports from here on has to end
    // this pass's sleep, not the one after it.
    let link_seen = watcher.generation();
    let t = lang.texts();
    while let Ok(c) = rx.try_recv() {
      match c {
        Cmd::Connect => {
          tx.send(Ev::Log(Line::debug("intent: online by request")))
            .ok();
          // A click is also a request to look again: the verdict may be minutes
          // old (it is only re-derived when something changes), and the user is
          // telling us something did.
          force_env = true;
          // A manual connect is the user's intent whatever state the machine is
          // in, and it is what lifts a rejected password.
          want_online = true;
          // Check the current state first so an existing session is not
          // re-authenticated. The query runs off this loop, so a link change
          // during it does not hold the loop for the request's timeout.
          let client = session.client().clone();
          let was_online = match await_request(&link_gen, move || client.status()) {
            Ok(Ok(v)) => {
              session.apply_status(&v);
              if session.state == State::LoggedIn {
                // This answer is a poll as much as the scheduled one: measure the
                // next poll from here, or it lands right behind this one.
                last_status = Instant::now();
                send_info!(crate::srun::portal_rows(&v));
                true
              } else {
                false
              }
            }
            // No answer, or the link moved: treat it like "not online" and let
            // the connect attempt find out on the current link.
            _ => false,
          };
          if was_online {
            send_state!();
            set_banner(&tx, &mut banner, None);
            tx.send(Ev::Log(Line::info("already online"))).ok();
            tx.send(Ev::Notify {
              msg: t.already_online.into(),
              kind: ToastKind::Info,
            })
            .ok();
          } else {
            user_connect = true;
            next_attempt = Instant::now();
            failures = 0;
            last_failure.clear();
            last_status = Instant::now() - Duration::from_secs(60);
            // A click is a request to get online once: give it the same bounded
            // window a start has, so a failing network is retried for a while
            // and then left alone. With `keep_online` the loop has no deadline,
            // so the window would only add a log line later.
            if !session.cfg().keep_online {
              connect_budget = CONNECT_WINDOW_BUDGET;
              connect_deadline = Instant::now() + CONNECT_WINDOW_DEADLINE;
              connect_tick = Instant::now();
            }
          }
        }
        Cmd::Disconnect => {
          tx.send(Ev::Log(Line::debug("intent: offline by request")))
            .ok();
          // The session on an 802.1X network belongs to the OS; ending the
          // portal's copy of it is a change this app has no business making,
          // whether the request comes from the window or from `thunet logout`.
          // And where the campus portal is not on this link there is no session
          // of ours to end at all.
          if !last_env.account_form_applies() || !last_env.portal_answers() {
            let why = if last_env.os_owns_session() {
              "refused: the portal session here is not managed by this app"
            } else {
              "refused: no campus portal on this link, so no session to end"
            };
            tx.send(Ev::Log(Line::warn(why))).ok();
            tx.send(Ev::Notify {
              msg: t.disconnect_unavailable.into(),
              kind: ToastKind::Err,
            })
            .ok();
          } else {
            // Whatever the portal answers, the user asked to stop being
            // online. This is the one place besides a rejected password that
            // drops the intent, so an offline machine still obeys the click.
            want_online = false;
            let client = session.client().clone();
            match await_request(&link_gen, move || client.status()) {
              Ok(Ok(v)) => {
                session.apply_status(&v);
                if session.state == State::LoggedIn {
                  match session.logout() {
                    Ok(m) => {
                      // The gateway starts intercepting again, so the link's verdict
                      // changes with the session and must be re-derived.
                      force_env = true;
                      // The session those rows described is over. The address this
                      // machine holds is the link's, so it stays; the account, the
                      // service, the traffic and the devices were the portal's
                      // answer about a session that no longer exists, and without
                      // this the page kept them until the next poll - up to ten
                      // seconds after the user pressed Disconnect.
                      send_info!(rows_without_session((shown_info.ip4, shown_info.ip6)));
                      // Ask at once so the rows are refilled by an answer about
                      // the link rather than left at the seed.
                      force_status = true;
                      tx.send(Ev::Log(Line::info(format!("logout: {m}")))).ok();
                      tx.send(Ev::Notify {
                        msg: t.disconnected.into(),
                        kind: ToastKind::Ok,
                      })
                      .ok();
                    }
                    Err(e) => {
                      set_banner(&tx, &mut banner, Some(failure_of(t.disconnect_failed, &e)));
                      tx.send(Ev::Notify {
                        msg: t.disconnect_failed.into(),
                        kind: ToastKind::Err,
                      })
                      .ok();
                    }
                  };
                } else {
                  // Same as above: a fresh answer restarts the poll schedule.
                  last_status = Instant::now();
                  send_info!(crate::srun::portal_rows(&v));
                  send_state!();
                  tx.send(Ev::Log(Line::info("not connected"))).ok();
                  tx.send(Ev::Notify {
                    msg: t.not_connected.into(),
                    kind: ToastKind::Info,
                  })
                  .ok();
                }
              }
              _ => {
                // The portal did not answer (or the link moved under the
                // query): as far as this click can tell, there is no session
                // to end.
                send_state!();
                tx.send(Ev::Log(Line::info("not connected"))).ok();
                tx.send(Ev::Notify {
                  msg: t.not_connected.into(),
                  kind: ToastKind::Info,
                })
                .ok();
              }
            }
          }
        }
        Cmd::Refresh { quiet } => {
          // A refresh is the user asking to look again, so the verdict is
          // re-derived as well as the session: it is only re-derived on a
          // trigger, and the user is one.
          force_env = true;
          // The portal is only asked where it can answer - the same rule as the
          // scheduled poll below. Elsewhere the query is a guaranteed timeout,
          // and what the button is for is the verdict ("this is not the campus
          // network"), not a failed request.
          if last_env.polls_portal(session.state) {
            let client = session.client().clone();
            match await_request(&link_gen, move || client.status()) {
              Ok(Ok(v)) => {
                session.apply_status(&v);
                refresh_failures = 0;
                // The scheduled poll measures from the last answer, and this is
                // one: without this the next poll kept its old phase, so a manual
                // refresh was followed by another answer seconds later and the
                // progress line restarted twice in a row.
                last_status = Instant::now();
                send_info!(crate::srun::portal_rows(&v));
                send_state!();
                // A refresh proves the portal answers, not that the stored
                // password is right: a rejection stays on screen through it,
                // unless the session turns out to be online.
                if session.state == State::LoggedIn
                  || !matches!(
                    banner.as_ref(),
                    Some(Failure::Credentials | Failure::AcUnreachable)
                  )
                {
                  set_banner(&tx, &mut banner, None);
                }
                if session.state == State::LoggedIn {
                  // Whatever the connect window was still waiting for, the machine
                  // is online now.
                  connect_budget = Duration::ZERO;
                }
                // A refresh the window started by itself only exists to make the
                // page current; it is not news.
                if !quiet {
                  tx.send(Ev::Log(Line::info("status refreshed"))).ok();
                  tx.send(Ev::Notify {
                    msg: t.status_refreshed.into(),
                    kind: ToastKind::Ok,
                  })
                  .ok();
                }
              }
              Ok(Err(e)) => {
                refresh_failures = refresh_failures.saturating_add(1);
                // The wording waits for the probe in this pass: the old verdict
                // can still name a link the machine has already left (a phone
                // swapping its upstream behind a hotspot is invisible here
                // until something asks), and "refresh failed" is only true
                // where the portal turned out to be on the link.
                pending_refresh = Some((quiet, e));
              }
              Err(()) => {
                // The link moved under the query. The page already describes
                // the new link (the link thread cleared the rows); the click
                // gets the same answer as a refresh on a link without a portal.
                refresh_failures = 0;
                last_status = Instant::now();
                if !quiet {
                  tx.send(Ev::Log(Line::debug("status refreshed: the link changed")))
                    .ok();
                  tx.send(Ev::Notify {
                    msg: t.status_refreshed.into(),
                    kind: ToastKind::Info,
                  })
                  .ok();
                }
              }
            }
          } else {
            // Nothing to ask on this link. The probe in this pass publishes what
            // it actually is, and the click is answered without an error.
            refresh_failures = 0;
            last_status = Instant::now();
            if !quiet {
              tx.send(Ev::Log(Line::debug(
                "status refreshed: no campus portal on this link",
              )))
              .ok();
              tx.send(Ev::Notify {
                msg: t.status_refreshed.into(),
                kind: ToastKind::Info,
              })
              .ok();
            }
          }
        }
        Cmd::SetLang(l) => {
          // Logged because two frontends can disagree: the window follows its
          // own setting, a CLI run follows the machine one.
          tx.send(Ev::Log(Line::debug(format!(
            "lang: SetLang {l:?} (was {lang:?})"
          ))))
          .ok();
          lang = l;
        }
        // The pair is not used on this network and cannot be tried, so storing
        // it would leave the machine with an unverified password. Answering
        // with the current state keeps the window from waiting for a reply.
        Cmd::SetCredentials { .. } if !last_env.account_form_applies() => {
          tx.send(Ev::Credentials(credential_state(
            &session, remembered, None,
          )))
          .ok();
          tx.send(Ev::Log(Line::warn(
            "skipped: no portal account is used on this network",
          )))
          .ok();
          tx.send(Ev::Notify {
            msg: t.credentials_unavailable.into(),
            kind: ToastKind::Err,
          })
          .ok();
        }
        Cmd::SetCredentials {
          username,
          password,
          remember,
          keep_password,
        } => {
          let dir = crate::config::dir();
          // "Remember the password the core already holds". The window never
          // keeps the plaintext, so after an Apply that did not save it the
          // password field is empty and the only pair left to seal is the one
          // in memory. That is allowed only while the form still names the
          // account the held password was proven for; pairing a held password
          // with a different account stays refused.
          let keeps_held = keep_password
            && password.is_empty()
            && !session.cfg().password.is_empty()
            && username.trim() == session.cfg().username.trim();
          let password = if keeps_held {
            session.cfg().password.clone()
          } else {
            password
          };
          let ready = !username.is_empty() && !password.is_empty();
          // *Remember password* needs a complete pair: an empty field means
          // "keep what is stored" (the window disables *Apply* in that state),
          // so a request like this has nothing to write and nothing to change —
          // and it must not rewrite the sealed blob or lift a rejection either.
          // Changing the account without retyping the password is not allowed.
          if remember && !ready {
            tx.send(Ev::Credentials(credential_state(
              &session, remembered, None,
            )))
            .ok();
            continue;
          }
          // Whether this Apply actually changes the pair. The same pair must
          // not lift a rejection, or Apply would turn into a retry button for
          // a password the portal already refused.
          let new_pair = session.cfg().username != username || session.cfg().password != password;
          // While online, the pair is *proven* before it is stored: the old
          // session goes away anyway (the portal serves one account per
          // address), so the only question is whether the machine comes back
          // on the new credentials or on the old ones. Offline the pair is
          // stored as given and queued for the retry loop: there is no session
          // to protect, and the account has to be ready when the network
          // returns.
          if ready && new_pair && session.state == State::LoggedIn {
            match switch_account(&mut session, &tx, t, (&username, &password, remember)) {
              Ok(()) => {
                want_online = true;
                failures = 0;
                last_failure.clear();
                next_attempt = Instant::now();
                match session.cfg().save_to(&dir) {
                  Ok(()) => {
                    remembered = remember && !session.cfg().password.is_empty();
                    set_banner(&tx, &mut banner, None);
                    tx.send(Ev::Notify {
                      msg: t.switched_account.into(),
                      kind: ToastKind::Ok,
                    })
                    .ok();
                  }
                  // The session runs on the new pair, so this is about the
                  // next start, not about now.
                  Err(e) => set_banner(
                    &tx,
                    &mut banner,
                    Some(Failure::Text(format!("cannot save the credential: {e}"))),
                  ),
                }
              }
              Err(failure) => {
                // Nothing was written, so the stored credential still works and
                // the loop may keep retrying with it.
                want_online = session.state != State::LoggedIn;
                failures = 0;
                last_failure.clear();
                next_attempt = Instant::now();
                if !session.cfg().keep_online {
                  // The user just asked for this pair: give the previous one
                  // the same bounded chance a manual connect gets.
                  connect_budget = CONNECT_WINDOW_BUDGET;
                  connect_deadline = Instant::now() + CONNECT_WINDOW_DEADLINE;
                  connect_tick = Instant::now();
                }
                // The typed pair was refused, but the previous account is back:
                // the machine is online and its stored credential was *not*
                // rejected, so the red banner would be a lie that never clears.
                // The toast and the log line carry the news instead. Only a
                // switch that left the machine offline keeps the banner.
                let back_online = session.state == State::LoggedIn;
                set_banner(
                  &tx,
                  &mut banner,
                  if back_online { None } else { Some(failure) },
                );
                tx.send(Ev::Notify {
                  msg: t.switch_failed.into(),
                  kind: ToastKind::Err,
                })
                .ok();
              }
            }
            tx.send(Ev::Credentials(credential_state(
              &session, remembered, None,
            )))
            .ok();
          } else {
            if ready {
              session.set_credentials(username.clone(), password.clone());
            } else if !remember {
              // An empty form with "remember" still ticked is almost certainly
              // the window reopening without the password; do not wipe a stored
              // credential. Unticking the box is the explicit clear.
              session.set_credentials(String::new(), String::new());
            }
            if new_pair {
              // A cleared pair has no attempt left in it, so a rejection would
              // stay on screen with nothing behind it.
              if !ready {
                set_banner(&tx, &mut banner, None);
              }
            }
            // The sealed blob alone is not enough: the core also has to record
            // that it wants it read back. Without `remember_password` in its own
            // `config.json` a restarted core ignores the blob and comes up with
            // no credential at all (the window would then show an empty, unsaved
            // password even though Apply had worked).
            session.cfg_mut().remember_password = remember;
            match session.cfg().save_to(&dir) {
              Ok(()) => remembered = remember && !session.cfg().password.is_empty(),
              Err(e) => {
                set_banner(
                  &tx,
                  &mut banner,
                  Some(Failure::Text(format!("cannot save the credential: {e}"))),
                );
              }
            }
            // Answer even when the request was a no-op: the window uses the
            // reply to learn whether the value it typed is the stored one.
            tx.send(Ev::Credentials(credential_state(
              &session, remembered, None,
            )))
            .ok();
            // Try a complete pair at once, because the backoff in flight was
            // chosen for the old one. An edit is the user asking to be online,
            // so it sets the intent regardless of `auto_connect`; the startup
            // window is not needed either, since this is a manual attempt.
            if ready && session.state != State::LoggedIn {
              tx.send(Ev::Log(Line::debug("intent: online by credentials")))
                .ok();
              want_online = true;
              next_attempt = Instant::now();
              failures = 0;
              last_failure.clear();
              last_status = Instant::now() - Duration::from_secs(60);
              if !session.cfg().keep_online {
                connect_budget = CONNECT_WINDOW_BUDGET;
                connect_deadline = Instant::now() + CONNECT_WINDOW_DEADLINE;
                connect_tick = Instant::now();
              }
            }
          }
        }
        Cmd::CheckCredentials { username, password } => {
          let matches = if session.cfg().password.is_empty() {
            None
          } else {
            Some(
              session.cfg().username.trim() == username.trim()
                && session.cfg().password == password,
            )
          };
          tx.send(Ev::Credentials(credential_state(
            &session, remembered, matches,
          )))
          .ok();
        }
        Cmd::SetAutoConnect(on) => {
          // A startup preference: stored for the next start, and nothing here
          // touches `want_online`. Turning it off is not "stop being online" -
          // that is what Disconnect is for.
          session.cfg_mut().auto_connect = on;
          save_preference(&tx, &session, "auto connect");
        }
        Cmd::SetKeepOnline(on) => {
          // This preference decides what may happen after a session is lost;
          // it is not a request to log in now, so `want_online` stays as it is.
          session.cfg_mut().keep_online = on;
          save_preference(&tx, &session, "keep online");
          if on {
            // The retry loop now owns every attempt, so a connect window that
            // was still ticking has nothing left to decide.
            connect_budget = Duration::ZERO;
            if want_online && session.state != State::LoggedIn {
              // Answer the click: a machine that was waiting for the window to
              // expire (or for a dropped session to be ignored) tries at once.
              next_attempt = Instant::now();
              failures = 0;
              last_failure.clear();
              last_status = Instant::now() - Duration::from_secs(60);
            }
          }
        }
        Cmd::Status => {
          // One dump with both halves: what the core is doing, and what the
          // portal says. A failed portal query still returns the core part.
          let info = core_info(
            started,
            &session,
            &last_env,
            want_online,
            tx.frontends(),
            credential_state(&session, remembered, None),
          );
          let mut dump = serde_json::json!({ "core": info });
          match session.refresh() {
            Ok(v) => dump["portal"] = v,
            Err(e) => {
              dump["portal"] = serde_json::Value::Null;
              dump["portal_error"] =
                serde_json::Value::String(format!("{}: {e}", t.refresh_failed));
            }
          }
          tx.send(Ev::Status(dump)).ok();
        }
        Cmd::CoreInfo => {
          let info = core_info(
            started,
            &session,
            &last_env,
            want_online,
            tx.frontends(),
            credential_state(&session, remembered, None),
          );
          tx.send(Ev::Core(info)).ok();
        }
        Cmd::AcId => {
          tx.send(Ev::AcId(session.cfg().ac_id.clone())).ok();
        }
        Cmd::Shutdown => {
          if crate::ipc::is_machine() {
            tx.send(Ev::Log(Line::warn(
              "ignoring Shutdown: the machine core is stopped through the service manager",
            )))
            .ok();
          } else if crate::ipc::hosted() {
            // The core is a thread of the window that started it: the window
            // stays, the core ends. Exiting the process here would close the
            // window as well, which is what the pre-host design did.
            crate::ipc::request_stop();
            tx.send(Ev::Log(Line::info("stopping: this window hosts the core")))
              .ok();
            return;
          } else {
            // A portable core belongs to the user who started it; exiting here
            // is the same as closing its terminal.
            std::process::exit(0);
          }
        }
      }
    }

    // A login attempt without credentials can only be rejected, so say what is
    // missing instead of POSTing an empty password to the portal. The attempt
    // below is gated on the same flag, so nothing can reach the portal either
    // way; this branch only decides what the user is told.
    let missing_creds = session.cfg().username.is_empty() || session.cfg().password.is_empty();
    if want_online && session.state != State::LoggedIn && missing_creds {
      let log = "skipped: no account or password to log in with";
      if last_failure != log {
        last_failure = log.to_string();
        tx.send(Ev::Log(Line::warn(log))).ok();
        if user_connect {
          tx.send(Ev::Notify {
            msg: t.no_credentials.into(),
            kind: ToastKind::Info,
          })
          .ok();
          user_connect = false;
        }
      }
    }

    // Fast link watch: retry the moment a usable route appears.
    //
    // This runs *before* the probe on purpose: a rising edge makes the probe
    // happen in this same pass, so the attempt at the bottom sees the network
    // that just appeared instead of the verdict from before it came up. (With
    // the probe first, boot used to skip once on "no network" and then sit out
    // that skip's 30 s delay even though the link was already usable.)
    //
    // It runs unconditionally: the same route lookup is also what notices that
    // the machine moved to a different network while a session was up, which
    // used to go unseen because the probe did not run while online.
    //
    // A move being held for `LINK_SETTLE` is the one deadline the backstop
    // cannot cover: it was started by an event, and waiting out the backstop
    // would hold a Wi-Fi switch for half a minute.
    let settle_due = pending_link
      .as_ref()
      .is_some_and(|(_, since)| since.elapsed() >= LINK_SETTLE);
    if Instant::now() >= next_link_check || settle_due {
      next_link_check = Instant::now() + crate::linkwatch::PERIOD;
      // A route is not a link: a driver can keep one on an adapter whose media
      // went away. Gating the identity on the interface's own state is what
      // clears the rows when the cable is out while the address is still cached.
      let kind = crate::srun::link_kind();
      let id = if kind != LinkKind::None {
        crate::srun::link_addrs()
      } else {
        (None, None)
      };
      let state = LinkState {
        kind,
        ip4: id.0,
        ip6: id.1,
      };
      // The link decides the address; what the session hangs on is the address
      // *the portal sees*, which is the route source below. So this step only
      // moves the addresses: the account and the traffic stay until that key
      // changes.
      if shown_link != Some(state) {
        shown_link = Some(state);
        link_gen.fetch_add(1, Ordering::Release);
        let families = match (state.ip4.is_some(), state.ip6.is_some()) {
          (true, true) => "v4+v6",
          (true, false) => "v4",
          (false, true) => "v6",
          (false, false) => "no address",
        };
        tx.send(Ev::Log(Line::debug(format!(
          "link: {:?} ({families})",
          state.kind
        ))))
        .ok();
        tx.send(Ev::Link(state)).ok();
        let mut rows = shown_info.clone();
        rows.ip4 = state.ip4;
        rows.ip6 = state.ip6;
        if state.ip4.is_none() && state.ip6.is_none() {
          // No address at all means there is no address the portal could see,
          // so nothing a session could hang on.
          rows = PortalRows::default();
        }
        if rows != shown_info {
          send_info!(rows);
        }
      }
      // Read before the settle below decides what to blank: whether this is a
      // new transport is what makes the rows describe a link that is gone.
      let kind_changed = last_link_kind != Some(kind);
      // A route change only counts once it holds: a DHCP renew or a Wi-Fi roam
      // flips the local address for a second, and acting on that took the
      // window offline and started a fresh login for nothing. While a session
      // is up the move waits for LINK_SETTLE; with nothing to protect it is
      // taken at once, so a fresh link still logs in within the first second.
      let settled = if id == last_link {
        // A blip that came back: the link is the one it was, so nothing has to
        // be re-decided - but the rows may have been told to ask again.
        if pending_link.take().is_some() {
          force_status = true;
        }
        false
      } else if session.state != State::LoggedIn {
        pending_link = None;
        true
      } else {
        match pending_link {
          Some((pending, since)) if pending == id => {
            if since.elapsed() >= LINK_SETTLE {
              pending_link = None;
              true
            } else {
              false
            }
          }
          _ => {
            pending_link = Some((id, Instant::now()));
            false
          }
        }
      };
      if settled {
        last_link = id;
        // The answer the window is showing belongs to the address this machine no
        // longer holds: ask again now, so the session found on the new one is not
        // held back by the offline poll period.
        force_status = true;
        // The new link's first answer can arrive before the portal's own row
        // does; keep asking for a few seconds so the account row fills without
        // waiting out the whole period.
        catchup_polls = LINK_CATCHUP_POLLS;
        // A different path to the internet: the verdict, the backoff in flight
        // and the reason last logged all belong to the old one.
        force_env = true;
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
      }
      let up = id.0.is_some() || id.1.is_some();
      if up && !link_ready {
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
        force_env = true;
      }
      link_ready = up;
      // The trigger that a default-route check cannot see: an SSL VPN that
      // carries campus traffic installs a route to the portal's prefix and
      // leaves the default route alone (aTrust, Clash TUN and friends all do
      // this), so the source address toward the portal changes while
      // `link_id()` does not. It is the same route lookup, so it costs nothing
      // and sends nothing.
      if let Some(addr) = portal_addr {
        let now = crate::srun::route_source(std::net::SocketAddr::new(addr, 443));
        if now != portal_route {
          portal_route = now;
          force_env = true;
        }
      }
      // The link facts themselves are published above; here only their
      // consequences for this loop. The path block below covers a changed
      // address; this covers a changed transport.
      if kind_changed {
        last_link_kind = Some(kind);
        session.reset_pool();
        force_env = true;
      }
      // What the rows say about the session is the portal's own word, kept
      // verbatim: nothing here invents or retires it on an address change. A
      // moved path only means the answer is stale for the *poll* below.
      let path = (id, portal_route);
      if path != info_path {
        let route_moved = info_path.1 != portal_route;
        info_path = path;
        session.reset_pool();
        if route_moved {
          force_status = true;
          if session.state == State::LoggedIn {
            session.state = State::Unknown;
          }
          // Families, not addresses: a log line gets pasted into an issue, and
          // the address is the one thing there that identifies the machine.
          let families = match (path.0.0.is_some(), path.0.1.is_some()) {
            (true, true) => "v4+v6",
            (true, false) => "v4",
            (false, true) => "v6",
            (false, false) => "no address",
          };
          let route = if path.1.is_some() { "known" } else { "unknown" };
          tx.send(Ev::Log(Line::debug(format!(
            "link changed, asking the portal again ({families}, route {route})"
          ))))
          .ok();
        }
      }
    }

    // Spend the connect window only while a link exists, so a machine that
    // boots with Wi-Fi still off does not burn the budget on a network it never
    // had; the wall-clock cap stops a flapping link from keeping it alive.
    if !connect_budget.is_zero() {
      let now = Instant::now();
      if link_ready {
        connect_budget = connect_budget.saturating_sub(now.saturating_duration_since(connect_tick));
      }
      connect_tick = now;
      if connect_budget.is_zero() || now >= connect_deadline {
        connect_budget = Duration::ZERO;
        tx.send(Ev::Log(Line::info(
          "connect window closed, staying offline until asked again",
        )))
        .ok();
      }
    }

    // Probe the network when something asked for it: the link watch saw a new
    // route or address, the route toward the portal changed, the portal answered
    // with a different session owner, or the user asked (a click or a refresh).
    // There is no timer: a verdict nobody changed cannot change by itself.
    if force_env {
      force_env = false;
      let client = session.client().clone();
      let os_session = session.os_session().map(str::to_owned);
      let host = session.cfg().host.clone();
      let probe = await_request(&link_gen, move || {
        let probe = probe_env(&client, os_session);
        let addr = crate::srun::resolve_host(&host);
        let route = addr.and_then(|a| crate::srun::route_source(std::net::SocketAddr::new(a, 443)));
        (probe, addr, route)
      });
      if let Ok((probe, addr, route)) = probe {
        let env = probe.env;
        // The portal's address is what the link watch compares a route to, so it
        // is resolved here, once per probe, and kept when the name does not answer
        // (a hotspot whose DNS is not up yet, a link that filters it).
        if let Some(addr) = addr
          && portal_addr != Some(addr)
        {
          portal_addr = Some(addr);
          if route != portal_route {
            portal_route = route;
            // The path the portal is reached on moved - a tunnel appeared, or the
            // name resolved somewhere else. The rows keep the portal's own word
            // and the poll below re-asks on this path; nothing is invented here.
            if info_path.1 != route {
              info_path.1 = route;
              session.reset_pool();
              force_status = true;
            }
          }
        }
        if let Portal::Ours { ac_id: Some(ac_id) } = &env.portal {
          session.set_ac_id(ac_id.clone());
        }
        if let Some(note) = &probe.portal_note
          && last_portal_note.as_ref() != Some(note)
        {
          tx.send(Ev::Log(Line::debug(note.clone()))).ok();
          last_portal_note = probe.portal_note;
        }
        // The verdict changed (no link → campus, or back), so decide again in this
        // pass instead of sitting out a backoff chosen for the old one.
        if env != last_env {
          tx.send(Ev::Log(Line::debug(format!("network: {env:?}"))))
            .ok();
          next_attempt = Instant::now();
          failures = 0;
          last_failure.clear();
        }
        // Only a link that carries our portal has a session row to wait for. A
        // change to one restarts the catch-up; any other verdict ends it instead
        // of asking a portal that will never answer.
        if env.portal_answers() {
          if env != last_env {
            catchup_polls = LINK_CATCHUP_POLLS;
          }
        } else {
          catchup_polls = 0;
        }
        // A red band about reaching the portal cannot outlive the link it was
        // measured on. Once the verdict says the portal is not here, the band and
        // the verdict below it state the same fact in opposite tones; a rejection
        // is about the account rather than the link, so it stays.
        if !env.portal_answers() && matches!(banner, Some(Failure::Network(_))) {
          set_banner(&tx, &mut banner, None);
        }
        if !env.portal_answers() {
          // No campus portal behind this link, so no answer is coming: the rows
          // say what the machine holds and nothing about a session. Published
          // with the verdict - the event that established it - rather than with
          // every reading of the interface, which is what used to put the
          // address of the link this machine had just left on screen.
          let rows = rows_without_session((shown_info.ip4, shown_info.ip6));
          if rows != shown_info {
            send_info!(rows);
          }
        }
        last_env = env.clone();
        tx.send(Ev::Env(env)).ok();
      } else {
        // The link moved while the probe was in flight; its verdict describes
        // the network this machine has left. Ask again on the next pass.
        force_env = true;
      }
    }

    // Answer a manual refresh now that the verdict has been re-derived: a
    // failed portal query is news only where the portal is on this link. On a
    // link that works without it, the verdict above is the answer, and calling
    // that a failure would contradict the page and the probes.
    if let Some((quiet, e)) = pending_refresh.take() {
      if last_env.portal_answers() {
        // The technical sentence (a TLS or DNS message) is far too long for a
        // band, so the log keeps it. One failed refresh is not news for the page
        // either - the next one clears it - so only a failure that repeats
        // raises the banner; the toast answers the button press.
        tx.send(Ev::Log(Line::warn(format!("refresh failed: {e}"))))
          .ok();
        if refresh_failures >= 2 {
          set_banner(&tx, &mut banner, Some(failure_of(t.refresh_failed, &e)));
        }
        if !quiet {
          tx.send(Ev::Notify {
            msg: t.refresh_failed.into(),
            kind: ToastKind::Err,
          })
          .ok();
        }
      } else {
        refresh_failures = 0;
        tx.send(Ev::Log(Line::debug(format!(
          "status refreshed: no campus portal on this link ({e})"
        ))))
        .ok();
        if !quiet {
          tx.send(Ev::Notify {
            msg: t.status_refreshed.into(),
            kind: ToastKind::Info,
          })
          .ok();
        }
      }
    }

    // Whether an automatic attempt may be made at all. `keep_online` is the
    // standing permission for it; without it, only an open connect window
    // allows one, which is what makes "connect at start" mean exactly that.
    let auto_attempts = want_online && (session.cfg().keep_online || !connect_budget.is_zero());
    if !missing_creds
      && auto_attempts
      && session.state != State::LoggedIn
      && link_ready
      && Instant::now() >= next_attempt
    {
      // One rule decides whether an attempt is worth making - the link has to
      // allow a login - and the branches below only word *why* it does not.
      // Without that rule a login would spray a link where it cannot work, or
      // vouch for an address that is not this machine's own.
      let mut skip: Option<(String, String, Duration, ToastKind)> = None;
      if last_env.login_possible() {
        // Fall through to the attempt.
      } else if let Some(service) = last_env.os_session.as_deref() {
        skip = Some((
          t.secure_wifi.to_string(),
          format!("skipped: the session is the OS's own ({service}, 802.1X)"),
          Duration::from_secs(60),
          ToastKind::Info,
        ));
      } else if !last_env.portal_answers() {
        // Nothing of ours is behind this link: a login would hand the account
        // to whatever sits on it.
        let (text, log) = match last_env.portal {
          Portal::Foreign => (
            t.not_campus_network.to_string(),
            "skipped: this is not the campus portal".to_string(),
          ),
          _ if last_env.internet => (
            t.no_login_needed.to_string(),
            "skipped: this is not the campus network".to_string(),
          ),
          _ => (
            t.network_unreachable.to_string(),
            "skipped: neither the probes nor the portal answered".to_string(),
          ),
        };
        skip = Some((text, log, Duration::from_secs(60), ToastKind::Info));
      } else if last_env.internet {
        // Our portal, and the link routes anyway: a grace period, or an address
        // the portal is not gating (a VPN's egress, a router already signed in).
        // A login here would authenticate that shared address for everybody
        // behind it, so one answer is all this gets.
        let client = session.client().clone();
        let online = match await_request(&link_gen, move || client.status()) {
          Ok(Ok(v)) => {
            session.apply_status(&v);
            session.state
          }
          _ => State::Unknown,
        };
        if online != State::LoggedIn {
          skip = Some((
            t.no_login_needed.to_string(),
            "skipped: the portal reports no session and the link already routes".to_string(),
            Duration::from_secs(60),
            ToastKind::Info,
          ));
        }
      }

      if let Some((msg, log, delay, kind)) = skip {
        next_attempt = Instant::now() + delay;
        failures = 0;
        // Log (and toast) only when the reason changes, not on every retry.
        if last_failure != log {
          last_failure = log.clone();
          tx.send(Ev::Log(Line::warn(&log))).ok();
          if user_connect {
            tx.send(Ev::Notify { msg, kind }).ok();
            user_connect = false;
          }
        }
        send_state!();
        continue;
      }

      let outcome = session.ensure_online();
      drain_warnings(&tx, &mut session);
      match outcome {
        Ok(m) => {
          // A click is what a toast answers. An automatic attempt is not news,
          // and at startup it would flash in the OS language before the window
          // has sent its own.
          let answered = user_connect;
          failures = 0;
          last_failure.clear();
          user_connect = false;
          // Connected: the connect window has done its job. Whether the core
          // keeps it that way is `keep_online`'s decision from here on.
          connect_budget = Duration::ZERO;
          set_banner(&tx, &mut banner, None);
          // The gateway stops intercepting once the session is up, so the
          // verdict this link earns has changed too - and it is never polled.
          force_env = true;
          // A login names the account but carries none of the numbers the
          // status page shows: without an answer here the page drew Online
          // with an empty account row until the next scheduled poll, which is
          // what a `keep_online` re-login looked like when another client
          // ended the shared session. The poll below is that answer.
          force_status = true;
          tx.send(Ev::Log(Line::info(m))).ok();
          if answered {
            tx.send(Ev::Notify {
              msg: t.connect_ok.into(),
              kind: ToastKind::Ok,
            })
            .ok();
          }
        }
        Err(e) => {
          failures = failures.saturating_add(1);
          let bad_credentials = session.failure == crate::session::FailureKind::Credentials;
          let ac_unreachable = session.failure == crate::session::FailureKind::AcUnreachable;
          // What the next pass may do differs per kind of failure.
          //
          // A rejected password is deterministic: the same pair can only be
          // refused again, and retrying it hammers the portal (and may trip its
          // lockout). So is an `ac_id` the portal cannot route - retrying the
          // same id on the same link cannot work either. Both therefore drop the
          // intent, the single gate every automatic path goes through, and only
          // a manual Connect (or a new pair) brings it back.
          //
          // The cost is that a machine that moves back to the network its id
          // belongs to does not recover by itself; the banner says so.
          //
          // Everything else is a transport hiccup: back off and try again.
          let final_refusal = bad_credentials || ac_unreachable;
          let next_retry = if final_refusal {
            want_online = false;
            Duration::ZERO
          } else {
            // 5s, 10s, 20s, 40s, ... capped at two minutes.
            let backoff = retry_delay.as_secs_f64() * 2f64.powi((failures - 1).min(5) as i32);
            Duration::from_secs_f64(backoff.min(120.0))
          };
          next_attempt = Instant::now() + next_retry;
          let answered = user_connect;
          user_connect = false;
          // A credential rejection carries a long JSON blob that only helps
          // when debugging, so it goes to the log pane while the red banner
          // says what the user can act on. The banner is worded by the
          // frontend, so a language switch re-words it, and it stays up until
          // a login succeeds.
          //
          // A transport failure is different: a timeout or a cut connection is
          // usually over by the next attempt, and every such retry painting the
          // page red reads as "everything is broken". The log keeps it, and
          // only a failure that survives one more attempt raises the band.
          if bad_credentials || ac_unreachable || failures >= 2 {
            set_banner(
              &tx,
              &mut banner,
              Some(if bad_credentials {
                Failure::Credentials
              } else if ac_unreachable {
                // The id belongs to another link; the banner says so instead of
                // pasting the portal's answer.
                Failure::AcUnreachable
              } else {
                // The frontend shows this verbatim, so the words have to be
                // here: a refresh or disconnect failure already says what it
                // was.
                failure_of(t.login_failed, &e)
              }),
            );
          }
          // Say what happens next, so the log explains the silence that follows
          // a rejection instead of leaving the reader wondering about a retry.
          let note = if bad_credentials || ac_unreachable {
            "automatic attempts stopped; connect manually to try again".to_string()
          } else {
            format!("retry in {}s", next_retry.as_secs())
          };
          tx.send(Ev::Log(Line::error(format!("{e} ({note})")))).ok();
          // Only buzz once per distinct reason, otherwise retries spam toasts.
          // A rejected password is not a toast: the banner already says it and
          // stays there, while a toast would flash and vanish.
          let detail = e.to_string();
          if detail != last_failure {
            last_failure = detail;
            if answered && !bad_credentials && !ac_unreachable {
              tx.send(Ev::Notify {
                msg: t.connect_failed.into(),
                kind: ToastKind::Err,
              })
              .ok();
            }
          }
        }
      };
    }

    // Poll the portal on the period, plus the catch-up owed to a link that just
    // came up. `last_status` is the last actual poll, so a forced one in this
    // pass already restarts the gap. A poll is only made where the portal can
    // answer (see [`LinkEnv::polls_portal`]): elsewhere it was a three-second
    // timeout whose failure the verdict already explains, and nothing it found
    // could change the page. The beat is sent either way - it is the core's own
    // liveness, and a link with no portal still has to show that the loop runs.
    let catchup_due = catchup_polls > 0 && last_status.elapsed() >= LINK_CATCHUP_STEP;
    let poll_every = if session.state == State::LoggedIn {
      POLL_ONLINE
    } else {
      POLL_OFFLINE
    };
    let poll_due = force_status || catchup_due || last_status.elapsed() >= poll_every;
    if poll_due {
      if last_env.polls_portal(session.state) {
        force_status = false;
        let client = session.client().clone();
        if let Ok(answer) = await_request(&link_gen, move || client.status()) {
          match &answer {
            Ok(v) => {
              status_failures = 0;
              last_status_log = None;
              // The answer names the service behind the session; when that changes,
              // the verdict that quotes it ("the OS owns this session") is stale.
              if session.note_session_service(v) {
                force_env = true;
              }
            }
            Err(e) => {
              // The portal did not answer at all: no route, a dead link, or a host
              // that stopped responding. While the machine sits on such a link
              // every poll fails the same way, so a message is logged once and then
              // only again when it changes or STATUS_LOG_REPEAT has passed.
              let text = status_error_text(e);
              let repeat_due = match &last_status_log {
                Some((last, at)) => last != &text || at.elapsed() >= STATUS_LOG_REPEAT,
                None => true,
              };
              if repeat_due {
                tx.send(Ev::Log(Line::debug(text.clone()))).ok();
                last_status_log = Some((text, Instant::now()));
              }
              if session.state == State::LoggedIn {
                status_failures = status_failures.saturating_add(1);
                if status_failures >= STATUS_FAILURES {
                  // Not "logged out": the portal never answered, so whether a
                  // session exists is unknown. Claiming one or the other is how the
                  // window ended up saying "offline" on a link that was working.
                  session.state = State::Unknown;
                  tx.send(Ev::Log(Line::warn("portal not answering, no answer yet")))
                    .ok();
                  send_info!(rows_without_session((shown_info.ip4, shown_info.ip6)));
                }
              }
            }
          }
          // An answer that names a session ends the catch-up; a missing one spends
          // one of the owed polls and then falls back to the normal cadence.
          if catchup_polls > 0 {
            let online = answer
              .as_ref()
              .is_ok_and(|v| v.get("online").and_then(|b| b.as_bool()).unwrap_or(false));
            catchup_polls = if online { 0 } else { catchup_polls - 1 };
          }
          if let Ok(v) = answer {
            // An answer proves the portal is reachable, so a band about failing to
            // reach it is out of date. The scheduled poll counts as proof, not just
            // a manual refresh: without this a single outage left the band up until
            // somebody pressed *Refresh* again.
            if matches!(banner, Some(Failure::Network(_))) {
              set_banner(&tx, &mut banner, None);
            }
            // The portal just answered on this link, so a verdict that says it is
            // not behind the link cannot still be true. One failed request during a
            // network change is enough to produce one, and while the session is up
            // the probe timer never runs - which left the portal verbs greyed out
            // until the link changed again. Ask again right away.
            if !last_env.portal_answers() {
              force_env = true;
            }
            let online = v.get("online").and_then(|b| b.as_bool()).unwrap_or(false);
            if online && session.state != State::LoggedIn {
              session.state = State::LoggedIn;
              failures = 0;
              // Being online is what the connect window was for: it is done. A
              // session found here may have been opened elsewhere, which is still
              // "online" as far as the machine is concerned.
              connect_budget = Duration::ZERO;
              set_banner(&tx, &mut banner, None);
              tx.send(Ev::Log(Line::info("already online"))).ok();
            } else if !online && session.state == State::LoggedIn {
              // Session ended outside our control (timeout / kicked / unplugged).
              session.state = State::LoggedOut;
              tx.send(Ev::Log(Line::warn("session ended, logged out")))
                .ok();
            } else if online
              && matches!(
                banner.as_ref(),
                Some(Failure::Credentials | Failure::AcUnreachable)
              )
            {
              // A credential banner is only true while the machine cannot sign in.
              // This session proves the stored pair works, so drop it even though
              // the state did not change: that is the case after a refused switch
              // that signed the previous account back in. The same goes for the
              // "already online" notice - this session *is* the account's session.
              set_banner(&tx, &mut banner, None);
            }
            send_info!(crate::srun::portal_rows(&v));
          }
        } else {
          // The link moved while the poll was in flight; its answer would
          // describe the network this machine has left. The new link asks again
          // on the next pass.
          tx.send(Ev::Log(Line::debug("status: dropped, the link changed")))
            .ok();
          force_status = true;
        }
      } else {
        // A forced poll is spent here as well, and a catch-up owed to a link
        // with no portal can never be answered.
        force_status = false;
        catchup_polls = 0;
      }
      // The beat is the loop, not the poll's answer: the window draws it as a
      // heartbeat, and neither a silent portal nor a link without one may look
      // like a core that stopped.
      tx.send(Ev::Beat).ok();
      last_status = Instant::now();
    }

    if session.state != last_state {
      send_state!();
      tx.send(Ev::Log(Line::debug(format!("state: {:?}", session.state))))
        .ok();
    }
    // The account row is the portal's own word and nothing else, so it is emptied
    // in exactly three places: the link is gone (above, no address at all), the
    // machine is not logged in (here), or an answer names nobody (the answer
    // itself, which is what replaces these rows anyway). A new address is a
    // question for the portal, not a reason to hide what it last said.
    if session.state == State::LoggedOut && !shown_info.name.is_empty() {
      send_info!(rows_without_session((shown_info.ip4, shown_info.ip6)));
    }

    for w in session.take_warnings() {
      tx.send(Ev::Log(w)).ok();
    }

    // Wait for the next tick, or until the OS says the link moved.
    //
    // Waking early only moves the link check forward, so a change becomes a
    // pass at once instead of up to a tick later; the tick stays because every
    // other deadline in this loop (polls, backoff, command handling) is read
    // from the clock on each pass.
    if watcher.wait_since(link_seen, LOOP_TICK) {
      next_link_check = Instant::now();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A link that has no portal on it is asked once and then left alone: the
  /// verdict is re-derived from triggers, never from a timer.
  #[test]
  fn the_portal_route_lookup_follows_the_target() {
    // Loopback always exists, so this part is the same on every machine: the
    // lookup names the address the packet *would* leave from, and sends none.
    let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 443));
    assert_eq!(
      crate::srun::route_source(loopback),
      Some(std::net::IpAddr::from([127, 0, 0, 1]))
    );
    // A literal needs no DNS, so this is offline-safe; a name is allowed to
    // fail here (a hotspot whose resolver is not up yet).
    assert_eq!(
      crate::srun::resolve_host("192.0.2.7"),
      Some(std::net::IpAddr::from([192, 0, 2, 7]))
    );
    assert!(crate::srun::resolve_host("192.0.2.7").is_some());
  }

  /// The verdict is built from the path, the portal and the session's owner -
  /// and nothing about the Wi-Fi name enters it: a phone sharing its own 802.1X
  /// session to this machine is a link whose local radio says something else
  /// entirely, and the portal's `domain` is the only honest evidence.
  #[test]
  fn the_verdict_is_built_from_the_path_and_the_portal() {
    let answered = crate::srun::PortalView {
      logged_in: true,
      address: Some("192.0.2.10".into()),
    };
    let no_session = crate::srun::PortalView {
      logged_in: false,
      address: None,
    };
    // No probe answered, so `internet` is false: a login is exactly what is
    // missing. Whether a session exists is the portal's answer, not this one.
    let silent_drop = classify(Probe::NothingAnswered, Some(&no_session), true, None);
    assert!(silent_drop.connected);
    assert!(!silent_drop.internet);
    assert!(silent_drop.portal_answers());
    assert!(silent_drop.login_possible());
    assert!(!silent_drop.os_owns_session());

    // Nothing intercepted and the portal answered: ours, but the link routes.
    // Behind a VPN or a router this is the ordinary case.
    let routed = classify(Probe::Direct, Some(&answered), true, None);
    assert!(routed.internet);
    assert!(routed.portal_answers());
    assert!(
      !routed.login_possible(),
      "a login here would vouch for the gateway"
    );

    // The portal says the session was opened by the 802.1X service: the verbs
    // are not ours here, whatever the local Wi-Fi is called.
    let os_owned = classify(
      Probe::Direct,
      Some(&answered),
      true,
      Some("tsinghua-secure_new"),
    );
    assert!(os_owned.os_owns_session());
    assert!(!os_owned.login_possible());
    assert!(!os_owned.logout_possible(State::LoggedIn));
    assert!(!os_owned.account_form_applies());

    // Nothing intercepted and the portal stayed silent: home Wi-Fi, a hotspot.
    let home = classify(Probe::Direct, None, true, None);
    assert!(home.internet);
    assert_eq!(home.portal, Portal::Silent);

    // No route at all: nothing to ask, and the portal could not have answered.
    let unplugged = classify(Probe::NothingAnswered, Some(&no_session), false, None);
    assert!(!unplugged.connected);
    assert!(!unplugged.internet);
    assert!(!unplugged.portal_answers());

    // The interception carries the access controller, which is the only
    // evidence a login can be addressed to.
    assert_eq!(
      classify(
        Probe::Intercepted {
          ac_id: "169".into()
        },
        None,
        true,
        None
      )
      .portal,
      Portal::Ours {
        ac_id: Some("169".into())
      }
    );
    // Another operator's portal: not ours to ask, whatever else answered.
    assert_eq!(
      classify(Probe::ForeignPortal, Some(&answered), true, None).portal,
      Portal::Foreign
    );
  }

  /// The portal names the service behind a session; that is the only evidence
  /// this app uses for "the OS owns it", and it has to survive a rename.
  #[test]
  fn the_os_owned_session_comes_from_the_portal_domain() {
    assert!(crate::srun::is_os_service("tsinghua-secure_new"));
    assert!(crate::srun::is_os_service("Tsinghua-Secure"));
    assert!(crate::srun::is_os_service("tsinghua-secure-2"));
    assert!(!crate::srun::is_os_service("default"));
    assert!(!crate::srun::is_os_service("tsinghua"));
    assert!(!crate::srun::is_os_service("tsinghua-ipv4_new"));
    assert!(!crate::srun::is_os_service(""));
  }

  /// A link with no campus portal behind it has no answer to wait for, so the
  /// rows it gets carry the address the machine holds and nothing else - no
  /// account, no traffic, and no guess about a session.
  #[test]
  fn a_link_without_a_portal_shows_its_own_address_only() {
    // Documentation addresses (RFC 5737 / RFC 3849).
    let v4: std::net::IpAddr = "192.0.2.10".parse().unwrap();
    let v6: std::net::IpAddr = "2001:db8::7".parse().unwrap();
    let rows = rows_without_session((Some(v4), Some(v6)));
    assert_eq!(rows.ip4, Some(v4));
    assert_eq!(rows.ip6, Some(v6));
    assert!(rows.name.is_empty());
    assert_eq!(rows.bytes_in, 0);
    assert_eq!(rows.devices_total, 0);
    // A link with an address of one family only says just that.
    let rows = rows_without_session((None, Some(v6)));
    assert_eq!(rows.ip4, None);
    assert_eq!(rows.ip6, Some(v6));
  }

  #[test]
  fn status_error_text_stays_one_short_line() {
    // The rustls form: a sentence plus a docs URL that used to be written out
    // verbatim on every poll.
    let rustls = "io: peer closed connection without sending TLS close_notify: \
                  https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof";
    assert_eq!(
      status_error_text(rustls),
      "status: the portal closed the connection without close_notify"
    );

    // Anything else keeps its own words, minus any URL.
    assert_eq!(
      status_error_text("io: something https://example.com/a/b"),
      "status: io: something"
    );
    assert_eq!(
      status_error_text("io: timeout: global"),
      "status: io: timeout: global"
    );
  }

  #[test]
  fn a_finished_request_comes_back() {
    let link_gen = AtomicU64::new(0);
    assert_eq!(await_request(&link_gen, || 7u32), Ok(7));
  }

  #[test]
  fn a_request_is_abandoned_when_the_link_moves() {
    let link_gen = Arc::new(AtomicU64::new(0));
    let bumper = link_gen.clone();
    std::thread::spawn(move || {
      std::thread::sleep(Duration::from_millis(150));
      bumper.fetch_add(1, Ordering::Release);
    });
    let started = Instant::now();
    let result = await_request(&link_gen, || {
      std::thread::sleep(Duration::from_secs(2));
      7u32
    });
    assert_eq!(result, Err(()), "the answer belongs to the old link");
    assert!(
      started.elapsed() < Duration::from_secs(1),
      "the waiter must not wait out the request"
    );
  }
}
