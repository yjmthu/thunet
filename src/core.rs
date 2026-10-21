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
use crate::proto::{Cmd, Ev, LoginEnv, ToastKind};
use crate::session::{Session, State};
use crate::srun::NetEnv;

/// How often the network is re-probed. Without a route every probe waits out
/// its timeout, so that case backs off much further; a route that answers
/// nothing keeps the normal cadence, because DNS and the portal can lag behind
/// the link at boot.
const ENV_PROBE_MS: u64 = 15_000;
const ENV_PROBE_OFFLINE_MS: u64 = 60_000;

/// Probe what the machine is attached to, plus the `ac_id` when a campus portal
/// answered. The SSID check comes first: on an 802.1X network no portal request
/// is worth making.
fn probe_env(session: &Session) -> (LoginEnv, Option<String>) {
  if let Some(ssid) = crate::wifi::no_portal_wifi() {
    return (LoginEnv::EnterpriseWifi(ssid), None);
  }
  match session.net_env() {
    NetEnv::Campus { ac_id } => (LoginEnv::Campus, Some(ac_id)),
    NetEnv::Unreachable => (LoginEnv::NoNetwork, None),
    NetEnv::OtherPortal => (LoginEnv::OtherPortal, None),
    NetEnv::DirectInternet => (LoginEnv::NoPortal, None),
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
  revision: u64,
  matches: Option<bool>,
) -> crate::proto::CredentialState {
  crate::proto::CredentialState {
    username: session.cfg().username.clone(),
    has_password: !session.cfg().password.is_empty(),
    remembered,
    revision,
    matches,
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
  let mut credential_rev = u64::from(remembered);
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
  // Link watch: `link_up()` costs a route lookup, so it can run every second.
  // A rising edge (network just became usable) retries immediately instead of
  // waiting out a backoff, which makes login start ~1 s after Wi-Fi connects.
  let mut next_link_check = Instant::now();
  let mut link_ready = false;
  // What the last network probe saw, and when the next one is due. The attempt
  // below reuses this verdict instead of probing again.
  let mut last_env = LoginEnv::Campus;
  let mut next_env_probe = Instant::now();

  for w in session.take_warnings() {
    tx.send(Ev::Log(w)).ok();
  }

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
            tx.send(Ev::Error(None)).ok();
            tx.send(Ev::Log(Line::info("already online"))).ok();
            tx.send(Ev::Notify {
              msg: t.already_online.into(),
              kind: ToastKind::Info,
            })
            .ok();
          } else {
            want_online = true;
            user_connect = true;
            next_attempt = Instant::now();
            failures = 0;
            last_failure.clear();
            last_status = Instant::now() - Duration::from_secs(60);
          }
        }
        Cmd::Disconnect => {
          // Check the current state first so no useless logout request is sent.
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
                tx.send(Ev::Error(Some(format!("{}: {e}", t.disconnect_failed))))
                  .ok();
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
        Cmd::Refresh => match session.refresh() {
          Ok(v) => {
            tx.send(Ev::Info(v)).ok();
            tx.send(Ev::State(session.state)).ok();
            tx.send(Ev::Error(None)).ok();
            tx.send(Ev::Log(Line::info("status refreshed"))).ok();
            tx.send(Ev::Notify {
              msg: t.status_refreshed.into(),
              kind: ToastKind::Ok,
            })
            .ok();
          }
          Err(e) => {
            tx.send(Ev::Error(Some(format!("{}: {e}", t.refresh_failed))))
              .ok();
            tx.send(Ev::Notify {
              msg: t.refresh_failed.into(),
              kind: ToastKind::Err,
            })
            .ok();
          }
        },
        Cmd::SetLang(l) => {
          lang = l;
        }
        Cmd::SetCredentials {
          username,
          password,
          remember,
        } => {
          let before = (
            session.cfg().username.clone(),
            session.cfg().password.clone(),
            session.cfg().remember_password,
          );
          let ready = !username.is_empty() && !password.is_empty();
          if ready {
            session.set_credentials(username.clone(), password.clone());
          } else if !remember {
            // An empty form with "remember" still ticked is almost certainly
            // the window reopening without the password; do not wipe a stored
            // credential. Unticking the box is the explicit clear.
            session.set_credentials(String::new(), String::new());
          }
          // The sealed blob alone is not enough: the core also has to record
          // that it wants it read back. Without `remember_password` in its own
          // `config.json` a restarted core ignores the blob and comes up with
          // no credential at all (the window would then show an empty, unsaved
          // password even though Apply had worked).
          session.cfg_mut().remember_password = remember;
          let dir = crate::config::dir();
          match session.cfg().save_to(&dir) {
            Ok(()) => remembered = remember && !session.cfg().password.is_empty(),
            Err(e) => {
              tx.send(Ev::Error(Some(format!("cannot save the credential: {e}"))))
                .ok();
            }
          }
          let after = (
            session.cfg().username.clone(),
            session.cfg().password.clone(),
            session.cfg().remember_password,
          );
          if before != after {
            credential_rev += 1;
          }
          // Answer even when the request was a no-op: the window uses the
          // reply to learn whether the value it typed is the stored one.
          tx.send(Ev::Credentials(credential_state(
            &session,
            remembered,
            credential_rev,
            None,
          )))
          .ok();
          if ready && session.cfg().auto_connect && session.state != State::Online {
            want_online = true;
            last_status = Instant::now() - Duration::from_secs(60);
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
            &session,
            remembered,
            credential_rev,
            matches,
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
            credential_state(&session, remembered, credential_rev, None),
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
            credential_state(&session, remembered, credential_rev, None),
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
    if !missing_creds
      && want_online
      && session.state != State::Online
      && Instant::now() >= next_link_check
    {
      next_link_check = Instant::now() + Duration::from_secs(1);
      let up = crate::srun::link_up();
      if up && !link_ready {
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
        next_env_probe = Instant::now();
      }
      link_ready = up;
    }

    // Probe the network on a timer, online or not: the answer is what greys out
    // *Connect* off-campus, and it must not be 15 s stale after a Wi-Fi change.
    if session.state != State::Online && Instant::now() >= next_env_probe {
      let (env, ac_id) = probe_env(&session);
      if let Some(ac_id) = ac_id {
        session.set_ac_id(ac_id);
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
          tx.send(Ev::Error(None)).ok();
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
          // A credential rejection carries a long JSON blob that only helps when
          // debugging, so it goes to the log pane while the red banner and the
          // toast carry the one thing the user can act on.
          tx.send(Ev::Error(if bad_credentials {
            None
          } else {
            Some(e.clone())
          }))
          .ok();
          tx.send(Ev::Log(Line::error(format!(
            "{e} (retry in {}s)",
            backoff.min(120.0) as u64
          ))))
          .ok();
          // Only buzz once per distinct reason, otherwise retries spam toasts.
          if e != last_failure {
            last_failure = e;
            tx.send(Ev::Notify {
              msg: if bad_credentials {
                t.invalid_credentials
              } else {
                t.connect_failed
              }
              .into(),
              kind: ToastKind::Err,
            })
            .ok();
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
          tx.send(Ev::Log(Line::info("already online"))).ok();
        } else if !online && session.state == State::Online {
          // Session ended outside our control (timeout / kicked / unplugged).
          session.state = State::Offline;
          tx.send(Ev::Log(Line::warn("session ended, now offline")))
            .ok();
        }
        tx.send(Ev::Info(v)).ok();
      }
      last_status = Instant::now();
    }

    if session.state == State::Online && last_ka.elapsed() >= keepalive {
      // Without credentials there is nothing a failing keepalive could trigger
      // but an empty-password login, so only report the state poll above.
      if session.keepalive().is_err() && !missing_creds {
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
