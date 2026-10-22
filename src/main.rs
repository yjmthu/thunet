// The normal entry point is the tray app, so no console window is created.
// CLI subcommands re-attach to the launching terminal (see `attach_parent_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod config;
mod core;
mod crypto;
mod diag;
mod elevate;
mod i18n;
mod ident;
mod ipc;
mod logline;
mod prompt;
mod proto;
mod secret;
mod session;
mod shortcut;
mod single;
mod srun;
mod ui;
mod wifi;
#[cfg(windows)]
mod winsvc;

use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser, Subcommand};

use proto::{Cmd as CoreCmd, Ev as CoreEv, ToastKind};

/// Where the elevated helper writes the error from [`run`], when the caller
/// passed `--admin-error`. The helper has no console of its own.
static ADMIN_ERROR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// GUI-subsystem processes own no console. When started from a terminal we
/// attach to it so the CLI subcommands can still print; when started from
/// Explorer the call simply fails and nothing is shown, which is the point.
#[cfg(windows)]
fn attach_parent_console() {
  use std::os::windows::io::AsRawHandle as _;
  use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
  use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetStdHandle,
  };

  /// A handle the launcher already set up (console, pipe or redirected file)
  /// must win over `CONOUT$`: replacing it makes `thunet --version > file`
  /// write to the console and leave the file empty.
  fn inherited(handle: windows_sys::Win32::Foundation::HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
  }

  // SAFETY: plain Win32 calls; the opened handle is leaked on purpose so that
  // stdout/stderr stay valid for the rest of the process.
  unsafe {
    // Read the handles *before* attaching: `AttachConsole` may replace the
    // standard handles with the console's, which would undo a redirection.
    let out_before = GetStdHandle(STD_OUTPUT_HANDLE);
    let err_before = GetStdHandle(STD_ERROR_HANDLE);
    let in_before = GetStdHandle(STD_INPUT_HANDLE);

    // Join the launching terminal. When there is no parent console, a console
    // this process owns itself (`CREATE_NEW_CONSOLE`, `start`) is the other
    // possibility: the console devices open exactly when one exists, so they
    // are both the test and the fix - a GUI process starts without usable
    // standard handles, and `thunet login` has to read from one.
    let joined = AttachConsole(ATTACH_PARENT_PROCESS) != 0;

    // `thunet login` asks questions, so stdin has to be the console too: a
    // prompt that reads from an invalid handle looks like an ignored Enter.
    if inherited(in_before) {
      SetStdHandle(STD_INPUT_HANDLE, in_before);
    } else if let Ok(file) = std::fs::OpenOptions::new().read(true).open("CONIN$") {
      SetStdHandle(
        STD_INPUT_HANDLE,
        file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
      );
      std::mem::forget(file);
    } else if !joined {
      // No console at all: nothing to print to and nothing to ask on. This is
      // the tray started from Explorer.
      return;
    }

    // Put back whatever the launcher provided (console, pipe or file).
    if inherited(out_before) {
      SetStdHandle(STD_OUTPUT_HANDLE, out_before);
    }
    if inherited(err_before) {
      SetStdHandle(STD_ERROR_HANDLE, err_before);
    }
    if inherited(out_before) && inherited(err_before) {
      return;
    }

    let Ok(file) = std::fs::OpenOptions::new().write(true).open("CONOUT$") else {
      return;
    };
    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if !inherited(out_before) {
      SetStdHandle(STD_OUTPUT_HANDLE, handle);
    }
    if !inherited(err_before) {
      SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    std::mem::forget(file);
  }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// Path to the executable on disk.
///
/// After an in-place upgrade the running image is unlinked, and on Linux
/// `/proc/self/exe` then reads `"/usr/bin/thunet (deleted)"`. Spawning or
/// registering that path fails, so the suffix is stripped and the current file
/// on disk is used instead.
pub fn exe_path() -> Option<std::path::PathBuf> {
  Some(without_deleted_suffix(std::env::current_exe().ok()?))
}

/// `/proc/self/exe` reads `"/usr/bin/thunet (deleted)"` once the file was
/// replaced in place (an upgrade); the path on disk is what we want.
#[cfg(target_os = "linux")]
fn without_deleted_suffix(exe: std::path::PathBuf) -> std::path::PathBuf {
  match exe.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
    Some(path) => std::path::PathBuf::from(path),
    None => exe,
  }
}

