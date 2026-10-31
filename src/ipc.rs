//! Transport between the background core and the window/tray frontend.
//!
//! One connection per frontend; the core fans every [`Ev`] out to all of them
//! and replays the tail of the log plus the current state to whoever connects
//! late. See [`crate::proto`] for the message shapes.
//!
//! The endpoint is per machine, not per account:
//!
//! * Linux: abstract socket `@thunet-control` in machine mode (any local uid
//!   can connect; `SO_PEERCRED` is checked), or
//!   `$XDG_RUNTIME_DIR/thunet-<state tag>/control.sock` (mode 0700) in portable
//!   mode, where the tag comes from the state directory,
//! * Windows: named pipe `\\.\pipe\thunet-control`, whose DACL grants the
//!   service account, administrators and interactive users, while the window
//!   runs in the user's session. The client checks the pipe's owner SID, so a
//!   user-run process cannot impersonate the service. A portable core gets
//!   `\\.\pipe\thunet-control-<user>-<state tag>`, owner-only.
//!
//! `interprocess` covers the socket/pipe; on Windows the pipe is created through
//! the same crate's named-pipe API, which takes the security descriptor.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use interprocess::local_socket::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::{
  ConnectOptions, GenericFilePath, GenericNamespaced, ListenerOptions, Name, ToFsName, ToNsName,
};
#[cfg(windows)]
use interprocess::os::windows::named_pipe::{
  PipeListener, PipeListenerOptions, PipeStream, pipe_mode::Bytes,
};

use crate::config::CoreConfig;
use crate::proto::{Cmd, Ev, Failure, Msg, VERSION, Wire};

/// Whether this process serves the machine-wide endpoint. Set by [`serve`].
static MACHINE: AtomicBool = AtomicBool::new(false);

/// Whether the core runs in a thread of this process instead of a process of
/// its own (portable mode, see [`host_core`]).
static HOSTED: AtomicBool = AtomicBool::new(false);

/// Asks a hosted core to end. The accept loop polls it; the worker returns.
static STOP: AtomicBool = AtomicBool::new(false);

/// How often a hosted core looks for a stopped flag or a new connection.
/// Below the shortest controller a human can click, so *Stop* feels immediate.
const HOST_POLL: Duration = Duration::from_millis(10);

/// How many recent events a frontend that connects late gets to see. Enough to
/// fill the log pane (which shows a handful of lines) without growing forever.
const HISTORY: usize = 200;

// ---------------------------------------------------------------- endpoint

/// The two directions of one connection, boxed so the rest of this module does
/// not have to know which platform produced them.
type Halves = (Box<dyn Read + Send>, Box<dyn Write + Send>);

#[cfg(unix)]
type Stream = interprocess::local_socket::Stream;
#[cfg(unix)]
type Listener = interprocess::local_socket::Listener;
#[cfg(windows)]
type Stream = PipeStream<Bytes, Bytes>;
#[cfg(windows)]
type Listener = PipeListener<Bytes, Bytes>;

/// Splits a connection at the type level, which `interprocess` spells
trait Duplex: Send + 'static {
  fn halves(self) -> Halves;
}

#[cfg(unix)]
impl Duplex for Stream {
  fn halves(self) -> Halves {
    let (recv, send) = self.split();
    (Box::new(recv), Box::new(send))
  }
}

#[cfg(windows)]
impl Duplex for Stream {
  fn halves(self) -> Halves {
    let (recv, send) = self.split();
    (Box::new(recv), Box::new(send))
  }
}

#[cfg(unix)]
fn endpoint(machine: bool) -> String {
  if machine {
    // Global in the network namespace, like a Windows named pipe. No
    // filesystem node, so no per-user directory permissions are involved; the
    // peer check in `attach` is what authorizes the client.
    return "@thunet-control".into();
  }
  #[cfg(target_os = "android")]
  {
    // The app's private directory: writable inside the sandbox and short
    // enough for `sun_path`. `THUNET_STATE_DIR` is the same directory the
    // config and credential use, so two setups never share a socket.
    let dir = std::env::var_os("THUNET_STATE_DIR")
      .map(std::path::PathBuf::from)
      .unwrap_or_else(|| std::env::temp_dir().join("thunet"));
    return dir.join("control.sock").to_string_lossy().into_owned();
  }
  #[cfg(not(target_os = "android"))]
  {
    use std::os::unix::fs::PermissionsExt as _;

    // The state directory is part of the name: two portable setups of one user
    // (two `--state-dir`s) are two independent cores, and a frontend must never
    // attach to the other one's session.
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
      Some(base) => {
        std::path::PathBuf::from(base).join(format!("thunet-{}", crate::config::state_tag()))
      }
      None => std::env::temp_dir().join(format!(
        "thunet-{}-{}",
        user_tag(),
        crate::config::state_tag()
      )),
    };
    if std::fs::create_dir_all(&dir).is_ok() {
      // The socket inherits the directory's permissions: owner only.
      let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir.join("control.sock").to_string_lossy().into_owned()
  }
}

#[cfg(windows)]
fn endpoint(machine: bool) -> String {
  if machine {
    // One name for the whole machine: the core is a service running as SYSTEM
    // in session 0, so an account-suffixed name could never be reached from the
    // window. Who may connect is decided by the pipe's DACL, not by the name.
    return "thunet-control".into();
  }
  // A portable core is owned by the user who started it, and belongs to one
  // state directory: the tag keeps two setups of the same user apart, because
  // sharing a name would let one window drive the other's session.
  format!(
    "thunet-control-{}-{}",
    user_tag(),
    crate::config::state_tag()
  )
}

#[cfg(not(target_os = "android"))]
fn user_tag() -> String {
  let raw = std::env::var("USER")
    .or_else(|_| std::env::var("LOGNAME"))
    .or_else(|_| std::env::var("USERNAME"))
    .unwrap_or_else(|_| "user".into());
  let tag: String = raw
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect();
  if tag.is_empty() { "user".into() } else { tag }
}

/// Where frontends connect; shown by `thunet status`.
pub fn endpoint_path() -> String {
  #[cfg(windows)]
  {
    format!(r"\\.\pipe\{}", endpoint(MACHINE.load(Ordering::Relaxed)))
  }
  #[cfg(unix)]
  {
    endpoint(MACHINE.load(Ordering::Relaxed))
  }
}

#[cfg(unix)]
fn name(path: &str) -> std::io::Result<Name<'_>> {
  match path.strip_prefix('@') {
    Some(ns) => ns.to_ns_name::<GenericNamespaced>(),
    None => path.to_fs_name::<GenericFilePath>(),
  }
}

// --------------------------------------------------------------------- hub

/// Fan-out of events from the single worker loop to every attached frontend.
#[derive(Clone)]
pub struct Hub(Arc<Mutex<Shared>>);

#[derive(Default)]
struct Shared {
  clients: Vec<Client>,
  /// Replayed to late connections: log lines and toasts, newest last.
  history: VecDeque<Ev>,
  state: Option<crate::session::State>,
  env: Option<crate::proto::LoginEnv>,
  info: Option<serde_json::Value>,
  error: Option<Failure>,
}

/// One attached frontend.
///
/// The flag is what makes the count exact at the moment it is asked for: a
/// `Sender` cannot tell whether its peer is still there (`std`'s `mpsc` has no
/// `is_closed`), and pruning only inside `publish` means a frontend that left
/// a moment ago is still counted until the next event happens to be
/// published — which is exactly the delay an installer polling for "all
/// windows gone" would pay twice.
struct Client {
  tx: Sender<Ev>,
  live: Arc<AtomicBool>,
}

