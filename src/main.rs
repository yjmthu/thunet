// The normal entry point is the tray app, so no console window is created.
// CLI subcommands re-attach to the launching terminal (see `attach_parent_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod config;
mod core;
mod crypto;
mod i18n;
mod ipc;
mod logline;
mod proto;
mod secret;
mod session;
mod shortcut;
mod single;
mod srun;
mod ui;
mod wifi;

use std::io::Write;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use proto::{Cmd as CoreCmd, Ev as CoreEv, ToastKind};

/// GUI-subsystem processes own no console. When started from a terminal we
/// attach to it so the CLI subcommands can still print; when started from
/// Explorer the call simply fails and nothing is shown, which is the point.
#[cfg(windows)]
fn attach_parent_console() {
  use std::os::windows::io::AsRawHandle as _;
  use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
  use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    SetStdHandle,
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

    if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
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
  Login,
  /// Ask the running service to de-authenticate.
  Logout,
  /// Run the background service (no window, no tray).
  ///
  /// Normally started by the systemd user unit or the Windows logon task; the
  /// tray starts it on demand when it is not running yet.
  Core,
  /// Install or control the background service.
  ///
  /// Wraps `systemctl --user` on Linux and `schtasks` on Windows, so the same
  /// verbs work everywhere.
  Service {
    #[arg(value_enum)]
    action: autostart::ServiceAction,
  },
  /// Print the config path and effective settings.
  Config,
  /// Create a desktop shortcut for this executable.
  Shortcut,
}

fn main() {
  // Must happen before clap parses, so `--help`/usage errors are visible too.
  attach_parent_console();
  install_panic_log();

  let cli = Cli::parse();
  if let Err(e) = run(cli) {
    eprintln!("error: {e}");
    std::process::exit(1);
  }
}

fn run(cli: Cli) -> Result<(), String> {
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
    // Make sure a core exists before the window starts looking for one. The
    // window also retries, so failing here only means the log says why later.
    if let Err(e) = ipc::ensure_core() {
      eprintln!("warning: {e}");
    }
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
        CoreEv::Error(Some(e)) => Err(e),
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
        CoreEv::Error(Some(e)) => Err(e),
        _ => Ok(()),
      }
    }
    Cmd::Login => {
      let link = with_core()?;
      // Credentials on the command line still win, but they are handed to the
      // core (in memory only) instead of being used behind its back.
      if cli.username.is_some() || cli.password.is_some() {
        link.send(CoreCmd::SetCredentials {
          username: cfg.username.clone(),
          password: cfg.password.clone(),
        });
      }
      link.send(CoreCmd::Connect);
      settled(&link, Duration::from_secs(40))
    }
    Cmd::Logout => {
      let link = with_core()?;
      link.send(CoreCmd::Disconnect);
      settled(&link, Duration::from_secs(20))
    }
    Cmd::Core => match ipc::serve(cfg)? {
      ipc::Serve::Started => Ok(()),
      ipc::Serve::AlreadyRunning => {
        // Nothing to do: whoever owns the endpoint keeps serving the frontends.
        eprintln!("another core is already running");
        Ok(())
      }
    },
    Cmd::Service { action } => {
      println!("{}", autostart::Autostart::service(action, cli.minimized)?);
      Ok(())
    }
  }
}

/// Connect to the running core, or explain how to start it.
///
/// Every session command goes through the core; none of them opens its own
/// connection to the portal, so there is exactly one owner of the session.
fn with_core() -> Result<ipc::Link, String> {
  let link = ipc::open().ok_or_else(|| {
    "the background service is not running (start it with `thunet core`, \
     or enable autostart with `thunet service enable`)"
      .to_string()
  })?;
  // The core replays its state and log tail on connect; swallow that so the
  // answer to our command cannot be confused with old news.
  let mut quiet = Instant::now();
  while quiet.elapsed() < Duration::from_millis(150) {
    if link.try_recv().is_some() {
      quiet = Instant::now();
    } else {
      std::thread::sleep(Duration::from_millis(10));
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
      return Err("the background service closed the connection".into());
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
    CoreEv::Error(Some(e)) => Err(e),
    _ => Ok(()),
  }
}