#[cfg(not(target_os = "linux"))]
fn without_deleted_suffix(exe: std::path::PathBuf) -> std::path::PathBuf {
  exe
}

/// With no console, a panic would vanish; keep it on disk instead.
fn install_panic_log() {
  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    let dir = config::dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(dir.join("crash.log"))
    {
      let _ = writeln!(
        f,
        "[{}] {info}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
      );
      let _ = writeln!(f, "{}", std::backtrace::Backtrace::force_capture());
    }
    previous(info);
  }));
}

#[derive(Parser)]
#[command(
  name = "thunet",
  version,
  about = "Minimal THUnet / SRun3K portal client"
)]
struct Cli {
  /// Portal host (overrides the config file).
  #[arg(long, global = true)]
  host: Option<String>,
  /// Use plain http instead of https.
  #[arg(long, global = true)]
  http: bool,
  /// Request timeout in seconds.
  #[arg(long, global = true, default_value_t = 5)]
  timeout: u64,
  /// TUNET account (overrides the saved one).
  #[arg(short, long, global = true)]
  username: Option<String>,
  /// Password (otherwise read from the saved credential, or prompted).
  #[arg(short, long, global = true)]
  password: Option<String>,
  /// Local IP to authenticate; empty lets the portal infer it.
  #[arg(long, global = true, default_value = "")]
  ip: String,
  /// ac_id override; empty means auto-detect.
  #[arg(long, global = true, default_value = "")]
  ac_id: String,
  /// Start minimised to the tray (used by the autostart entry).
  #[arg(long, global = true)]
  minimized: bool,
  /// Where `config.json` and `credential.bin` live. The Windows service is
  /// registered with the installing user's directory, because the token it
  /// runs under has no profile of its own.
  #[arg(long, global = true, hide = true)]
  state_dir: Option<std::path::PathBuf>,
  /// Act as if `THUNET_STATE_DIR` were unset: talk to the machine setup only.
  ///
  /// The Windows installer passes this, because an immediate custom action
  /// inherits the environment of whatever started msiexec, and a leftover
  /// portable variable would silently point `quit` at the wrong endpoint. It is
  /// also the escape hatch for `thunet service …` from a portable shell, where
  /// those verbs are refused. `core --machine` (what the Linux unit runs) means
  /// the same thing, plus the machine state directory.
  #[arg(long, global = true, hide = true)]
  machine: bool,
  /// Internal: file where the elevated helper records a failure.
  #[arg(long, global = true, hide = true)]
  admin_error: Option<std::path::PathBuf>,

