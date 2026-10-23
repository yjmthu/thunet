//! The background core: session state machine, network probing, and the event
//! stream the frontend renders.
//!
//! This is the loop that used to live inside the window process. It talks to
//! the outside world through [`crate::ipc`]: commands arrive over a channel,
//! every event is published to whichever frontends are attached. It never
//! touches a window, a tray icon or a display server, so the same code runs at
//! boot (`thunet core` from a systemd system unit or the Windows service) with
//! nobody logged in.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::i18n::Lang;
use crate::ipc::Sink;
use crate::logline::Line;
use crate::proto::{Cmd, Ev, Failure, LoginEnv, ToastKind};
use crate::session::{Session, State};
use crate::srun::{NetCause, NetEnv, NetError};

/// How often the network is re-probed. Without a route every probe waits out
/// its timeout, so that case backs off much further; a route that answers
/// nothing keeps the normal cadence, because DNS and the portal can lag behind
/// the link at boot.
const ENV_PROBE_MS: u64 = 15_000;
const ENV_PROBE_OFFLINE_MS: u64 = 60_000;

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

/// Windows names a freshly joined network's profile "Identifying…" for a moment
/// after the address appears, so the probe that runs on the link change can
/// read that placeholder instead of the SSID and leave the enterprise verdict
/// unset for as long as the session lasts. While online only the name is
/// re-read (a local call, no request), and only while it is still unknown - the
/// first answer that has a name in it ends the watch.
const SSID_WATCH_AFTER_LINK: Duration = Duration::from_secs(90);
const SSID_WATCH_STEP: Duration = Duration::from_secs(2);
const SSID_WATCH_MAX_STEP: Duration = Duration::from_secs(20);

/// Do the portal verbs (Connect, Disconnect, a credential change) mean anything
/// on this network? On 802.1X Wi-Fi the OS owns the session and the portal
/// entry is only a mirror of it: a logout there would end something this app
/// never opened, and a login has nothing to prove. The window greys those verbs
/// out; this is the same rule for a request that arrives over the pipe.
fn portal_verbs(env: &LoginEnv) -> bool {
  !matches!(env, LoginEnv::EnterpriseWifi(_))
}

/// One network probe.
struct EnvProbe {
  env: LoginEnv,
  ac_id: Option<String>,
  /// Why the SSID could not be read, when it could not, so the log can say that
  /// the 802.1X shortcut was skipped instead of silently not applying.
  ssid_note: Option<String>,
}

/// Probe what the machine is attached to, plus the `ac_id` when a campus portal
/// answered. The SSID check comes first: on an 802.1X network no portal request
/// is worth making.
fn probe_env(session: &Session) -> EnvProbe {
  let wifi = crate::wifi::classify();
  // Reported once per change: it is what makes "no 802.1X shortcut" visible,
  // whether Windows refused to name the SSID or the name simply is not one of
  // the enterprise networks.
  let ssid_note = match &wifi {
    crate::wifi::WifiKind::Enterprise(ssid) => {
      Some(format!("wifi: {ssid} authenticates below the portal"))
    }
    crate::wifi::WifiKind::Campus(ssid) => Some(format!("wifi: {ssid} is a campus network")),
    crate::wifi::WifiKind::Portal(ssid) => Some(format!("wifi: {ssid} uses the portal")),
    // Nothing wireless is connected: there is no verdict to explain, so this
    // stays quiet instead of claiming "no ssid" on every wired machine.
    crate::wifi::WifiKind::NoWifi => None,
    crate::wifi::WifiKind::Unknown(why) => Some(format!("wifi: no ssid ({why})")),
  };
  if let crate::wifi::WifiKind::Enterprise(ssid) = wifi {
    return EnvProbe {
      env: LoginEnv::EnterpriseWifi(ssid),
      ac_id: None,
      ssid_note,
    };
  }
  let named_campus = matches!(&wifi, crate::wifi::WifiKind::Campus(_));
  // The silent-drop case needs a second look: the campus gateway drops
  // pre-login traffic on the wired network instead of redirecting it, so the
  // probes see "nothing answers" while a portal login is exactly what is
  // missing. Two local facts decide it: the SSID name, and the campus portal
  // already talking to this machine's own address (true only while no NAT sits
  // in between).
  //
  // A link that *does* reach the internet is deliberately not upgraded: there a
  // login is not needed, and the `ac_id` of the campus network is per link, so
  // the one this machine has may belong to the network it came from - the
  // portal then answers `BAS respond timeout` or `E2808` and nothing has been
  // gained.
  let campus_link = |session: &Session| named_campus || session.portal_owns_address();
  let (env, ac_id) = match session.net_env() {
    NetEnv::Campus { ac_id } => (LoginEnv::Campus, Some(ac_id)),
    NetEnv::OtherPortal => (LoginEnv::OtherPortal, None),
    NetEnv::DirectInternet => (LoginEnv::NoPortal, None),
    NetEnv::Unreachable => (
      if campus_link(session) && crate::srun::link_up() {
        LoginEnv::Campus
      } else {
        LoginEnv::NoNetwork
      },
      None,
    ),
  };
  EnvProbe {
    env,
    ac_id,
    ssid_note,
  }
}

