//! Transport between the background core and the window/tray frontend.
//!
//! One connection per frontend; the core fans every [`Ev`] out to all of them
//! and replays the tail of the log plus the current state to whoever connects
//! late. See [`crate::proto`] for the message shapes.
//!
//! The endpoint is per user:
//!
//! * Linux/BSD: `$XDG_RUNTIME_DIR/thunet/control.sock` (mode 0700 directory,
//!   so no other account can talk to it),
//! * Windows: named pipe `thunet-control-<user>`.
//!
//! `interprocess` maps those two onto one `Stream` that is `Read + Write`, so
//! nothing above this module is platform specific.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ConnectOptions, ListenerOptions, Name};

use crate::config::Config;
use crate::proto::{Cmd, Ev, Msg, VERSION, Wire};

/// How many recent events a frontend that connects late gets to see. Enough to
/// fill the log pane (which shows a handful of lines) without growing forever.
const HISTORY: usize = 200;

/// How long a frontend waits for a freshly spawned core to publish its socket.
const SPAWN_WAIT: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------- endpoint

#[cfg(unix)]
fn endpoint() -> String {
  use std::os::unix::fs::PermissionsExt as _;

  let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
    Some(base) => std::path::PathBuf::from(base).join("thunet"),
    None => std::env::temp_dir().join(format!("thunet-{}", user_tag())),
  };
  if std::fs::create_dir_all(&dir).is_ok() {
    // The socket inherits the directory's permissions: owner only.
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
  }
  dir.join("control.sock").to_string_lossy().into_owned()
}

#[cfg(windows)]
fn endpoint() -> String {
  // The pipe namespace is machine-wide, so the account name keeps two users
  // from meeting each other.
  format!("thunet-control-{}", user_tag())
}

/// Where frontends connect; shown by `thunet status`.
pub fn endpoint_path() -> String {
  endpoint()
}

#[cfg(unix)]
fn user_tag() -> String {
  std::env::var("USER")
    .or_else(|_| std::env::var("LOGNAME"))
    .unwrap_or_else(|_| "user".into())
}

#[cfg(windows)]
fn user_tag() -> String {
  std::env::var("USERNAME").unwrap_or_else(|_| "user".into())
}

#[cfg(unix)]
fn name(path: &str) -> std::io::Result<Name<'_>> {
  use interprocess::local_socket::GenericFilePath;
  path.to_fs_name::<GenericFilePath>()
}

#[cfg(windows)]
fn name(path: &str) -> std::io::Result<Name<'_>> {
  use interprocess::local_socket::GenericNamespaced;
  path.to_ns_name::<GenericNamespaced>()
}

// --------------------------------------------------------------------- hub

/// Fan-out of events from the single worker loop to every attached frontend.
#[derive(Clone)]
pub struct Hub(Arc<Mutex<Shared>>);

#[derive(Default)]
struct Shared {
  clients: Vec<Sender<Ev>>,
  /// Replayed to late connections: log lines and toasts, newest last.
  history: VecDeque<Ev>,
  state: Option<crate::session::State>,
  env: Option<crate::proto::LoginEnv>,
  info: Option<serde_json::Value>,
  error: Option<String>,
}

impl Hub {
  pub fn new() -> Self {
    Self(Arc::new(Mutex::new(Shared::default())))
  }

  pub fn publish(&self, ev: Ev) {
    let mut shared = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    match &ev {
      Ev::State(state) => shared.state = Some(*state),
      Ev::Env(env) => shared.env = Some(env.clone()),
      Ev::Info(info) => shared.info = Some(info.clone()),
      Ev::Error(err) => shared.error = err.clone(),
      // Diagnostics also go to the journal / `%APPDATA%` log via stderr; the
      // frontend only needs them for the log pane.
      Ev::Log(line) => eprintln!("{}", line.render()),
      // Replies to one-off requests belong to the client that asked; they are
      // not part of the state a late frontend needs.
      Ev::Notify { .. } | Ev::Status(_) | Ev::Core(_) | Ev::AcId(_) => {}
    }
    // Only log lines are replayed: a toast belongs to the action that raised
    // it, and replaying one to a fresh frontend would look like news.
    if matches!(ev, Ev::Log(_)) {
      shared.history.push_back(ev.clone());
      if shared.history.len() > HISTORY {
        shared.history.pop_front();
      }
    }
    shared.clients.retain(|tx| tx.send(ev.clone()).is_ok());
  }