  #[command(subcommand)]
  cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
  /// Offline protocol self-test (see `cargo test`).
  Selftest,
  /// Print the portal ac_id the running service is using.
  AcId,
  /// Show portal session / traffic information.
  Status,
  /// Ask the running service to authenticate now.
  ///
  /// Started from a terminal it asks for the account, the password (not echoed)
  /// and whether to keep the password; `--username`/`--password` skip the
  /// questions and use that pair for this session only.
  Login,
  /// Ask the running service to de-authenticate.
  Logout,
  /// Run the background service (no window, no tray).
  ///
  /// Normally started by the systemd system unit or the Windows service
  /// manager, or on demand with `thunet service start` / the status page's
  /// *Start* button.
  Core {
    /// Started by the Windows service manager; not for humans.
    #[arg(long, hide = true)]
    service: bool,
  },
  /// Install or control the background service.
  ///
  /// Wraps `systemctl` on Linux and the Service Control Manager on Windows, so
  /// the same verbs work everywhere. Changing the registration, the start type
  /// or the running state needs administrator rights on both platforms, which
  /// this asks for once.
  ///
  /// A portable setup (`THUNET_STATE_DIR`) has no service: there only `status`
  /// runs, and every other verb is refused.
  Service {
    #[arg(value_enum)]
    action: autostart::ServiceAction,
  },
  /// Perform one privileged service operation (the elevated half of
  /// `service <action>`, not for humans).
  #[command(hide = true)]
  ServiceAdmin {
    #[arg(value_enum)]
    action: autostart::ServiceAction,
  },
  /// Restart the core after an upgrade if it starts at boot (installer
  /// internal).
  #[command(hide = true)]
  ServiceResume,
  /// Delete the machine state directory after a real uninstall (installer
  /// internal; never called for an upgrade or repair).
  #[command(hide = true)]
  ServicePurge,
  /// Ask the running window to quit, then wait until it is really gone.
  ///
  /// Closing the window only hides it — that is what a tray app has to do — and
  /// killing the process skips its shutdown. An installer therefore asks here
  /// instead: the window exits through its own event loop and this command
  /// polls the single-instance name until it disappears.
  Quit {
    /// Give up (and report) after this many milliseconds.
    #[arg(long, default_value_t = 20_000)]
    timeout: u64,
    /// Ask the core to end every attached window, not just this session's.
    ///
    /// The per-session doorbell cannot cross a session, so windows in other
    /// sessions or other users' accounts survive an upgrade that leaves their
    /// executable replaced — an old window driving a new core is what the
    /// protocol check has to refuse. The core already fans its events out to
    /// every attached frontend, so one request ends them all.
    #[arg(long)]
    all_sessions: bool,
  },
  /// Print the config path and effective settings.
  Config,
  /// Create a desktop shortcut for this executable.
  Shortcut,
  /// Print a shell completion script.
  ///
  /// Write it where the shell looks for completions, for example
  /// `thunet completions bash > ~/.local/share/bash-completion/completions/thunet`.
  Completions {
    #[arg(value_enum)]
    shell: clap_complete::Shell,
  },
}

fn main() {
  // Must happen before clap parses, so `--help`/usage errors are visible too.
  attach_parent_console();
  install_panic_log();

  let cli = Cli::parse();
  if let Some(path) = &cli.admin_error {
    let _ = ADMIN_ERROR.set(path.clone());
  }
  if let Err(e) = run(cli) {
    if let Some(path) = ADMIN_ERROR.get() {
      let _ = std::fs::write(path, &e);
    }
    eprintln!("error: {e}");
    std::process::exit(1);
  }
}