impl Client {
  fn is_live(&self) -> bool {
    self.live.load(Ordering::Relaxed)
  }
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
      // Diagnostics also go to the journal / the service log file; the
      // frontend only needs them for the log pane.
      Ev::Log(line) => crate::diag::line(&line.render()),
      // Replies to one-off requests belong to the client that asked; they are
      // not part of the state a late frontend needs.
      Ev::Notify { .. }
      | Ev::Beat
      | Ev::Status(_)
      | Ev::Core(_)
      | Ev::Credentials(_)
      | Ev::AcId(_) => {}
    }
    // Only log lines are replayed: a toast belongs to the action that raised
    // it, and replaying one to a fresh frontend would look like news.
    if matches!(ev, Ev::Log(_)) {
      shared.history.push_back(ev.clone());
      if shared.history.len() > HISTORY {
        shared.history.pop_front();
      }
    }
    shared
      .clients
      .retain(|client| client.is_live() && client.tx.send(ev.clone()).is_ok());
  }

  /// Register a new frontend and hand back what it missed, plus the liveness
  /// handle its connection threads have to clear when they end.
  fn attach(&self) -> (Receiver<Ev>, Vec<Ev>, Arc<AtomicBool>) {
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
    let live = Arc::new(AtomicBool::new(true));
    shared.clients.push(Client {
      tx,
      live: live.clone(),
    });
    (rx, backlog, live)
  }

  /// How many frontends are attached right now, liveness flags included.
  ///
  /// Read before a new frontend is added, so the answer says whether this
  /// connect replaced something that was still there.
  fn live_count(&self) -> usize {
    let shared = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    shared.clients.iter().filter(|client| client.is_live()).count()
  }

  /// A `Sender<Ev>`-shaped handle for the worker loop.
  pub fn sink(&self) -> Sink {
    Sink(self.clone())
  }

  /// How many frontends are attached right now.
  ///
  /// Dead connections are dropped first, so the answer describes this instant
  /// rather than the last time something happened to be published.
  fn frontends(&self) -> usize {
    let mut shared = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    shared.clients.retain(Client::is_live);
    shared.clients.len()
  }

  /// End every attached connection.
  ///
  /// Closing the listener is not enough when a core stops: the connections it
  /// already accepted stay open, and a window would sit on a link that never
  /// answers again. Dropping the senders ends each writer thread, which closes
  /// the peer's read side, so the frontend sees the ordinary "core went away".
  fn close_all(&self) {
    let mut shared = match self.0.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    shared.clients.clear();
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
  /// Another core already owns the endpoint. Only a portable core reports this
  /// (and then exits quietly); a machine-mode core waits out a predecessor that
  /// is still shutting down and fails loudly if it never lets go.
  AlreadyRunning,
}

enum BindError {
  InUse,
  Other(String),
}

/// How long a service start waits for a predecessor to let go of the endpoint.
const BIND_WAIT: Duration = Duration::from_secs(15);
/// Poll interval while waiting, doubling up to [`BIND_STEP_MAX`]. A predecessor
/// normally releases in well under a second, so the first steps are short; the
/// backoff only matters when something is genuinely stuck, and keeps the
/// retries from turning into a busy loop of probe connections against whatever
/// still owns the endpoint.
const BIND_STEP: Duration = Duration::from_millis(100);
const BIND_STEP_MAX: Duration = Duration::from_millis(1_000);

/// Bind the endpoint, waiting out an owner that is still shutting down.
///
/// The SCM has already been told the service is `RUNNING` by the time a
/// predecessor releases the pipe, so giving up on `IN_USE` at once leaves the
/// service looking alive while serving nobody, and only another manual restart
/// brings it back. A portable core still exits immediately: there a busy
/// endpoint really does mean another core is already serving, and the caller can
/// just talk to that one.
fn bind_listener(path: &str, machine: bool) -> Result<Option<Listener>, String> {
  // `None` waits for nobody: one attempt, and a busy endpoint is the answer.
  let wait = machine.then_some(BIND_WAIT);
  bind_retrying(path, wait, || bind(path, machine))
}

/// Drive `attempt` until it succeeds, waiting at most `wait` for a busy endpoint.
///
/// Returns `Ok(None)` when `wait` is `None` and the very first attempt finds the
/// endpoint in use, so the caller can report "already running" instead of an
/// error.
fn bind_retrying<T>(
  path: &str,
  wait: Option<Duration>,
  mut attempt: impl FnMut() -> Result<T, BindError>,
) -> Result<Option<T>, String> {
  let Some(wait) = wait else {
    return match attempt() {
      Ok(value) => Ok(Some(value)),
      Err(BindError::InUse) => Ok(None),
      Err(BindError::Other(e)) => Err(e),
    };
  };
  let deadline = Instant::now() + wait;
  let mut step = BIND_STEP.min(wait);
  loop {
    match attempt() {
      Ok(value) => return Ok(Some(value)),
      Err(BindError::Other(e)) => return Err(e),
      Err(BindError::InUse) => {
        if Instant::now() + step >= deadline {
          let waited = if wait.as_secs() > 0 {
            format!("{} s", wait.as_secs())
          } else {
            format!("{} ms", wait.as_millis())
          };
          return Err(format!(
            "cannot listen on {path}: another core owned it for {waited}, so this one is not serving"
          ));
        }
        std::thread::sleep(step);
        step = (step * 2).min(BIND_STEP_MAX);
      }
    }
  }
}

/// Run the core: bind the endpoint, then drive the session loop forever.
///
/// `machine` selects the machine-wide endpoint and requires the process to run
/// as the service identity. A user-run core stays on its per-user endpoint.
pub fn serve(cfg: CoreConfig, machine: bool) -> Result<Serve, String> {
  if machine && !crate::ident::is_current_service() {
    return Err(
      "refusing machine mode: this process is not running as the THUnet service account".into(),
    );
  }
  MACHINE.store(machine, Ordering::Relaxed);
  let path = endpoint(machine);
  let listener = match bind_listener(&path, machine)? {
    Some(listener) => listener,
    None => return Ok(Serve::AlreadyRunning),
  };
  crate::diag::line(&format!("core: listening on {path} (machine={machine})"));
  #[cfg(unix)]
  if !path.starts_with('@') {
    // The directory is already 0700; this is belt and braces for a fallback
    // directory that happened to be world-readable.
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
  }
  let hub = Hub::new();
  let (cmd_tx, cmd_rx) = channel::<Cmd>();
  let worker_hub = hub.clone();
  std::thread::spawn(move || crate::core::worker(cfg, cmd_rx, worker_hub.sink()));

  // One accept loop for the life of the core; each connection gets its own
  // reader and writer thread so a stalled frontend cannot block the others.
  //
  // A hosted core polls instead of blocking in `accept`, because it has to be
  // stoppable without ending its host: `Cmd::Shutdown` sets the flag and the
  // next tick breaks out of here. A core with a process of its own keeps the
  // cheaper blocking wait - ending it means ending the process.
  let polling = hosted();
  if polling {
    allow_polling(&listener)?;
  }
  loop {
    if polling && stopping() {
      crate::diag::line("core: stopping (its host asked)");
      // The listener is about to go; the connections it already accepted have
      // to go with it or the window keeps talking to nobody.
      hub.close_all();
      break;
    }
    match listener.accept() {
      Ok(stream) => {
        // A polling listener also hands out polling streams: the flag is part
        // of the instance, and every new instance is created from it. See
        // `unpoll_accepted` for what that costs.
        if polling && let Err(e) = unpoll_accepted(&stream) {
          crate::diag::line(&format!("core: cannot make the connection blocking: {e}"));
          continue;
        }
        spawn_connection(stream, &cmd_tx, &hub)
      }
      Err(e) if polling && e.kind() == std::io::ErrorKind::WouldBlock => {
        std::thread::sleep(HOST_POLL);
      }
      Err(e) => {
        crate::diag::line(&format!("core: accept failed: {e}"));
        if polling {
          std::thread::sleep(HOST_POLL);
        }
      }
    }
  }
  Ok(Serve::Started)
}