  /// Register a new frontend and hand back what it missed.
  fn attach(&self) -> (Receiver<Ev>, Vec<Ev>) {
    let (tx, rx) = channel::<Ev>();
    let mut shared = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    let mut backlog = Vec::new();
    if let Some(state) = shared.state {
      backlog.push(Ev::State(state));
    }
    if let Some(env) = shared.env.clone() {
      backlog.push(Ev::Env(env));
    }
    if let Some(info) = shared.info.clone() {
      backlog.push(Ev::Info(info));
    }
    if let Some(err) = shared.error.clone() {
      backlog.push(Ev::Error(Some(err)));
    }
    backlog.extend(shared.history.iter().cloned());
    shared.clients.push(tx);
    (rx, backlog)
  }

  /// A `Sender<Ev>`-shaped handle for the worker loop.
  pub fn sink(&self) -> Sink {
    Sink(self.clone())
  }

  fn frontends(&self) -> usize {
    match self.0.lock() {
      Ok(shared) => shared.clients.len(),
      Err(poisoned) => poisoned.into_inner().clients.len(),
    }
  }
}

/// Anything with the same `send` shape as the `mpsc` sender the worker used
/// before the split, so the session loop itself did not have to change.
pub struct Sink(Hub);

impl Sink {
  pub fn send(&self, ev: Ev) -> Result<(), ()> {
    self.0.publish(ev);
    Ok(())
  }

  /// How many frontends are attached right now (for `thunet status`).
  pub fn frontends(&self) -> usize {
    self.0.frontends()
  }
}

// ------------------------------------------------------------------ server

/// Outcome of [`serve`].
pub enum Serve {
  Started,
  /// Another core already owns the endpoint; this process exits quietly.
  AlreadyRunning,
}

enum BindError {
  InUse,
  Other(String),
}

/// Run the core: bind the endpoint, then drive the session loop forever.
pub fn serve(cfg: Config) -> Result<Serve, String> {
  let path = endpoint();
  let listener = match bind(&path) {
    Ok(listener) => listener,
    Err(BindError::InUse) => return Ok(Serve::AlreadyRunning),
    Err(BindError::Other(e)) => return Err(e),
  };
  #[cfg(unix)]
  {
    // The directory is already 0700; this is belt and braces for a fallback
    // directory that happened to be world-readable.
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
  }

  let hub = Hub::new();
  let (cmd_tx, cmd_rx) = channel::<Cmd>();
  let worker_hub = hub.clone();
  std::thread::spawn(move || crate::core::worker(cfg, cmd_rx, worker_hub.sink()));

  // One accept loop for the life of the process; each connection gets its own
  // reader and writer thread so a stalled frontend cannot block the others.
  for stream in listener.incoming() {
    match stream {
      Ok(stream) => attach(stream, cmd_tx.clone(), hub.clone()),
      Err(_) => continue,
    }
  }
  Ok(Serve::Started)
}

fn bind(path: &str) -> Result<LocalSocketListener, BindError> {
  let create = || -> std::io::Result<LocalSocketListener> {
    let name = name(path)?;
    ListenerOptions::new().name(name).create_sync()
  };
  match create() {
    Ok(listener) => Ok(listener),
    Err(first) => {
      // A live core answers a probe; a leftover socket file does not.
      if connect(path).is_ok() {
        return Err(BindError::InUse);
      }
      #[cfg(unix)]
      let _ = std::fs::remove_file(path);
      create().map_err(|e| {
        BindError::Other(format!(
          "cannot listen on {path}: {e} (first attempt: {first})"
        ))
      })
    }
  }
}

fn connect(path: &str) -> Result<LocalSocketStream, String> {
  let name = name(path).map_err(|e| format!("bad endpoint {path}: {e}"))?;
  ConnectOptions::new()
    .name(name)
    .connect_sync()
    .map_err(|e| format!("{path}: {e}"))
}