fn run(cli: Cli) -> Result<(), String> {
  // `--machine` is the one way to say "this process is not portable", whatever
  // the environment says: an installer inherits the environment of its caller,
  // and a leftover `THUNET_STATE_DIR` would silently retarget its commands at a
  // portable endpoint that nothing is listening on. Clearing the variable here
  // is enough - `portable()`, `open_result` and the single-instance names all
  // derive from it, so every downstream decision follows.
  if cli.machine {
    if cli.state_dir.is_some() {
      return Err("`--machine` and `--state-dir` contradict each other".into());
    }
    // SAFETY: this runs on the main thread before anything reads the
    // environment (no other thread exists yet), and `config::dir` is only
    // reached below.
    unsafe { std::env::remove_var("THUNET_STATE_DIR") };
  }
  // The Windows service runs as SYSTEM, whose own `%APPDATA%` is the system
  // profile; `sc create` therefore passes the installing user's directory.
  if let Some(dir) = &cli.state_dir {
    // SAFETY: this runs on the main thread before anything reads the
    // environment (no other thread exists yet, and `config::dir` is only
    // reached below).
    unsafe { std::env::set_var("THUNET_STATE_DIR", dir) };
  }
  // Effective settings: config file first, explicit CLI flags win.
  let mut cfg = config::Config::load();
  if let Some(h) = cli.host.clone() {
    cfg.host = h;
  }
  if cli.http {
    cfg.https = false;
  }
  if let Some(u) = cli.username.clone() {
    cfg.username = u;
  }
  if let Some(p) = cli.password.clone() {
    cfg.password = p;
  }
  if !cli.ac_id.is_empty() {
    cfg.ac_id = cli.ac_id.clone();
  }
  if !cli.ip.is_empty() {
    cfg.ip = cli.ip.clone();
  }

  // No subcommand -> run the tray/window app. The HTTP agent is only built for
  // the CLI subcommands below, so launching the GUI does no network setup on
  // the startup path (the worker thread owns its own client).
  let Some(cmd) = cli.cmd else {
    // A second launch only asks the running copy to show its window.
    let Some(_instance) = single::acquire() else {
      return Ok(());
    };
    // `--minimized` (the autostart entry passes it) and the saved setting are
    // equivalent: a launch without the flag still starts in the tray when the
    // user asked for that in the settings page.
    let minimized = cli.minimized || cfg.start_minimized;
    // The window never starts a service by itself: it only talks to one that
    // already answers (autostart at boot, `thunet service start`, or the
    // status page's *Start* button). Starting one here would make a deliberate
    // `systemctl stop` look broken the next time the window opens.
    return ui::run(cfg, minimized);
  };

  match cmd {
    Cmd::Selftest => {
      println!("protocol vectors live in `cargo test` (5 tests)");
      Ok(())
    }
    Cmd::Config => {
      println!("path: {}", config::path().display());
      let mut masked = cfg.clone();
      if !masked.password.is_empty() {
        masked.password = "***".into();
      }
      println!("{}", serde_json::to_string_pretty(&masked).unwrap());
      Ok(())
    }
    Cmd::Shortcut => {
      shortcut::create_desktop_shortcut()?;
      println!("desktop shortcut created");
      Ok(())
    }
    Cmd::Completions { shell } => {
      // Generated from the same `clap` definition the parser uses, so the
      // script can never drift from the real flags and subcommands. Rendering
      // into a buffer first keeps `thunet completions bash | head` from
      // panicking on the closed pipe `head` leaves behind.
      let mut cmd = Cli::command();
      let mut script = Vec::new();
      clap_complete::generate(shell, &mut cmd, "thunet", &mut script);
      let _ = std::io::stdout().write_all(&script);
      Ok(())
    }
    Cmd::AcId => {
      let link = with_core()?;
      link.send(CoreCmd::AcId);
      match wait_for(&link, Duration::from_secs(10), |ev| {
        matches!(ev, CoreEv::AcId(_) | CoreEv::Error(Some(_)))
      })? {
        CoreEv::AcId(id) => {
          println!("{id}");
          Ok(())
        }
        CoreEv::Error(Some(e)) => Err(e.to_string()),
        _ => Ok(()),
      }
    }
    Cmd::Status => {
      let link = with_core()?;
      link.send(CoreCmd::Status);
      match wait_for(&link, Duration::from_secs(15), |ev| {
        matches!(ev, CoreEv::Status(_) | CoreEv::Error(Some(_)))
      })? {
        CoreEv::Status(v) => {
          println!("{}", serde_json::to_string_pretty(&v).unwrap());
          // The core part is always there; only the portal half can fail.
          match v.get("portal_error").and_then(|e| e.as_str()) {
            Some(e) => Err(e.to_string()),
            None => Ok(()),
          }
        }
        CoreEv::Error(Some(e)) => Err(e.to_string()),
        _ => Ok(()),
      }
    }
    Cmd::Login => {
      let link = with_core()?;
      // Interactive unless the pair came in on the command line: typing the
      // password is the only way it stays out of the shell history and out of
      // every process listing.
      let interactive = cli.username.is_none() && cli.password.is_none() && prompt::available();
      if interactive || cli.username.is_some() || cli.password.is_some() {
        let (username, password, remember) = if interactive {
          ask_credentials(&cfg)?
        } else {
          (cfg.username.clone(), cfg.password.clone(), false)
        };
        link.send(CoreCmd::SetCredentials {
          username,
          password,
          remember,
          // False: a password answered with Enter is empty, and the core then
          // keeps the one it already holds (see `Cmd::SetCredentials`).
          keep_password: false,
        });
        // The answer says whether the pair was actually stored, which is the
        // difference the user asked about; staying silent would let a typo in
        // the account name pass as "saved".
        match wait_for(&link, Duration::from_secs(10), |ev| {
          matches!(ev, CoreEv::Credentials(_) | CoreEv::Error(Some(_)))
        })? {
          CoreEv::Credentials(state) => println!("{}", credential_line(&state, remember)),
          CoreEv::Error(Some(e)) => return Err(e.to_string()),
          _ => return Err("the service did not confirm the credentials".into()),
        }
      }
      link.send(CoreCmd::Connect);
      settled(&link, Duration::from_secs(40))
    }
    Cmd::Logout => {
      let link = with_core()?;
      link.send(CoreCmd::Disconnect);
      settled(&link, Duration::from_secs(20))
    }
    Cmd::Core { service } => {
      #[cfg(windows)]
      if service {
        return winsvc::run();
      }
      // `--service` is the Windows SCM entry point, and it has already been
      // handled above; the mode itself is always `--machine`, on both
      // platforms, so a portable core is what a bare `thunet core` starts.
      #[cfg(not(windows))]
      let _ = service;
      let machine = cli.machine;
      if machine && std::env::var_os("THUNET_STATE_DIR").is_none() {
        // The service manager passes `--state-dir`; this is the fallback for
        // an administrator-started core that did not.
        // SAFETY: this is the single-threaded startup path, before the worker
        // or any frontend thread exists.
        unsafe { std::env::set_var("THUNET_STATE_DIR", config::machine_dir()) };
      }
      // The machine core has no stderr anybody can read, and this runs before
      // the config is loaded: a failure while opening the sealed credential
      // would otherwise go to a place nobody can see. Point diagnostics at the
      // log file here, first. `diag::line` still writes to stderr as well, so
      // under systemd the journal keeps its copy; on Windows this file is the
      // only copy (`winsvc` sets the same path again).
      if machine {
        diag::set_file(config::machine_dir().join("core.log"));
      }
      let cfg = if machine {
        config::Config::load_machine()
      } else {
        config::Config::load()
      };
      match ipc::serve(cfg, machine)? {
        ipc::Serve::Started => Ok(()),
        ipc::Serve::AlreadyRunning => {
          // Nothing to do: whoever owns the endpoint keeps serving the frontends.
          eprintln!("another core is already running");
          Ok(())
        }
      }
    }
    Cmd::Service { action } => {
      // Portable mode has no service, so every mutating verb here would act on
      // the *machine* service instead - the one thing a portable setup is
      // defined by not using, and `service start` would even put a second core
      // on the same portal session. Refuse, and name both ways out: the core
      // belongs to the window, and the machine service is only meaningful
      // without `THUNET_STATE_DIR`. `status` is a read-only query and still
      // runs; `service-admin` is deliberately not gated (see its arm below).
      if ipc::portable() && autostart::service_refused_in_portable(action) {
        return Err(
          "portable mode has no service: the core runs inside the window, so it lives exactly as \
           long as the window does. Closing the window only hides both, and `thunet quit` (or \
           Quit in the tray menu) ends them. To act on the machine service, run \
           `thunet --machine service …`"
            .into(),
        );
      }
      println!("{}", autostart::Autostart::service(action, cli.minimized)?);
      Ok(())
    }
    // `--machine` is how the two legitimate callers arrive: `elevate` starts the
    // helper with it, and the Windows installer passes it to its custom
    // actions. Both need the machine service, and neither may inherit the mode
    // of whatever started it. Anything else that turns up portable is a person
    // typing the hidden command in a portable shell - where there is no service
    // to act on, and `--machine` is the explicit way to say they mean the
    // machine one.
    Cmd::ServiceAdmin { action } => {
      if ipc::portable() {
        return Err(
          "portable mode has no service, and this is the machine-only half of `service`. \
           `thunet --machine service-admin …` acts on the machine service explicitly"
            .into(),
        );
      }
      autostart::service_admin(action)
    }
    Cmd::ServiceResume => autostart::Autostart::resume_after_update(),
    Cmd::ServicePurge => autostart::purge_machine_state(),
    Cmd::Quit {
      timeout,
      all_sessions,
    } => {
      let timeout = Duration::from_millis(timeout);
      if all_sessions {
        quit_all(timeout)?;
      }
      // This session's own window, and the wait an installer needs before it
      // may replace the file. A window that was attached to the core is
      // already gone, so this normally returns at once.
      single::quit_and_wait(timeout)
    }
  }
}