/// The core's own state, shared by the CLI dump and the window's service row.
fn core_info(
  started: Instant,
  session: &Session,
  login_env: &LoginEnv,
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
    env: login_env.clone(),
    ac_id: session.cfg().ac_id.clone(),
    host: session.cfg().host.clone(),
    // Read here rather than in the frontend so `thunet status` describes the
    // same link the core's own requests leave on.
    link: crate::srun::link_kind(),
    credential,
    auto_connect: session.cfg().auto_connect,
    keep_online: session.cfg().keep_online,
    wanted_online,
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
  match session.ensure_online() {
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

pub fn worker(cfg: Config, rx: Receiver<Cmd>, tx: Sink) {
  let started = Instant::now();
  let mut lang = Lang::from_setting(&cfg.lang);
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
  let mut last_state = State::Offline;
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
  // Link watch: `link_up()` costs a route lookup, so it can run every second.
  // A rising edge (network just became usable) retries immediately instead of
  // waiting out a backoff, which makes login start ~1 s after Wi-Fi connects.
  let mut next_link_check = Instant::now();
  let mut link_ready = false;
  // What the last network probe saw, and when the next one is due. The attempt
  // below reuses this verdict instead of probing again.
  let mut last_env = LoginEnv::Campus;
  let mut next_env_probe = Instant::now();
  // The path to the internet the last probe ran on. When it changes - a cable
  // pulled, Wi-Fi joined, a new address - the verdict describes a network the
  // machine has already left, so it is re-probed even while the session is up.
  let mut last_link: Option<std::net::IpAddr> = None;
  let mut force_env = false;
  // The last thing the log said about reading the SSID, so a read that Windows
  // refuses is reported once instead of on every probe.
  let mut last_ssid_note: Option<String> = None;
  // After the path to the internet changes, the SSID is re-read for a while:
  // see [`SSID_WATCH_AFTER_LINK`].
  let mut ssid_watch_until = Instant::now();
  let mut next_ssid_watch = Instant::now();
  let mut ssid_watch_step = SSID_WATCH_STEP;

  for w in session.take_warnings() {
    tx.send(Ev::Log(w)).ok();
  }
  tx.send(Ev::Log(Line::debug(format!("lang: start {lang:?}"))))
    .ok();

  loop {
    let t = lang.texts();
    while let Ok(c) = rx.try_recv() {
      match c {
        Cmd::Connect => {
          // A manual connect is the user's intent whatever state the machine is
          // in, and it is what lifts a rejected password.
          want_online = true;
          // Check the current state first so an existing session is not re-authenticated.
          if session.is_online_now() {
            if let Ok(v) = session.refresh() {
              tx.send(Ev::Info(v)).ok();
            }
            tx.send(Ev::State(session.state)).ok();
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
          // The session on an 802.1X network belongs to the OS; ending the
          // portal's copy of it is a change this app has no business making,
          // whether the request comes from the window or from `thunet logout`.
          if !portal_verbs(&last_env) {
            tx.send(Ev::Log(Line::warn(
              "refused: the portal session here is not managed by this app",
            )))
            .ok();
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
            if !session.is_online_now() {
              if let Ok(v) = session.refresh() {
                tx.send(Ev::Info(v)).ok();
              }
              tx.send(Ev::State(session.state)).ok();
              tx.send(Ev::Log(Line::info("not connected"))).ok();
              tx.send(Ev::Notify {
                msg: t.not_connected.into(),
                kind: ToastKind::Info,
              })
              .ok();
            } else {
              match session.logout() {
                Ok(m) => {
                  tx.send(Ev::Log(Line::info(format!("logged out: {m}"))))
                    .ok();
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
            }
          }
        }
        Cmd::Refresh { quiet } => match session.refresh() {
          Ok(v) => {
            refresh_failures = 0;
            tx.send(Ev::Info(v)).ok();
            tx.send(Ev::State(session.state)).ok();
            // A refresh proves the portal answers, not that the stored
            // password is right: a rejection stays on screen through it,
            // unless the session turns out to be online.
            if session.state == State::Online
              || !matches!(
                banner.as_ref(),
                Some(Failure::Credentials | Failure::AcUnreachable)
              )
            {
              set_banner(&tx, &mut banner, None);
            }
            if session.state == State::Online {
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
          Err(e) => {
            refresh_failures = refresh_failures.saturating_add(1);
            // The technical sentence (a TLS or DNS message) is far too long for
            // a band, so the log keeps it. One failed refresh is not news for
            // the page either - the next one clears it - so only a failure that
            // repeats raises the banner; the toast answers the button press.
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
          }
        },
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
        Cmd::SetCredentials { .. } if !portal_verbs(&last_env) => {
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
          if ready && new_pair && session.state == State::Online {
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
                want_online = session.state != State::Online;
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
                let back_online = session.state == State::Online;
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
            if ready && session.state != State::Online {
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
            if want_online && session.state != State::Online {
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
        Cmd::QuitAll => {
          // One request, every attached window - across sessions and users,
          // which is why it goes through the fan-out instead of the
          // per-session doorbell. The core itself keeps running: the installer
          // stops the service on its own schedule.
          tx.send(Ev::Log(Line::info(format!(
            "asked {} attached window(s) to quit",
            tx.frontends()
          ))))
          .ok();
          tx.send(Ev::Quit).ok();
        }
      }
    }

    // A login attempt without credentials can only be rejected, so say what is
    // missing instead of POSTing an empty password to the portal. The attempt
    // below is gated on the same flag, so nothing can reach the portal either
    // way; this branch only decides what the user is told.
    let missing_creds = session.cfg().username.is_empty() || session.cfg().password.is_empty();
    if want_online && session.state != State::Online && missing_creds {
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
    if Instant::now() >= next_link_check {
      next_link_check = Instant::now() + Duration::from_secs(1);
      let id = crate::srun::link_id();
      if id != last_link {
        last_link = id;
        // A different path to the internet: the verdict, the backoff in flight
        // and the reason last logged all belong to the old one. The SSID window
        // is *replaced*, never extended - a switch supersedes the watch started
        // for the previous network, so a leftover window can never decide when
        // the new one is read.
        force_env = true;
        ssid_watch_until = Instant::now() + SSID_WATCH_AFTER_LINK;
        ssid_watch_step = SSID_WATCH_STEP;
        next_ssid_watch = Instant::now() + ssid_watch_step;
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
      }
      let up = id.is_some();
      if up && !link_ready {
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
        next_env_probe = Instant::now();
      }
      if !up {
        // No route at all: there is no name to read. Drop the window; the next
        // link change opens a fresh one.
        ssid_watch_until = Instant::now();
      }
      link_ready = up;
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

    // While online, re-read only the name, and only while the last read could
    // not name the network. The full probe stays off while a session is up (it
    // costs three requests), but a name is local, and it is what decides the
    // enterprise verdict - right after joining, Windows may still be calling
    // the network "Identifying…".
    if session.state == State::Online
      && !force_env
      && Instant::now() < ssid_watch_until
      && Instant::now() >= next_ssid_watch
    {
      let kind = crate::wifi::classify();
      if matches!(kind, crate::wifi::WifiKind::Enterprise(_))
        == !matches!(last_env, LoginEnv::EnterpriseWifi(_))
      {
        // The name appeared (or the machine left that network): let the full
        // probe decide, and say so in the log. That is the last look needed.
        force_env = true;
        ssid_watch_until = Instant::now();
      } else if matches!(kind, crate::wifi::WifiKind::Unknown(_)) {
        // Still "Identifying…": back off instead of hammering a read that is
        // waiting for Windows to finish naming the network.
        ssid_watch_step = (ssid_watch_step * 2).min(SSID_WATCH_MAX_STEP);
        next_ssid_watch = Instant::now() + ssid_watch_step;
      } else {
        // A name that exists is the name this network keeps: stop asking.
        ssid_watch_until = Instant::now();
      }
    }

    // Probe the network on a timer while offline, and in the same pass the path
    // to the internet changed - online included, because the verdict is what
    // the window shows and it may not describe the old network.
    if force_env || (session.state != State::Online && Instant::now() >= next_env_probe) {
      force_env = false;
      let probe = probe_env(&session);
      let env = probe.env;
      if let Some(ac_id) = probe.ac_id {
        session.set_ac_id(ac_id);
      }
      if let Some(note) = &probe.ssid_note
        && last_ssid_note.as_ref() != Some(note)
      {
        tx.send(Ev::Log(Line::debug(note.clone()))).ok();
        last_ssid_note = probe.ssid_note;
      }
      // The slow cadence is only for a machine with no route at all.
      // `link_ready` is maintained by the link watch, and that only runs while
      // an automatic attempt is possible; ask the route table directly for the
      // rest (auto connect off, no saved password), so the verdict still
      // refreshes when the cable or Wi-Fi comes back.
      let route = link_ready || crate::srun::link_up();
      let wait = if env == LoginEnv::NoNetwork && !route {
        ENV_PROBE_OFFLINE_MS
      } else {
        ENV_PROBE_MS
      };
      next_env_probe = Instant::now() + Duration::from_millis(wait);
      if env != last_env {
        tx.send(Ev::Log(Line::debug(format!("network: {env:?}"))))
          .ok();
        // The verdict changed (no link → campus, or back), so decide again in
        // this pass instead of sitting out a backoff chosen for the old one.
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
      }
      last_env = env.clone();
      tx.send(Ev::Env(env)).ok();
    }

    // Whether an automatic attempt may be made at all. `keep_online` is the
    // standing permission for it; without it, only an open connect window
    // allows one, which is what makes "connect at start" mean exactly that.
    let auto_attempts = want_online && (session.cfg().keep_online || !connect_budget.is_zero());
    if !missing_creds
      && auto_attempts
      && session.state != State::Online
      && link_ready
      && Instant::now() >= next_attempt
    {
      // Never spray login attempts at a network where logging in cannot work:
      // enterprise Wi-Fi (802.1X), no cable/Wi-Fi, a network that already
      // reaches the internet, or some other operator's captive portal. The
      // probe above already decided which case this is.
      let mut skip: Option<(String, String, Duration, ToastKind)> = None;
      match last_env.clone() {
        LoginEnv::Campus => {}
        LoginEnv::EnterpriseWifi(ssid) => {
          let text = t.secure_wifi.replacen("{}", &ssid, 1);
          let log = format!("skipped: {ssid} authenticates at the OS level (802.1X)");
          skip = Some((text, log, Duration::from_secs(60), ToastKind::Info));
        }
        LoginEnv::NoNetwork => {
          skip = Some((
            t.network_unavailable.to_string(),
            "skipped: no network connection".to_string(),
            Duration::from_secs(30),
            ToastKind::Info,
          ))
        }
        LoginEnv::OtherPortal => {
          skip = Some((
            t.not_campus_network.to_string(),
            "skipped: this is not the campus portal".to_string(),
            Duration::from_secs(60),
            ToastKind::Info,
          ))
        }
        LoginEnv::NoPortal => {
          // The internet works without interception. Either the campus session
          // is already authenticated, or this is a network that has no portal
          // at all (another operator), in which case logging in is wrong.
          let online = session
            .refresh()
            .map(|_| session.state)
            .unwrap_or(State::Offline);
          if online != State::Online {
            skip = Some((
              t.no_login_needed.to_string(),
              "skipped: internet is reachable without a portal login".to_string(),
              Duration::from_secs(60),
              ToastKind::Info,
            ));
          }
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
        tx.send(Ev::State(session.state)).ok();
        continue;
      }

      match session.ensure_online() {
        Ok(m) => {
          failures = 0;
          last_failure.clear();
          user_connect = false;
          // Connected: the connect window has done its job. Whether the core
          // keeps it that way is `keep_online`'s decision from here on.
          connect_budget = Duration::ZERO;
          set_banner(&tx, &mut banner, None);
          tx.send(Ev::Log(Line::info(m))).ok();
          tx.send(Ev::Notify {
            msg: t.connect_ok.into(),
            kind: ToastKind::Ok,
          })
          .ok();
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
            if !bad_credentials && !ac_unreachable {
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

    // Ask the portal what is actually going on, online or not: the session may
    // have been created elsewhere, and "no password saved" is not "offline".
    let poll_every = if session.state == State::Online {
      15
    } else {
      10
    };
    if last_status.elapsed() >= Duration::from_secs(poll_every) {
      if let Ok(v) = session.client().status(&session.cfg().ac_id) {
        let online = v.get("online").and_then(|b| b.as_bool()).unwrap_or(false);
        if online && session.state != State::Online {
          session.state = State::Online;
          failures = 0;
          // Being online is what the connect window was for: it is done. A
          // session found here may have been opened elsewhere, which is still
          // "online" as far as the machine is concerned.
          connect_budget = Duration::ZERO;
          set_banner(&tx, &mut banner, None);
          tx.send(Ev::Log(Line::info("already online"))).ok();
        } else if !online && session.state == State::Online {
          // Session ended outside our control (timeout / kicked / unplugged).
          session.state = State::Offline;
          tx.send(Ev::Log(Line::warn("session ended, now offline")))
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
        tx.send(Ev::Info(v)).ok();
      }
      last_status = Instant::now();
    }

    if session.state != last_state {
      tx.send(Ev::State(session.state)).ok();
      tx.send(Ev::Log(Line::debug(format!("state: {:?}", session.state))))
        .ok();
      last_state = session.state;
    }

    for w in session.take_warnings() {
      tx.send(Ev::Log(w)).ok();
    }

    std::thread::sleep(Duration::from_millis(400));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The window greys the portal verbs out on 802.1X; the pipe refuses them for
  /// the same reason, so `thunet logout` cannot end a session the OS owns.
  #[test]
  fn portal_verbs_are_off_on_enterprise_wifi() {
    assert!(portal_verbs(&LoginEnv::Campus));
    // A campus link that already reaches the internet is still a portal link:
    // Disconnect has a real session to end there.
    assert!(portal_verbs(&LoginEnv::NoPortal));
    assert!(portal_verbs(&LoginEnv::OtherPortal));
    assert!(portal_verbs(&LoginEnv::NoNetwork));
    assert!(!portal_verbs(&LoginEnv::EnterpriseWifi(
      "Tsinghua-Secure".into()
    )));
    // The duplicate-name form Windows produces is the same network.
    assert!(!portal_verbs(&LoginEnv::EnterpriseWifi(
      "Tsinghua-Secure 2".into()
    )));
  }
}