/// Undo the listener's nonblocking flag on one accepted connection.
///
/// The Windows listener creates every new pipe instance from its own options,
/// so enabling `PIPE_NOWAIT` for polling also applies to the streams it hands
/// out. A nonblocking read on a pipe with nothing to read does not block: it
/// returns zero bytes, which the reader thread can only read as the frontend
/// closing the connection. Every frontend was therefore dropped right after it
/// attached - the window reconnected in a loop and its state flickered between
/// online and offline, and a one-shot `thunet status` never got its answer. The
/// flag is per instance, so clearing it here leaves the listener itself
/// polling.
#[cfg(windows)]
fn unpoll_accepted(stream: &Stream) -> Result<(), String> {
  stream.set_nonblocking(false).map_err(|e| e.to_string())
}

/// The local-socket listener already keeps its accepted streams blocking; only
/// `accept` itself polls, so there is nothing to undo.
#[cfg(unix)]
fn unpoll_accepted(_stream: &Stream) -> Result<(), String> {
  Ok(())
}

/// Make `accept` return [`WouldBlock`](std::io::ErrorKind::WouldBlock) instead
/// of waiting, so the accept loop can look at its stop flag.
#[cfg(unix)]
fn allow_polling(listener: &Listener) -> Result<(), String> {
  use interprocess::local_socket::ListenerNonblockingMode;
  listener
    .set_nonblocking(ListenerNonblockingMode::Accept)
    .map_err(|e| format!("cannot poll the hosted listener: {e}"))
}

/// The raw named-pipe listener has the same switch, spelled with a `bool`.
#[cfg(windows)]
fn allow_polling(listener: &Listener) -> Result<(), String> {
  listener
    .set_nonblocking(true)
    .map_err(|e| format!("cannot poll the hosted listener: {e}"))
}

/// Hand one accepted connection to its own reader and writer threads.
fn spawn_connection(stream: Stream, cmds: &Sender<Cmd>, hub: &Hub) {
  #[cfg(windows)]
  {
    let cmds = cmds.clone();
    let hub = hub.clone();
    // Impersonation only works after the server has read from the pipe, so
    // reading the first line happens in this connection's own thread (a silent
    // client must not block the accept loop).
    std::thread::spawn(move || handle_windows_connection(stream, cmds, hub));
  }
  #[cfg(unix)]
  {
    let peer = peer_of(&stream);
    crate::diag::line(&format!(
      "core: accepted connection from {} (authorized={})",
      peer.label, peer.authorized
    ));
    if !peer.authorized {
      // Fail closed, but tell the frontend why instead of letting it reconnect
      // every two seconds with no explanation.
      let (_, mut out) = stream.halves();
      let line = Wire::ev(Ev::Log(crate::logline::Line::warn(format!(
        "core rejected frontend: {}",
        peer.label
      ))));
      let _ = write_line(&mut BufWriter::new(&mut out), &line);
      crate::diag::line(&format!("core: rejected frontend {}", peer.label));
      return;
    }
    attach(stream, peer, cmds.clone(), hub.clone(), String::new());
  }
}

/// Read one newline-terminated line without consuming bytes past it.
///
/// `ImpersonateNamedPipeClient` fails with `ERROR_CANNOT_IMPERSONATE` until the
/// server has read from the pipe, so the identity check has to wait for the
/// frontend's first command. Reading one byte at a time keeps the rest of the
/// stream intact for the normal line reader.
#[cfg(windows)]
fn read_first_line(stream: &mut Stream) -> Result<String, String> {
  const MAX: usize = 64 * 1024;
  let mut bytes = Vec::new();
  let mut byte = [0u8; 1];
  loop {
    match stream.read(&mut byte) {
      Ok(0) => return Err("connection closed before the first command".into()),
      Ok(_) if byte[0] == b'\n' => break,
      Ok(_) if byte[0] != b'\r' => {
        bytes.push(byte[0]);
        if bytes.len() > MAX {
          return Err("first command is too long".into());
        }
      }
      Ok(_) => {}
      Err(e) => return Err(format!("cannot read the first command: {e}")),
    }
  }
  String::from_utf8(bytes).map_err(|e| format!("first command is not UTF-8: {e}"))
}

#[cfg(windows)]
fn handle_windows_connection(mut stream: Stream, cmds: Sender<Cmd>, hub: Hub) {
  let first = match read_first_line(&mut stream) {
    Ok(first) => first,
    Err(e) => {
      crate::diag::line(&format!("core: {e}"));
      return;
    }
  };
  let peer = peer_of(&stream);
  crate::diag::line(&format!(
    "core: accepted connection from {} (authorized={})",
    peer.label, peer.authorized
  ));
  if !peer.authorized {
    let line = Wire::ev(Ev::Log(crate::logline::Line::warn(format!(
      "core rejected frontend: {}",
      peer.label
    ))));
    let mut out = BufWriter::new(&mut stream);
    let _ = write_line(&mut out, &line);
    let _ = out.flush();
    crate::diag::line(&format!("core: rejected frontend {}", peer.label));
    return;
  }
  attach(stream, peer, cmds, hub, first);
}

#[cfg(unix)]
fn bind(path: &str, _machine: bool) -> Result<Listener, BindError> {
  let create = || -> std::io::Result<Listener> {
    let name = name(path)?;
    ListenerOptions::new().name(name).create_sync()
  };
  match create() {
    Ok(listener) => Ok(listener),
    Err(first) => {
      // A live core answers a probe; a leftover socket file does not. Abstract
      // names have no filesystem node and disappear with their process.
      if connect(path).is_ok() {
        return Err(BindError::InUse);
      }
      if !path.starts_with('@') {
        let _ = std::fs::remove_file(path);
      }
      create().map_err(|e| {
        BindError::Other(format!(
          "cannot listen on {path}: {e} (first attempt: {first})"
        ))
      })
    }
  }
}

/// Named pipe with an explicit DACL: the core owns the pipe as SYSTEM, whose
/// default ACL (SYSTEM + administrators) would hide it from the window.
///
/// SYSTEM and administrators get everything; interactive users may read and
/// write, which is all a frontend needs. A portable core has no reason to
/// expose itself to other accounts, so its pipe is owner-only. The machine
/// half is shared with every other machine-wide channel this program publishes.
#[cfg(windows)]
pub(crate) const INTERACTIVE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

#[cfg(windows)]
fn pipe_sddl(machine: bool) -> Result<String, String> {
  if machine {
    let mut sddl = INTERACTIVE_SDDL.to_string();
    // `accept()` creates a new instance for every connection. Creating an
    // instance requires FILE_CREATE_PIPE_INSTANCE on the existing pipe, so the
    // service account needs its own ACE; without it the first client connects
    // but is never accepted, and every later client times out.
    let service_sids = crate::ident::service_sid_strings();
    if service_sids.is_empty() {
      return Err("cannot resolve the service account SID for the machine pipe".into());
    }
    for sid in service_sids {
      sddl.push_str(&format!("(A;;GA;;;{sid})"));
    }
    Ok(sddl)
  } else {
    let sid = crate::ident::current_user_sid_string()
      .ok_or("cannot determine the current user SID for the portable pipe")?;
    Ok(format!("D:(A;;GA;;;{sid})"))
  }
}