/// Ask the core to end every attached window, and wait until they are gone.
///
/// Only the core can reach windows in other sessions: the per-session doorbell
/// (`Local\` event, runtime marker) is per session by construction. The wait is
/// a fact rather than a guess — `Cmd::CoreInfo` reports how many frontends are
/// still attached, and this connection is one of them.
fn quit_all(timeout: Duration) -> Result<(), String> {
  /// How long one question gets. The core answers from its own loop, so a
  /// single answer can take a beat; a slow answer means "ask again", never
  /// "nothing is attached", which is why this is not the overall budget.
  const QUERY: Duration = Duration::from_millis(500);
  /// Polling interval while waiting for that answer. The answer arrives over a
  /// socket, so this only has to stay below "instant"; 25 ms is still 40
  /// wake-ups per second against replies that take hundreds of milliseconds,
  /// so the interval is never what the user waits for.
  const POLL: Duration = Duration::from_millis(25);

  let link = match ipc::open_result() {
    Ok(link) => link,
    // No core: nobody can be told, and the per-session doorbell the caller
    // rings afterwards is everything that is left.
    Err(_) => return Ok(()),
  };
  link.send(CoreCmd::QuitAll);
  let deadline = Instant::now() + timeout;
  // Whether any answer actually said "more than just me is attached". The two
  // ways this command can fail are not the same thing: a window that refuses
  // to quit is a fact, while a core that never answered is only the absence of
  // one - and the caller (an installer) must not be told a guess.
  let mut saw_attached = false;
  loop {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
      return Err(if saw_attached {
        format!("windows are still attached after {}", brief(timeout))
      } else {
        format!(
          "no answer from the background service within {}",
          brief(timeout)
        )
      });
    }
    link.send(CoreCmd::CoreInfo);
    let ask_until = Instant::now() + remaining.min(QUERY);
    // Wait for the answer to *this* question only. The log replay, the
    // broadcast's own `Quit` and anything else on the wire is not news for a
    // command that exists to end windows, so nothing is echoed here (an
    // installer would only have to throw it away).
    let answer = loop {
      match link.try_recv() {
        Some(CoreEv::Core(info)) => break Ok(Some(info)),
        Some(CoreEv::Error(Some(e))) => return Err(e.to_string()),
        Some(_) => continue,
        // The core went away (stopped, or being replaced): nothing is attached
        // to it any more.
        None if !link.alive() => break Ok(None),
        // No answer within the question's own budget: the budget for the whole
        // command may still be there, so try again.
        None if Instant::now() >= ask_until => break Err(()),
        None => std::thread::sleep(POLL),
      }
    };
    match answer {
      // This command counts as a frontend, so one attached means "all done".
      Ok(Some(info)) if info.frontends > 1 => {
        saw_attached = true;
        continue;
      }
      Ok(_) => return Ok(()),
      Err(()) => continue,
    }
  }
}