fn attach(stream: LocalSocketStream, cmds: Sender<Cmd>, hub: Hub) {
  // Subscribe before writing the backlog, so an event published in between
  // arrives exactly once through the channel (the backlog is a snapshot).
  let (events, backlog) = hub.attach();
  hub.publish(Ev::Log(crate::logline::Line::debug("frontend connected")));
  let (recv_half, send_half) = stream.split();

  std::thread::spawn(move || {
    let mut out = BufWriter::new(send_half);
    for ev in backlog {
      if write_line(&mut out, &Wire::ev(ev)).is_err() {
        return;
      }
    }
    for ev in events {
      if write_line(&mut out, &Wire::ev(ev)).is_err() {
        return;
      }
    }
  });

  std::thread::spawn(move || {
    let mut lines = BufReader::new(recv_half).lines();
    while let Some(Ok(text)) = lines.next() {
      if text.trim().is_empty() {
        continue;
      }
      let Ok(wire) = Wire::parse(&text) else {
        continue;
      };
      match wire.msg {
        Msg::Cmd(cmd) if wire.v == VERSION => {
          if cmds.send(cmd).is_err() {
            return;
          }
        }
        Msg::Cmd(_) => {
          // Do not act on a message we may be reading with the wrong rules.
          hub.publish(Ev::Log(crate::logline::Line::error(format!(
            "frontend speaks protocol {} but the core speaks {VERSION}; upgrade both halves",
            wire.v
          ))));
          return;
        }
        // A core never receives events.
        Msg::Ev(_) => {}
      }
    }
  });
}

fn write_line(out: &mut impl Write, wire: &Wire) -> std::io::Result<()> {
  let line = wire.line().map_err(std::io::Error::other)?;
  out.write_all(line.as_bytes())?;
  out.flush()
}

// ------------------------------------------------------------------ client

/// A running core, as seen from the frontend.
pub struct Link {
  cmds: Sender<Cmd>,
  events: Receiver<Ev>,
  alive: Arc<AtomicBool>,
}

impl Link {
  /// `None` when no core is listening.
  pub fn connect() -> Option<Link> {
    let stream = connect(&endpoint()).ok()?;
    let (recv_half, send_half) = stream.split();
    let alive = Arc::new(AtomicBool::new(true));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (ev_tx, ev_rx) = channel::<Ev>();

    let writer_alive = alive.clone();
    std::thread::spawn(move || {
      let mut out = BufWriter::new(send_half);
      for cmd in cmd_rx {
        if write_line(&mut out, &Wire::cmd(cmd)).is_err() {
          break;
        }
      }
      writer_alive.store(false, Ordering::Relaxed);
    });

    let reader_alive = alive.clone();
    std::thread::spawn(move || {
      let mut lines = BufReader::new(recv_half).lines();
      while let Some(Ok(text)) = lines.next() {
        if text.trim().is_empty() {
          continue;
        }
        match Wire::parse(&text) {
          Ok(wire) if wire.v == VERSION => {
            if let Msg::Ev(ev) = wire.msg
              && ev_tx.send(ev).is_err()
            {
              break;
            }
          }
          Ok(wire) => {
            let _ = ev_tx.send(Ev::Log(crate::logline::Line::error(format!(
              "core speaks protocol {} but this build speaks {VERSION}",
              wire.v
            ))));
            break;
          }
          Err(e) => {
            let _ = ev_tx.send(Ev::Log(crate::logline::Line::warn(format!(
              "malformed message from the core: {e}"
            ))));
          }
        }
      }
      reader_alive.store(false, Ordering::Relaxed);
    });

    Some(Link {
      cmds: cmd_tx,
      events: ev_rx,
      alive,
    })
  }

  pub fn send(&self, cmd: Cmd) {
    let _ = self.cmds.send(cmd);
  }

  pub fn try_recv(&self) -> Option<Ev> {
    self.events.try_recv().ok()
  }

  /// `false` once either half of the connection is gone.
  pub fn alive(&self) -> bool {
    self.alive.load(Ordering::Relaxed)
  }
}

/// Try to reach the core without starting anything.
///
/// This is what a running frontend uses when reconnecting, so a core that was
/// stopped on purpose (`systemctl --user stop`, `schtasks /End`) stays stopped
/// instead of being resurrected by the window every two seconds.
pub fn open() -> Option<Link> {
  Link::connect()
}