/// Listener options for `path` with an explicit DACL.
#[cfg(windows)]
fn pipe_options_with(path: &str, sddl: &str) -> Result<PipeListenerOptions<'static>, String> {
  use std::borrow::Cow;

  use interprocess::os::windows::security_descriptor::SecurityDescriptor;
  use widestring::U16CString;

  let full =
    U16CString::from_str(format!(r"\\.\pipe\{path}")).map_err(|e| format!("bad pipe name: {e}"))?;
  let sddl = U16CString::from_str(sddl).map_err(|e| format!("bad pipe DACL: {e}"))?;
  let descriptor =
    SecurityDescriptor::deserialize(sddl.as_ucstr()).map_err(|e| format!("bad pipe DACL: {e}"))?;

  let mut options = PipeListenerOptions::new();
  options.path = Cow::Owned(full);
  options.security_descriptor = Some(descriptor);
  Ok(options)
}

#[cfg(windows)]
fn pipe_options(path: &str, machine: bool) -> Result<PipeListenerOptions<'static>, String> {
  pipe_options_with(path, &pipe_sddl(machine)?)
}

#[cfg(windows)]
fn bind(path: &str, machine: bool) -> Result<Listener, BindError> {
  let options = pipe_options(path, machine).map_err(BindError::Other)?;
  let create = || options.create_duplex::<Bytes>();
  match create() {
    Ok(listener) => Ok(listener),
    Err(first) => {
      // ERROR_PIPE_BUSY or ERROR_ACCESS_DENIED both mean an instance exists,
      // so this name is taken rather than broken.
      if connect(path).is_ok() {
        return Err(BindError::InUse);
      }
      create().map_err(|e| {
        BindError::Other(format!(
          "cannot listen on {path}: {e} (first attempt: {first})"
        ))
      })
    }
  }
}

#[cfg(unix)]
fn connect(path: &str) -> Result<Stream, String> {
  let name = name(path).map_err(|e| format!("bad endpoint {path}: {e}"))?;
  ConnectOptions::new()
    .name(name)
    .connect_sync()
    .map_err(|e| format!("{path}: {e}"))
}

#[cfg(windows)]
fn connect(path: &str) -> Result<Stream, String> {
  use widestring::U16CString;

  let full = format!(r"\\.\pipe\{path}");
  let wide = U16CString::from_str(&full).map_err(|e| format!("bad pipe name: {e}"))?;
  PipeStream::<Bytes, Bytes>::connect_by_path(wide.as_ucstr()).map_err(|e| format!("{full}: {e}"))
}

// ----------------------------------------------------------------- endpoint

/// A named endpoint this process owns, for a channel that is not the core's
/// link - today the machine-wide quit and the answers its windows send back.
///
/// The transport, the peer identity and the polling accept are the same ones
/// the core link uses, which is the point: the quit channel used to hand-roll a
/// `PIPE_NOWAIT` loop and got two documented semantics wrong (a fresh
/// instance's first `ConnectNamedPipe` means "available", not "connected";
/// `ERROR_NO_DATA` means a client connected and left), so every window's answer
/// was read and thrown away. Sharing this layer keeps the two from drifting
/// apart again.
///
/// The protocol is one line in each direction: the peer writes its state, the
/// owner records it and writes one byte back. The byte is an acknowledgement,
/// not data: a Windows pipe loses both the buffered line and the client's
/// identity if the client goes away before the server accepts it, so the peer
/// stays connected until it sees this.
pub(crate) mod endpoint {
  #[cfg(unix)]
  use interprocess::local_socket::prelude::*;
  use std::io::{Read as _, Write as _};
  use std::time::{Duration, Instant};

  /// How long one exchange may take before the peer counts as silent.
  const IO: Duration = Duration::from_millis(250);
  /// Poll interval for the bounded reads below.
  const POLL: Duration = Duration::from_millis(5);
  /// The whole protocol is one short state word.
  const MAX_LINE: usize = 64;

  pub(crate) struct Endpoint(super::Listener);

  /// One accepted (or initiated) connection on such an endpoint.
  pub(crate) struct Conn(super::Stream);

  impl Endpoint {
    /// Bind `path`: a bare pipe name on Windows (the `\\.\pipe\` prefix is added
    /// here), a filesystem path on Unix. `sddl` is Windows-only; on Unix the
    /// socket file and the directory holding it decide who may connect.
    pub(crate) fn bind(path: &str, sddl: &str) -> Result<Self, String> {
      #[cfg(windows)]
      {
        let options = super::pipe_options_with(path, sddl)?;
        let listener = options
          .create_duplex::<super::Bytes>()
          .map_err(|e| format!("cannot listen on {path}: {e}"))?;
        // `accept` has to return `WouldBlock` so the caller can look at its stop
        // flag, exactly as the core's hosted listener does.
        listener
          .set_nonblocking(true)
          .map_err(|e| format!("cannot poll {path}: {e}"))?;
        Ok(Self(listener))
      }
      #[cfg(unix)]
      {
        let _ = sddl;
        let name = super::name(path).map_err(|e| format!("bad endpoint {path}: {e}"))?;
        let listener = interprocess::local_socket::ListenerOptions::new()
          .name(name)
          .create_sync()
          .map_err(|e| format!("cannot listen on {path}: {e}"))?;
        listener
          .set_nonblocking(interprocess::local_socket::ListenerNonblockingMode::Accept)
          .map_err(|e| format!("cannot poll {path}: {e}"))?;
        Ok(Self(listener))
      }
    }

    /// One waiting connection, or `None` when nobody is there yet.
    pub(crate) fn accept(&self) -> Option<Conn> {
      self.0.accept().ok().map(Conn::new)
    }
  }

  impl Conn {
    /// Make an accepted polling stream blocking again.
    ///
    /// A Windows polling listener hands out polling streams, and a nonblocking
    /// read on a pipe with nothing buffered returns zero bytes - which cannot
    /// be told apart from a peer that left. See `unpoll_accepted`.
    #[cfg(windows)]
    fn new(stream: super::Stream) -> Self {
      let _ = stream.set_nonblocking(false);
      Self(stream)
    }

    /// The accepted streams a Unix listener hands out are already blocking.
    #[cfg(unix)]
    fn new(stream: super::Stream) -> Self {
      Self(stream)
    }

    /// The client's process id, as the kernel reports it.
    pub(crate) fn pid(&self) -> Option<u32> {
      #[cfg(windows)]
      {
        self.0.client_process_id().ok()
      }
      #[cfg(unix)]
      {
        self
          .0
          .peer_creds()
          .ok()
          .and_then(|creds| creds.pid())
          .map(|pid| pid as u32)
      }
    }

    /// The client's uid, where the platform has one (Unix only).
    ///
    /// `SO_PEERCRED` is what makes the answer trustworthy on Unix: the uid comes
    /// from the kernel, never from the payload.
    #[cfg(unix)]
    pub(crate) fn uid(&self) -> Option<u32> {
      self.0.peer_creds().ok().and_then(|creds| creds.euid())
    }