/// "800 ms" / "15 s" — a timeout the user reads, rather than `Duration`'s
/// debug form (`400ms`) or a `0 s` that hides a sub-second budget.
fn brief(d: Duration) -> String {
  if d.as_secs() >= 1 {
    format!("{} s", d.as_secs())
  } else {
    format!("{} ms", d.as_millis())
  }
}

/// Connect to the running core, or explain how to start it.
///
/// Every session command goes through the core; none of them opens its own
/// connection to the portal, so there is exactly one owner of the session.
fn with_core() -> Result<ipc::Link, String> {
  let link = ipc::open_result().map_err(|reason| {
    // The reason matters: "nobody listens" and "something answered but is not
    // the service" are different problems, and the second one is exactly what
    // a trust anchor that cannot be read looks like. The hint has to match the
    // mode: portable mode has no service, and `thunet service enable` is
    // refused there, so pointing at it would send the user to a dead end. It
    // also has to name the state directory: a bare `thunet` is machine mode,
    // so "start the window" only helps if it is the same portable setup.
    let hint = if ipc::portable() {
      let dir = crate::config::dir();
      format!(
        "portable mode: the core runs inside the window - start it with `thunet --state-dir {}`, \
         or run `thunet core --state-dir {}` for one without a window",
        dir.display(),
        dir.display()
      )
    } else {
      "start it with `thunet core`, or enable autostart with `thunet service enable`".to_string()
    };
    format!("the background service is not reachable ({hint}): {reason}")
  })?;
  // The core replays its state, the log tail and any active banner on connect.
  // Everything that arrives before the answer to this marker command is that
  // replay: waiting for the marker instead of guessing a delay keeps a banner
  // that is still up from being mistaken for the answer to the real command.
  link.send(CoreCmd::CoreInfo);
  let deadline = Instant::now() + Duration::from_secs(5);
  while Instant::now() < deadline {
    match link.try_recv() {
      Some(CoreEv::Core(_)) => break,
      Some(_) => continue,
      None if !link.alive() => break,
      None => std::thread::sleep(Duration::from_millis(10)),
    }
  }
  Ok(link)
}

