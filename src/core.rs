//! The background core: session state machine, keepalive, and the event stream
//! the frontend renders.
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
/// on this network? On 802.1X Wi-Fi Windows owns the session and the portal
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
  let (env, ac_id) = match session.net_env() {
    NetEnv::Campus { ac_id } => (LoginEnv::Campus, Some(ac_id)),
    NetEnv::Unreachable => (LoginEnv::NoNetwork, None),
    NetEnv::OtherPortal => (LoginEnv::OtherPortal, None),
    NetEnv::DirectInternet => (LoginEnv::NoPortal, None),
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
    credential,
    auto_connect: session.cfg().auto_connect,
    wanted_online,
    failures: session.failures,
    frontends,
    endpoint: crate::ipc::endpoint_path(),
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
  let keepalive = Duration::from_secs(cfg.keepalive_interval_secs.max(30));
  let mut lang = Lang::from_setting(&cfg.lang);
  // `load_machine` only fills `password` when `remember_password` is set and a
  // sealed blob could be opened, so this pair is exactly "the next start will
  // find a credential".
  let mut remembered = cfg.remember_password && !cfg.password.is_empty();
  let mut session = Session::new(cfg);
  let has_creds = !session.cfg().username.is_empty() && !session.cfg().password.is_empty();
  let mut want_online = session.cfg().auto_connect && has_creds;
  if session.cfg().auto_connect && !has_creds {
    tx.send(Ev::Log(Line::warn(
      "auto connect is on, but no account and password are saved",
    )))
    .ok();
  }
  let mut last_ka = Instant::now();
  let mut last_status = Instant::now() - Duration::from_secs(60);
  let mut last_state = State::Offline;
  // Backoff state so a rejected login does not hammer the portal every tick.
  let retry_delay = Duration::from_secs(session.cfg().reconnect_delay_secs.max(5));
  let mut next_attempt = Instant::now();
  let mut failures = 0u32;
  let mut last_failure = String::new();
  // Set when the user pressed "Connect", so a skipped attempt can be reported.
  let mut user_connect = false;
  // A rejected password is deterministic: the same pair can only be refused
  // again, and retrying it hammers the portal (and may trip its lockout). So
  // automatic attempts stop until the pair changes, the user presses Connect,
  // a login succeeds, or the core restarts (a password fixed on the portal's
  // own page then gets one more try).
  let mut rejected = false;
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
          // Check the current state first so an existing session is not re-authenticated.
          if session.is_online_now() {
            if let Ok(v) = session.refresh() {
              tx.send(Ev::Info(v)).ok();
            }
            tx.send(Ev::State(session.state)).ok();
            rejected = false;
            set_banner(&tx, &mut banner, None);
            tx.send(Ev::Log(Line::info("already online"))).ok();
            tx.send(Ev::Notify {
              msg: t.already_online.into(),
              kind: ToastKind::Info,
            })
            .ok();
          } else {
            want_online = true;
            user_connect = true;
            // An explicit Connect is the user overriding the pause.
            rejected = false;
            next_attempt = Instant::now();
            failures = 0;
            last_failure.clear();
            last_status = Instant::now() - Duration::from_secs(60);
          }
        }
        Cmd::Disconnect => {
          // The session on an 802.1X network belongs to Windows; ending the
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
          } else if !session.is_online_now() {
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
            want_online = false;
          }
        }
        Cmd::Refresh { quiet } => match session.refresh() {
          Ok(v) => {
            tx.send(Ev::Info(v)).ok();
            tx.send(Ev::State(session.state)).ok();
            // A refresh proves the portal answers, not that the stored
            // password is right: a rejection stays on screen (and stays
            // latched) through it, unless the session turns out to be online.
            if session.state == State::Online {
              rejected = false;
            }
            if session.state == State::Online
              || !matches!(banner.as_ref(), Some(Failure::Credentials))
            {
              set_banner(&tx, &mut banner, None);
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
            // The banner is the point of even a quiet refresh; only the toast
            // is skipped.
            set_banner(&tx, &mut banner, Some(failure_of(t.refresh_failed, &e)));
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
        } => {
          let dir = crate::config::dir();
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
                rejected = false;
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
                rejected = false;
                want_online = session.state != State::Online;
                failures = 0;
                last_failure.clear();
                next_attempt = Instant::now();
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
              rejected = false;
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
            // chosen for the old one. `auto_connect` only says whether the app
            // logs in by itself at start-up, so it does not gate an edit; a pair
            // that was just refused is not tried again either.
            if ready && session.state != State::Online && !rejected {
              want_online = true;
              next_attempt = Instant::now();
              failures = 0;
              last_failure.clear();
              last_status = Instant::now() - Duration::from_secs(60);
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
          want_online = on;
          if on && session.state != State::Online {
            next_attempt = Instant::now();
            last_status = Instant::now() - Duration::from_secs(60);
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
          } else {
            // A portable core belongs to the user who started it; exiting here
            // is the same as `systemctl --user stop` / closing its terminal.
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

    if !missing_creds
      && want_online
      && session.state != State::Online
      && !rejected
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
          rejected = false;
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
          // 5s, 10s, 20s, 40s, ... capped at two minutes.
          let backoff = retry_delay.as_secs_f64() * 2f64.powi((failures - 1).min(5) as i32);
          next_attempt = Instant::now() + Duration::from_secs_f64(backoff.min(120.0));
          let bad_credentials = session.failure == crate::session::FailureKind::Credentials;
          // A rejected password stops the automatic attempts: the same pair
          // can only be refused again. Only a new pair, a manual Connect or a
          // restart lifts that.
          rejected = bad_credentials;
          user_connect = false;
          // A credential rejection carries a long JSON blob that only helps
          // when debugging, so it goes to the log pane while the red banner
          // says what the user can act on. The banner is worded by the
          // frontend, so a language switch re-words it, and it stays up until
          // a login succeeds.
          set_banner(
            &tx,
            &mut banner,
            Some(if bad_credentials {
              Failure::Credentials
            } else {
              // The frontend shows this verbatim, so the words have to be here:
              // a refresh or disconnect failure already says what it was.
              failure_of(t.login_failed, &e)
            }),
          );
          tx.send(Ev::Log(Line::error(format!(
            "{e} (retry in {}s)",
            backoff.min(120.0) as u64
          ))))
          .ok();
          // Only buzz once per distinct reason, otherwise retries spam toasts.
          // A rejected password is not a toast: the banner already says it and
          // stays there, while a toast would flash and vanish.
          let detail = e.to_string();
          if detail != last_failure {
            last_failure = detail;
            if !bad_credentials {
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
          // Being online leaves the warning with nothing to act on; if the
          // session drops later, one attempt re-detects a rejection.
          rejected = false;
          set_banner(&tx, &mut banner, None);
          tx.send(Ev::Log(Line::info("already online"))).ok();
        } else if !online && session.state == State::Online {
          // Session ended outside our control (timeout / kicked / unplugged).
          session.state = State::Offline;
          tx.send(Ev::Log(Line::warn("session ended, now offline")))
            .ok();
        } else if online && matches!(banner.as_ref(), Some(Failure::Credentials)) {
          // A credential banner is only true while the machine cannot sign in.
          // This session proves the stored pair works, so drop it even though
          // the state did not change: that is the case after a refused switch
          // that signed the previous account back in.
          set_banner(&tx, &mut banner, None);
        }
        tx.send(Ev::Info(v)).ok();
      }
      last_status = Instant::now();
    }

    if session.state == State::Online && last_ka.elapsed() >= keepalive {
      // Without credentials there is nothing a failing keepalive could trigger
      // but an empty-password login, so only report the state poll above. A
      // latched rejection stays latched here too, otherwise a dropped session
      // would quietly retry the refused pair behind the user's back.
      if session.keepalive().is_err() && !missing_creds && !rejected {
        let _ = session.ensure_online();
      }
      last_ka = Instant::now();
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