    /// Read one newline-terminated line, giving up after [`IO`].
    pub(crate) fn read_line(&mut self) -> Option<String> {
      if self.0.set_nonblocking(true).is_err() {
        return None;
      }
      let deadline = Instant::now() + IO;
      let mut bytes = Vec::new();
      let mut byte = [0u8; 1];
      while Instant::now() < deadline && bytes.len() < MAX_LINE {
        match self.0.read(&mut byte) {
          // Nothing buffered yet. On a Windows pipe that is a zero-byte read,
          // not an end of input; the deadline above is what ends the wait.
          Ok(0) => std::thread::sleep(POLL),
          Ok(_) => {
            if byte[0] == b'\n' {
              break;
            }
            bytes.push(byte[0]);
          }
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
          Err(_) => break,
        }
      }
      let _ = self.0.set_nonblocking(false);
      (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Send one line of state.
    pub(crate) fn write_line(&mut self, line: &str) -> Result<(), String> {
      self.0.write_all(line.as_bytes()).map_err(|e| e.to_string())
    }

    /// Report that the line just read has been recorded.
    pub(crate) fn ack(&mut self) {
      let _ = self.0.write_all(b"\n");
      let _ = self.0.flush();
    }

    /// Wait for the owner's acknowledgement, giving up after [`IO`].
    pub(crate) fn wait_ack(&mut self) -> bool {
      if self.0.set_nonblocking(true).is_err() {
        return false;
      }
      let deadline = Instant::now() + IO;
      let mut byte = [0u8; 1];
      while Instant::now() < deadline {
        match self.0.read(&mut byte) {
          Ok(n) if n > 0 => return true,
          Ok(_) => std::thread::sleep(POLL),
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
          Err(_) => return false,
        }
      }
      false
    }
  }

  /// Connect to an endpoint this program published.
  pub(crate) fn connect(path: &str) -> Result<Conn, String> {
    Ok(Conn::new(super::connect(path)?))
  }
}

fn attach(stream: impl Duplex, peer: Peer, cmds: Sender<Cmd>, hub: Hub, first: String) {
  // Subscribe before writing the backlog, so an event published in between
  // arrives exactly once through the channel (the backlog is a snapshot).
  //
  // The count is taken before this one is added: it says whether this connect
  // replaced a frontend that was still attached, which is what the teardown
  // line below is about. A window that reconnects used to leave "attached"
  // and "went away" in the same second with nothing linking the two.
  let others = hub.live_count();
  let started = Instant::now();
  let (events, backlog, live) = hub.attach();
  crate::diag::line(&format!("core: frontend attached ({})", peer.label));
  hub.publish(Ev::Log(crate::logline::Line::debug(format!(
    "core: attached frontend ({}{})",
    peer.label,
    match others {
      0 => String::new(),
      n => format!(", {n} still attached"),
    }
  ))));
  let (recv_half, send_half) = stream.halves();

  let writer_hub = hub.clone();
  let writer_label = peer.label.clone();
  let writer_live = live.clone();
  std::thread::spawn(move || {
    // The flag is cleared however this thread ends, including the early
    // returns below: a frontend that is gone must stop being counted at once,
    // not at the next publish.
    (move || {
      let mut out = BufWriter::new(send_half);
      for ev in backlog {
        if let Err(e) = write_line(&mut out, &Wire::ev(ev)) {
          // A window that was closed (or upgraded) takes its pipe with it;
          // that is the normal end of a frontend, not a failure worth a
          // warning.
          crate::diag::line(&format!("core: writer for {writer_label} stopped: {e}"));
          writer_hub.publish(Ev::Log(crate::logline::Line::debug(format!(
            "core: frontend went away after {:.1}s: {e}",
            started.elapsed().as_secs_f32()
          ))));
          return;
        }
      }
      for ev in events {
        if let Err(e) = write_line(&mut out, &Wire::ev(ev)) {
          crate::diag::line(&format!("core: writer for {writer_label} stopped: {e}"));
          writer_hub.publish(Ev::Log(crate::logline::Line::debug(format!(
            "core: frontend went away after {:.1}s: {e}",
            started.elapsed().as_secs_f32()
          ))));
          return;
        }
      }
    })();
    writer_live.store(false, Ordering::Relaxed);
  });

  let reader_hub = hub.clone();
  let reader_label = peer.label.clone();
  let reader_live = live;
  std::thread::spawn(move || {
    // Same as the writer: the count has to drop the moment the peer is gone,
    // and this is the thread that notices that first.
    (move || {
      let mut initial = first.into_bytes();
      if !initial.is_empty() {
        initial.push(b'\n');
      }
      let mut lines = BufReader::new(std::io::Cursor::new(initial).chain(recv_half)).lines();
      loop {
        let text = match lines.next() {
          Some(Ok(text)) => text,
          Some(Err(e)) => {
            crate::diag::line(&format!("core: reader for {reader_label} failed: {e}"));
            reader_hub.publish(Ev::Log(crate::logline::Line::debug(format!(
              "core: frontend read failed after {:.1}s: {e}",
              started.elapsed().as_secs_f32()
            ))));
            break;
          }
          None => {
            crate::diag::line(&format!(
              "core: frontend {reader_label} disconnected after {:.1}s",
              started.elapsed().as_secs_f32()
            ));
            break;
          }
        };
        if text.trim().is_empty() {
          continue;
        }
        let Ok(wire) = Wire::parse(&text) else {
          reader_hub.publish(Ev::Log(crate::logline::Line::warn(
            "core: malformed command from the frontend",
          )));
          continue;
        };
        match wire.msg {
          Msg::Cmd(cmd) if wire.v == VERSION => {
            if cmds.send(cmd).is_err() {
              crate::diag::line(&format!("core: worker gone, dropping {reader_label}"));
              return;
            }
          }
          Msg::Cmd(_) => {
            // Do not act on a message we may be reading with the wrong rules.
            reader_hub.publish(Ev::Log(crate::logline::Line::error(format!(
              "frontend speaks protocol {} but the core speaks {VERSION}; upgrade both halves",
              wire.v
            ))));
            return;
          }
          // A core never receives events.
          Msg::Ev(_) => {}
        }
      }
    })();
    reader_live.store(false, Ordering::Relaxed);
  });
}

fn write_line(out: &mut impl Write, wire: &Wire) -> std::io::Result<()> {
  let line = wire.line().map_err(std::io::Error::other)?;
  out.write_all(line.as_bytes())?;
  out.flush()
}

// ------------------------------------------------------------------ client

/// A connection's peer, reduced to what the policy needs.
struct Peer {
  label: String,
  /// Whether the token check let this peer drive the core. The transport's own
  /// permissions (the pipe DACL) have already decided who may connect at all.
  authorized: bool,
}

#[cfg(windows)]
fn peer_of(stream: &Stream) -> Peer {
  let label = match stream.client_process_id() {
    Ok(pid) => crate::ident::describe(pid),
    Err(_) => "unknown".into(),
  };
  // The pipe DACL already decides who may connect: SYSTEM, Administrators and
  // INTERACTIVE for the machine pipe, the owner only for a portable one.
  // Impersonation therefore checks one thing the DACL cannot express - that the
  // peer is not the service's own account, which the ACL has to trust with
  // `FILE_CREATE_PIPE_INSTANCE` and which could otherwise pose as a frontend.
  //
  // The session id is copied into the label and deliberately kept out of the
  // decision. OpenSSH runs its children in session 0 with the user's own token,
  // so rejecting session 0 turned every `thunet status` typed over SSH into a
  // pipe the DACL had just admitted being closed again, with no reason the CLI
  // could show.
  let (authorized, how) = match stream.impersonate_client() {
    Ok(guard) => {
      let sid = crate::ident::current_thread_user_sid();
      let session = crate::ident::current_thread_session();
      drop(guard);
      let sid_text = sid
        .as_deref()
        .and_then(crate::ident::sid_to_string)
        .unwrap_or_else(|| "unknown".into());
      let how = match session {
        Some(session) => format!("impersonated sid={sid_text} session={session}"),
        None => format!("impersonated sid={sid_text}, session unreadable"),
      };
      // A SID that cannot be read cannot be told apart from the service account,
      // so it fails closed; a missing session id only costs a label.
      match sid {
        Some(sid) => (!crate::ident::is_service_sid(&sid), how),
        None => (false, how),
      }
    }
    Err(e) => (false, format!("impersonation failed: {e}")),
  };
  Peer {
    label: format!("{label} [{how}]"),
    authorized,
  }
}

#[cfg(unix)]
fn peer_of(stream: &Stream) -> Peer {
  match stream.peer_creds().ok().and_then(|c| c.euid()) {
    Some(uid) => Peer {
      label: crate::ident::describe(uid),
      authorized: crate::ident::is_interactive(uid),
    },
    None => Peer {
      label: "unknown".into(),
      authorized: false,
    },
  }
}

/// A running core, as seen from the frontend.
pub struct Link {
  cmds: Sender<Cmd>,
  events: Receiver<Ev>,
  alive: Arc<AtomicBool>,
  /// Protocol version the core announced when it refused to talk to this
  /// build. A frontend needs it to tell "an old core is serving" apart from
  /// "no core at all", which are very different situations for the user.
  speaks: Arc<Mutex<Option<u8>>>,
  /// The last warning the core sent, kept because a core that refuses a
  /// frontend says why in one warning and then closes the pipe. The event still
  /// travels to the caller - the CLI drops everything it reads while it waits
  /// for the replay to end - so the reason is remembered here or nowhere.
  last_warn: Arc<Mutex<Option<String>>>,
  machine: bool,
}

/// Remember the reason a link is about to die for.
///
/// Only warnings and above count: the replay a frontend receives on connect is
/// mostly `DEBUG` bookkeeping, and remembering those would answer "closed by
/// itself" with an unrelated line. A later warning replaces an earlier one, so
/// the slot always names the last thing that went wrong.
fn remember_warn(slot: &Mutex<Option<String>>, ev: &Ev) {
  if let Ev::Log(line) = ev
    && line.level >= crate::logline::Level::Warn
  {
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(line.msg.clone());
  }
}

#[cfg(windows)]
fn trusted_stream(stream: Stream, machine: bool) -> Result<Stream, String> {
  use std::os::windows::io::{AsRawHandle as _, OwnedHandle};

  let handle = match OwnedHandle::try_from(stream) {
    Ok(handle) => handle,
    Err(_) => return Err("cannot take ownership of the pipe handle".into()),
  };
  let raw = handle.as_raw_handle() as _;
  // The machine pipe must belong to the service; a portable pipe must belong
  // to this user. Accepting either owner for both names is what let a user-run
  // `thunet core` impersonate the service.
  let trusted = if machine {
    crate::ident::pipe_owner_is_service(raw)
  } else {
    crate::ident::pipe_owner_is_current_user(raw)
  };
  if !trusted {
    let owner = crate::ident::pipe_owner_sid(raw)
      .as_deref()
      .and_then(crate::ident::sid_to_string)
      .unwrap_or_else(|| "unknown".into());
    return Err(format!(
      "pipe owner {owner} is not {}",
      if machine { "the service" } else { "this user" }
    ));
  }
  Stream::try_from(handle).map_err(|_| "cannot reconstruct the pipe stream".into())
}

#[cfg(unix)]
fn trusted_stream(stream: Stream, machine: bool) -> Result<Stream, String> {
  let creds = stream
    .peer_creds()
    .map_err(|e| format!("peer_creds: {e}"))?;
  let uid = creds.euid().ok_or("peer has no effective uid")?;
  let trusted = if machine {
    // Root is trusted as well: it can read the machine state anyway, and a
    // hand-started `thunet core --machine` serves as root.
    crate::ident::expected_service_uid() == Some(uid) || uid == 0
  } else {
    crate::ident::is_current_user(uid)
  };
  if trusted {
    Ok(stream)
  } else {
    Err(format!("peer uid {uid} is not trusted for this endpoint"))
  }
}

impl Link {
  /// `None` when no core is listening, or when the process behind the endpoint
  /// is not the trusted core for this mode.
  #[allow(dead_code)] // kept for tests and callers that do not need a reason
  pub fn connect(machine: bool) -> Option<Link> {
    Self::connect_result(machine).ok()
  }