/// Wait until an event satisfies `done`, echoing the core's log lines.
fn wait_for<F>(link: &ipc::Link, timeout: Duration, mut done: F) -> Result<CoreEv, String>
where
  F: FnMut(&CoreEv) -> bool,
{
  let deadline = Instant::now() + timeout;
  loop {
    while let Some(ev) = link.try_recv() {
      if let CoreEv::Log(line) = &ev {
        println!("{}", line.render());
      }
      if done(&ev) {
        return Ok(ev);
      }
    }
    if !link.alive() {
      return Err(match link.speaks() {
        // The core refused every line this build sent, so nothing will work
        // until the service is restarted on a matching build.
        Some(v) => format!(
          "the background service speaks protocol {v} but this build speaks {}; restart the service",
          proto::VERSION
        ),
        None => "the background service closed the connection".into(),
      });
    }
    if Instant::now() >= deadline {
      return Err(format!(
        "no answer from the background service within {}s",
        timeout.as_secs()
      ));
    }
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// Ask for the account, the password and whether to keep it.
///
/// The stored account is offered as the default so that Enter accepts it; an
/// empty password answer means "keep the one the core already holds", which is
/// what makes it possible to change only the *remember* choice without typing
/// the secret again.
fn ask_credentials(cfg: &config::Config) -> Result<(String, String, bool), String> {
  prompt::arm_cancel();
  let default = cfg.username.trim();
  let question = if default.is_empty() {
    "THUnet account: ".to_string()
  } else {
    format!("THUnet account [{default}]: ")
  };
  let answer = prompt::line(&question)?;
  let username = if answer.is_empty() {
    default.to_string()
  } else {
    answer
  };
  if username.is_empty() {
    return Err("no account given".into());
  }
  let password = prompt::secret("Password (Enter keeps the stored one): ")?;
  let answer = prompt::line("Remember the password for the next start? [y/N] ")?;
  let remember = matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes");
  Ok((username, password, remember))
}

/// One line about the pair the core now holds: which account, and whether it
/// survives a restart. The core answers `SetCredentials` even when it stored
/// nothing (an incomplete pair, or no portal on this network), so the CLI has
/// to say which of the two happened instead of assuming success.
fn credential_line(state: &proto::CredentialState, remember: bool) -> String {
  let account = if state.username.is_empty() {
    "(none)".to_string()
  } else {
    state.username.clone()
  };
  match (remember, state.remembered, state.has_password) {
    (true, true, _) => format!("credentials: {account}, saved for the next start"),
    (true, false, _) => {
      format!("credentials: {account}, NOT saved (a complete --username/--password pair is needed)")
    }
    (false, true, _) => {
      format!("credentials: {account}, this session only (the stored copy is untouched)")
    }
    (false, _, true) => format!("credentials: {account}, this session only"),
    (false, _, false) => format!("credentials: {account}, nothing to log in with"),
  }
}

/// Wait for the outcome of a Connect/Disconnect and print it.
fn settled(link: &ipc::Link, timeout: Duration) -> Result<(), String> {
  match wait_for(link, timeout, |ev| {
    matches!(
      ev,
      CoreEv::State(session::State::Online) | CoreEv::Notify { .. } | CoreEv::Error(Some(_))
    )
  })? {
    CoreEv::State(session::State::Online) => {
      println!("online");
      Ok(())
    }
    CoreEv::Notify {
      msg,
      kind: ToastKind::Err,
    } => Err(msg),
    CoreEv::Notify { msg, .. } => {
      println!("{msg}");
      Ok(())
    }
    CoreEv::Error(Some(e)) => Err(e.to_string()),
    _ => Ok(()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The `.deb` installs the scripts under `assets/completions/`, and a
  /// checked-in copy can only stay honest if something compares it with the
  /// real CLI definition.
  #[test]
  fn checked_in_completion_scripts_match_the_cli() {
    let cases = [
      (
        clap_complete::Shell::Bash,
        "thunet.bash",
        include_str!("../assets/completions/thunet.bash"),
      ),
      (
        clap_complete::Shell::Zsh,
        "_thunet",
        include_str!("../assets/completions/_thunet"),
      ),
      (
        clap_complete::Shell::Fish,
        "thunet.fish",
        include_str!("../assets/completions/thunet.fish"),
      ),
    ];
    for (shell, file, expected) in cases {
      let mut cmd = Cli::command();
      let mut out = Vec::new();
      clap_complete::generate(shell, &mut cmd, "thunet", &mut out);
      let generated = String::from_utf8(out).expect("completion scripts are UTF-8");
      assert_eq!(
        generated, expected,
        "assets/completions/{file} is stale; regenerate it with \
         `cargo run -- completions {shell} > assets/completions/{file}`"
      );
    }
  }

  #[test]
  fn an_unlinked_image_resolves_to_the_path_on_disk() {
    // Right after an in-place upgrade `/proc/self/exe` keeps the marker.
    #[cfg(target_os = "linux")]
    assert_eq!(
      without_deleted_suffix(std::path::PathBuf::from("/usr/bin/thunet (deleted)")),
      std::path::PathBuf::from("/usr/bin/thunet")
    );
    assert_eq!(
      without_deleted_suffix(std::path::PathBuf::from("/usr/bin/thunet")),
      std::path::PathBuf::from("/usr/bin/thunet")
    );
  }
}