/// Connect to the core, starting one if it is not running yet.
///
/// Everything goes through the socket even in the "portable" case: one code
/// path on every platform, and the session survives closing the window.
pub fn ensure_core() -> Result<Link, String> {
  if let Some(link) = Link::connect() {
    return Ok(link);
  }
  if crate::autostart::core_managed() {
    // The core belongs to systemd / Task Scheduler. Ask that manager to start
    // it: spawning a second, unmanaged copy behind its back would make `stop`
    // useless and fight the unit's own `Restart=` policy.
    crate::autostart::Autostart::service(crate::autostart::ServiceAction::Start, false)?;
  } else {
    spawn_core()?;
  }
  // The socket appears as soon as the core has bound it, before it touches the
  // network, so this is a short wait even on a machine with no connectivity.
  for _ in 0..60 {
    std::thread::sleep(SPAWN_WAIT);
    if let Some(link) = Link::connect() {
      return Ok(link);
    }
  }
  Err("the background service did not start".into())
}

fn spawn_core() -> Result<(), String> {
  let exe = std::env::current_exe().map_err(|e| e.to_string())?;
  let mut cmd = std::process::Command::new(exe);
  cmd
    .arg("core")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt as _;
    // No console flash; the binary is GUI-subsystem anyway.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
  }
  cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::logline::Line;
  use crate::proto::{LoginEnv, ToastKind};
  use crate::session::State;

  #[test]
  fn a_late_frontend_gets_the_current_state_and_the_log_tail() {
    let hub = Hub::new();
    hub.publish(Ev::State(State::Online));
    hub.publish(Ev::Env(LoginEnv::Campus));
    hub.publish(Ev::Log(Line::info("first")));
    hub.publish(Ev::Error(Some("boom".into())));

    let (_rx, backlog) = hub.attach();
    // Snapshot first, then the replayed log lines.
    assert!(matches!(backlog[0], Ev::State(State::Online)));
    assert!(matches!(backlog[1], Ev::Env(LoginEnv::Campus)));
    assert!(matches!(&backlog[2], Ev::Error(Some(e)) if e == "boom"));
    assert!(matches!(backlog.last(), Some(Ev::Log(_))));
  }

  #[test]
  fn every_attached_frontend_receives_events() {
    let hub = Hub::new();
    let (first, _) = hub.attach();
    let (second, _) = hub.attach();
    hub.publish(Ev::Notify {
      msg: "hi".into(),
      kind: ToastKind::Ok,
    });
    for events in [first, second] {
      assert!(matches!(events.try_recv(), Ok(Ev::Notify { .. })));
    }
  }

  #[test]
  fn the_replayed_history_is_bounded() {
    let hub = Hub::new();
    for i in 0..HISTORY + 20 {
      hub.publish(Ev::Log(Line::info(format!("line {i}"))));
    }
    let (_rx, backlog) = hub.attach();
    assert_eq!(backlog.len(), HISTORY);
  }

  /// Full round trip over the real endpoint: a core in a thread, a frontend
  /// here. Ignored by default because it binds the live per-user endpoint (a
  /// running instance makes it fail) — run it with an isolated runtime dir:
  ///
  /// ```text
  /// XDG_RUNTIME_DIR=/tmp/tt cargo test -- --ignored a_frontend_can_talk_to_a_core
  /// ```
  #[test]
  #[ignore = "binds the live core endpoint; run by hand"]
  fn a_frontend_can_talk_to_a_core() {
    std::thread::spawn(|| {
      let _ = serve(crate::config::Config::load());
    });

    let mut link = None;
    for _ in 0..40 {
      std::thread::sleep(Duration::from_millis(100));
      if let Some(l) = Link::connect() {
        link = Some(l);
        break;
      }
    }
    let link = link.expect("no core endpoint appeared");

    // The snapshot (state + network verdict) arrives right after connecting.
    let mut saw_snapshot = false;
    for _ in 0..50 {
      while let Some(ev) = link.try_recv() {
        saw_snapshot |= matches!(ev, Ev::State(_) | Ev::Env(_));
      }
      if saw_snapshot {
        break;
      }
      std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_snapshot, "no snapshot events");

    // A command has to travel the other way; both the success and the failure
    // path answer with a toast.
    link.send(Cmd::Refresh);
    let mut answered = false;
    for _ in 0..100 {
      while let Some(ev) = link.try_recv() {
        answered |= matches!(ev, Ev::Notify { .. });
      }
      if answered {
        break;
      }
      std::thread::sleep(Duration::from_millis(100));
    }
    assert!(answered, "the command did not reach the core");
  }
}