  /// Like [`Link::connect`], but the refusal reason is preserved for logging.
  pub fn connect_result(machine: bool) -> Result<Link, String> {
    let path = endpoint(machine);
    let stream = connect(&path).map_err(|e| format!("cannot connect to {path}: {e}"))?;
    let stream = trusted_stream(stream, machine)?;
    let (recv_half, send_half) = stream.halves();
    let alive = Arc::new(AtomicBool::new(true));
    let speaks = Arc::new(Mutex::new(None::<u8>));
    let last_warn = Arc::new(Mutex::new(None::<String>));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (ev_tx, ev_rx) = channel::<Ev>();

    let writer_alive = alive.clone();
    let writer_events = ev_tx.clone();
    std::thread::spawn(move || {
      let mut out = BufWriter::new(send_half);
      for cmd in cmd_rx {
        let wire = Wire::cmd(cmd);
        if let Err(e) = write_line(&mut out, &wire) {
          let _ = writer_events.send(Ev::Log(crate::logline::Line::warn(format!(
            "frontend: cannot send to the core: {e}"
          ))));
          break;
        }
      }
      writer_alive.store(false, Ordering::Relaxed);
    });

    let reader_alive = alive.clone();
    let reader_speaks = speaks.clone();
    let reader_warn = last_warn.clone();
    std::thread::spawn(move || {
      let mut lines = BufReader::new(recv_half).lines();
      loop {
        let text = match lines.next() {
          Some(Ok(text)) => text,
          Some(Err(e)) => {
            let _ = ev_tx.send(Ev::Log(crate::logline::Line::warn(format!(
              "frontend: the core connection failed: {e}"
            ))));
            break;
          }
          None => {
            let _ = ev_tx.send(Ev::Log(crate::logline::Line::debug(
              "frontend: the core closed the connection",
            )));
            break;
          }
        };
        if text.trim().is_empty() {
          continue;
        }
        match Wire::parse(&text) {
          Ok(wire) if wire.v == VERSION => {
            if let Msg::Ev(ev) = wire.msg {
              remember_warn(&reader_warn, &ev);
              if ev_tx.send(ev).is_err() {
                break;
              }
            }
          }
          Ok(wire) => {
            // Remember the version instead of only logging it here: the
            // window has to keep one stable banner and must not re-announce
            // the same mismatch every few seconds, and the CLI wants a precise
            // error when the link dies under it.
            let mut slot = reader_speaks
              .lock()
              .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(wire.v);
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

    Ok(Link {
      cmds: cmd_tx,
      events: ev_rx,
      alive,
      speaks,
      last_warn,
      machine,
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

  /// The protocol version the core answered with, when it is not the one this
  /// build speaks. `None` until such an answer arrives.
  pub fn speaks(&self) -> Option<u8> {
    let slot = self
      .speaks
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot
  }

  /// The last warning the core sent, when it died without answering.
  pub fn last_warn(&self) -> Option<String> {
    let slot = self
      .last_warn
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.clone()
  }

  /// Whether this link goes to the machine service rather than a portable core.
  pub fn is_machine(&self) -> bool {
    self.machine
  }
}

/// Whether this process serves the machine-wide endpoint.
pub fn is_machine() -> bool {
  MACHINE.load(Ordering::Relaxed)
}

/// Try to reach the core without starting anything.
///
/// This is what a running frontend uses when reconnecting, so a core that was
/// stopped on purpose (`systemctl stop`, `sc stop`) stays stopped instead of
/// being resurrected by the window every two seconds. The machine endpoint is
/// tried first unless this process is portable (see [`open_result`]).
pub fn open() -> Option<Link> {
  open_result().ok()
}

/// Whether this process asked for a portable core.
///
/// One definition for the whole program: [`open_result`], [`core_is_service`]
/// and the window's settings page all have to agree, otherwise "portable"
/// would mean different things in different places. An empty
/// `THUNET_STATE_DIR` counts as unset: `--state-dir ""` must not silently turn
/// the machine service into a portable core.
pub fn portable() -> bool {
  std::env::var_os("THUNET_STATE_DIR").is_some_and(|v| !v.is_empty())
}

/// Like [`open`], but reports why neither endpoint answered.
///
/// A portable process only ever tries the portable endpoint. Being shadowed by
/// the machine service is what made a hand-started portable core look broken:
/// the window and the CLI would answer from the service (a different credential
/// store, a different lifecycle) while the portable core sat there serving
/// nobody. `core_is_service` gives the same answer, so "portable" means one
/// thing everywhere.
pub fn open_result() -> Result<Link, String> {
  if portable() {
    return Link::connect_result(false).map_err(|reason| format!("portable: {reason}"));
  }
  match Link::connect_result(true) {
    Ok(link) => Ok(link),
    Err(machine) => Link::connect_result(false)
      .map_err(|portable| format!("machine: {machine}; portable: {portable}")),
  }
}

/// Does starting a core here mean starting the machine service?
///
/// Linux and Windows have exactly one supported core: the service, started
/// through systemd / the SCM. macOS has no machine service in this design, and
/// an explicit `THUNET_STATE_DIR` asks for a portable core on every platform,
/// so both of those host one instead (see [`host_core`]).
///
/// The frontend uses the same answer to decide where a credential belongs: the
/// machine core reads the machine directory, so a credential typed while no
/// core answers has to travel with the start that this function predicts.
pub fn core_is_service() -> bool {
  !portable() && (cfg!(any(windows, target_os = "linux")) || crate::autostart::core_managed())
}

/// Ask for a core without waiting for it to answer.
///
/// One rule for every platform, so "portable" means the same thing everywhere:
/// with [`core_is_service`] the service manager is asked (systemd on Linux, the
/// SCM on Windows - both register the core on first use when necessary),
/// otherwise the core is hosted in a thread of this process by [`host_core`].
/// A second, unmanaged *machine* core would make `systemctl stop` / `sc stop`
/// useless and would fight the unit's `Restart=` policy, which is why the two
/// cases must not be mixed up.
///
/// `restart` is for the upgrade case: a core that is already running keeps
/// running the replaced image, and starting it again is a no-op, so the service
/// has to be stopped and started to pick the new binary up. A hosted core is
/// this build by construction, so there is nothing to restart behind its back.
pub fn start_core(restart: bool) -> Result<(), String> {
  let action = if restart {
    crate::autostart::ServiceAction::Restart
  } else {
    crate::autostart::ServiceAction::Start
  };
  if core_is_service() {
    return crate::autostart::Autostart::service(action, false).map(|_| ());
  }
  // Portable: the core is a thread of this process, so there is nothing to
  // restart by remote control - hosting it again *is* the restart, and a
  // hosted core is always this build.
  host_core()
}

/// Whether the core of this process runs in one of its threads.
pub fn hosted() -> bool {
  HOSTED.load(Ordering::Relaxed)
}

fn stopping() -> bool {
  STOP.load(Ordering::Relaxed)
}

/// Ask a hosted core to end: the accept loop and the worker both check it.
///
/// The window's *Stop* button and a `Cmd::Shutdown` on the control channel end
/// up here. Nothing outside this process is touched.
pub fn request_stop() {
  STOP.store(true, Ordering::Relaxed);
}

/// Run a portable core in a thread of this process.
///
/// This is what makes portable mode one process: the window owns the core, the
/// core ends when the window does, and there is no child to orphan. The
/// endpoint is still served, so `thunet status` and `thunet quit` from a
/// terminal keep working.
///
/// A core that already serves the endpoint (a hand-started `thunet core`, or a
/// second click) is not an error: the thread ends at once with
/// [`Serve::AlreadyRunning`] and the window talks to that core instead.
pub fn host_core() -> Result<(), String> {
  if HOSTED.swap(true, Ordering::Relaxed) {
    // Idempotent on purpose: on Android the keep-alive service may host the
    // core first and the Activity then asks for the same thing, which is not
    // a conflict.
    return Ok(());
  }
  STOP.store(false, Ordering::Relaxed);
  let spawned = std::thread::Builder::new()
    .name("thunet-core".into())
    .spawn(|| {
      match serve(CoreConfig::load(), false) {
        Ok(Serve::Started) => {}
        Ok(Serve::AlreadyRunning) => {
          crate::diag::line("core (hosted): another core already serves the endpoint");
        }
        Err(e) => crate::diag::line(&format!("core (hosted): {e}")),
      }
      // The next *Start* has to see "nothing is hosted" again, whatever ended
      // the thread.
      HOSTED.store(false, Ordering::Relaxed);
    });
  if let Err(e) = spawned {
    HOSTED.store(false, Ordering::Relaxed);
    return Err(format!("cannot start the core thread: {e}"));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::logline::Line;
  use crate::proto::{LoginEnv, ToastKind};
  use crate::session::State;

  #[test]
  fn portable_and_machine_endpoints_are_distinct() {
    assert_ne!(
      endpoint(true),
      endpoint(false),
      "a portable core must not own the machine endpoint"
    );
  }

  #[test]
  fn a_portable_core_does_not_wait_for_a_busy_endpoint() {
    let started = Instant::now();
    let got = bind_retrying("thunet-control", None, || Err::<(), _>(BindError::InUse))
      .expect("a busy endpoint is an answer, not a failure");
    assert!(got.is_none(), "the caller reports 'already running'");
    assert!(
      started.elapsed() < Duration::from_secs(1),
      "a second portable core exits instead of waiting for the first"
    );
  }

  #[test]
  fn a_service_waits_out_the_core_it_replaces() {
    let mut attempts = 0;
    let got = bind_retrying("thunet-control", Some(Duration::from_secs(2)), || {
      attempts += 1;
      if attempts < 3 {
        Err(BindError::InUse)
      } else {
        Ok("listener")
      }
    })
    .expect("the predecessor let go within the window");
    assert_eq!(got, Some("listener"));
    assert_eq!(attempts, 3, "a busy endpoint is retried, not surrendered");
  }

  #[test]
  fn a_stuck_owner_is_reported_instead_of_serving_nothing() {
    let err = bind_retrying::<()>("thunet-control", Some(Duration::from_millis(250)), || {
      Err(BindError::InUse)
    })
    .expect_err("an owner that never leaves must not look like a running service");
    assert!(
      err.contains("another core owned it for 250 ms"),
      "the wait must be visible in the error: {err}"
    );
  }

  #[test]
  fn a_real_bind_failure_is_not_retried() {
    let mut attempts = 0;
    let err = bind_retrying::<()>("thunet-control", Some(Duration::from_secs(5)), || {
      attempts += 1;
      Err(BindError::Other("bad pipe DACL".into()))
    })
    .expect_err("a malformed endpoint must surface immediately");
    assert_eq!(err, "bad pipe DACL");
    assert_eq!(attempts, 1, "retrying cannot fix a broken endpoint");
  }

  #[cfg(windows)]
  #[test]
  fn the_machine_pipe_grants_the_service_create_instance_right() {
    let sddl = pipe_sddl(true).expect("machine SDDL");
    for sid in crate::ident::service_sid_strings() {
      assert!(
        sddl.contains(&format!("(A;;GA;;;{sid})")),
        "the service SID must own a full-access ACE: {sddl}"
      );
    }
    // Also prove the composed SDDL is accepted by the pipe API.
    pipe_options("thunet-test", true).expect("machine pipe options");
  }

  #[cfg(windows)]
  #[test]
  fn a_listener_can_serve_more_than_one_client() {
    use std::borrow::Cow;

    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    let name = format!(
      "thunet-test-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    );
    let sid = crate::ident::current_user_sid_string().expect("current user SID");
    let sddl = format!("D:(A;;GA;;;{sid})");
    let sddl = U16CString::from_str(&sddl).unwrap();
    let mut options = PipeListenerOptions::new();
    options.path = Cow::Owned(U16CString::from_str(&format!(r"\\.\pipe\{name}")).unwrap());
    options.security_descriptor = Some(SecurityDescriptor::deserialize(sddl.as_ucstr()).unwrap());
    let listener = options.create_duplex::<Bytes>().expect("listener");

    let mut first = connect(&name).expect("first client");
    let mut accepted = listener.accept().expect("accept first client");
    // Impersonation requires the server to have read from the pipe first, so
    // send the same kind of handshake line a real frontend sends.
    use std::io::Write as _;
    writeln!(first, "{{\"v\":1}}").expect("handshake write");
    let line = read_first_line(&mut accepted).expect("handshake read");
    assert_eq!(line, "{\"v\":1}");
    let peer = peer_of(&accepted);
    assert!(peer.authorized, "peer rejected: {}", peer.label);
    // `accept` creates the next instance; if the DACL denied the server that
    // right, this second connection would block until it timed out.
    let second = connect(&name).expect("second client");
    drop(second);
    drop(first);
  }

  #[test]
  fn a_late_frontend_gets_the_current_state_and_the_log_tail() {
    let hub = Hub::new();
    hub.publish(Ev::State(State::LoggedIn));
    hub.publish(Ev::Env(LoginEnv::Campus));
    hub.publish(Ev::Log(Line::info("first")));
    hub.publish(Ev::Error(Some(Failure::Text("boom".into()))));

    let (_rx, backlog, _live) = hub.attach();
    // Snapshot first, then the replayed log lines.
    assert!(matches!(backlog[0], Ev::State(State::LoggedIn)));
    assert!(matches!(backlog[1], Ev::Env(LoginEnv::Campus)));
    assert!(matches!(&backlog[2], Ev::Error(Some(Failure::Text(e))) if e == "boom"));
    assert!(matches!(backlog.last(), Some(Ev::Log(_))));
  }

  #[test]
  fn every_attached_frontend_receives_events() {
    let hub = Hub::new();
    let (first, _, _live1) = hub.attach();
    let (second, _, _live2) = hub.attach();
    hub.publish(Ev::Notify {
      msg: "hi".into(),
      kind: ToastKind::Ok,
    });
    for events in [first, second] {
      assert!(matches!(events.try_recv(), Ok(Ev::Notify { .. })));
    }
  }

  /// A core that refuses a frontend says why in one warning and then closes.
  /// The CLI drops everything it reads while it waits for the replay to end, so
  /// the reason has to be kept outside the event stream or it is lost.
  #[test]
  fn the_last_warning_is_kept_for_the_error_message() {
    let slot: Mutex<Option<String>> = Mutex::new(None);
    remember_warn(&slot, &Ev::State(State::LoggedIn));
    remember_warn(&slot, &Ev::Log(Line::debug("core: attached frontend")));
    assert_eq!(slot.lock().unwrap().as_deref(), None);

    remember_warn(&slot, &Ev::Log(Line::warn("core rejected frontend: pid 1")));
    // An `INFO` line after it is not "the last thing that went wrong".
    remember_warn(&slot, &Ev::Log(Line::info("later, harmless")));
    assert_eq!(
      slot.lock().unwrap().as_deref(),
      Some("core rejected frontend: pid 1")
    );

    remember_warn(&slot, &Ev::Log(Line::error("fatal")));
    assert_eq!(slot.lock().unwrap().as_deref(), Some("fatal"));
  }

  #[test]
  fn the_replayed_history_is_bounded() {
    let hub = Hub::new();
    for i in 0..HISTORY + 20 {
      hub.publish(Ev::Log(Line::info(format!("line {i}"))));
    }
    let (_rx, backlog, _live) = hub.attach();
    assert_eq!(backlog.len(), HISTORY);
  }

  /// A hosted core (portable mode) has to stop on request *without* ending the
  /// process it lives in, and it has to let go of the endpoint **and** of the
  /// connections it already accepted. Ignored by default because it binds the
  /// live per-user endpoint; run it alone:
  ///
  /// ```text
  /// XDG_RUNTIME_DIR=/tmp/tt cargo test -- --ignored a_hosted_core_stops_without_ending_its_host
  /// ```
  #[test]
  #[ignore = "binds the live core endpoint; run by hand"]
  fn a_hosted_core_stops_without_ending_its_host() {
    // An empty state directory keeps this from picking up the real account:
    // the hosted worker would otherwise use whatever `auto_connect` says.
    // SAFETY: only one test runs in this process (`--ignored` with one name),
    // and this happens before the core thread starts.
    let state = std::env::temp_dir().join(format!("thunet-hosted-{}", std::process::id()));
    unsafe { std::env::set_var("THUNET_STATE_DIR", &state) };

    host_core().expect("hosting the core");
    let mut link = None;
    for _ in 0..50 {
      std::thread::sleep(Duration::from_millis(100));
      if let Some(l) = Link::connect(false) {
        link = Some(l);
        break;
      }
    }
    let link = link.expect("the hosted core never answered");

    // Exactly what the window's *Stop* button sends.
    link.send(Cmd::Shutdown);
    // A hosted core cannot report this over the socket - both halves of the
    // connection live in this process, so no EOF is ever sent. What the window
    // watches instead is the hosting side: the thread ends and the endpoint
    // stops accepting. That is what this asserts.
    let mut released = false;
    for _ in 0..200 {
      if !hosted() && Link::connect(false).is_none() {
        released = true;
        break;
      }
      std::thread::sleep(Duration::from_millis(25));
    }
    assert!(released, "the hosted core kept serving after Stop");
    // And the hosting slot is free again - in this very process, which is the
    // whole point of hosting instead of spawning.
    host_core().expect("hosting again after a stop");
    for _ in 0..100 {
      std::thread::sleep(Duration::from_millis(25));
      if Link::connect(false).is_some() {
        break;
      }
    }
    request_stop();
    for _ in 0..200 {
      if !hosted() {
        break;
      }
      std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!hosted(), "the second hosted core never stopped");
    let _ = std::fs::remove_dir_all(&state);
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
      let _ = serve(crate::config::CoreConfig::load(), false);
    });

    let mut link = None;
    for _ in 0..40 {
      std::thread::sleep(Duration::from_millis(100));
      if let Some(l) = Link::connect(false) {
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
    link.send(Cmd::Refresh { quiet: false });
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
