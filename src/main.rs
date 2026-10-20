// The normal entry point is the tray app, so no console window is created.
// CLI subcommands re-attach to the launching terminal (see `attach_parent_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod config;
mod crypto;
mod i18n;
mod logline;
mod secret;
mod session;
mod shortcut;
mod single;
mod srun;
mod ui;
mod wifi;

use std::io::Write;
use std::time::Duration;

use clap::{Parser, Subcommand};

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
  /// Password (otherwise read from Windows Credential Manager, or prompted).
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
  /// Print the campus ac_id.
  AcId,
  /// Show portal session / traffic information.
  Status,
  /// Authenticate once (no-op when the portal already reports a session).
  Login {
    /// Re-authenticate even if a session already exists.
    #[arg(long)]
    force: bool,
  },
  /// De-authenticate.
  Logout,
  /// Touch a URL periodically to keep the session alive.
  Keepalive {
    #[arg(long, default_value = "https://www.tsinghua.edu.cn/")]
    target: String,
    #[arg(long, default_value_t = 30)]
    interval: u64,
    #[arg(long, default_value_t = 1)]
    count: u64,
  },
  /// Auto-connect and stay online using the config file (headless).
  Run {
    /// Do a single ensure-online step and exit.
    #[arg(long)]
    once: bool,
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
    return ui::run(cfg, minimized);
  };

  let client = srun::Client::new(&cfg.host, cfg.https, Duration::from_secs(cli.timeout));

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
      println!("{}", client.ac_id()?);
      Ok(())
    }
    Cmd::Status => {
      let (ac_id, _) = resolve_ac_id(&client, &cfg.ac_id);
      println!(
        "{}",
        serde_json::to_string_pretty(&client.status(&ac_id)?).unwrap()
      );
      Ok(())
    }
    Cmd::Keepalive {
      target,
      interval,
      count,
    } => {
      for i in 1..=count {
        match client.touch(&target) {
          Ok(n) => println!("[{i}/{count}] keepalive ok {target} ({n} bytes)"),
          Err(e) => println!("[{i}/{count}] keepalive failed: {e}"),
        }
        if i < count {
          std::thread::sleep(Duration::from_secs(interval));
        }
      }
      Ok(())
    }
    cmd @ (Cmd::Login { .. } | Cmd::Logout) => {
      let logout = matches!(cmd, Cmd::Logout);
      let force = matches!(cmd, Cmd::Login { force: true });
      let (mut ac_id, _) = resolve_ac_id(&client, &cfg.ac_id);
      let was_online = client.is_online(&ac_id);
      // `logout` must also work for a session this profile did not create, and
      // the account the portal reports for this address wins over the saved one
      // (they differ on a shared machine).
      let online = if logout {
        client.online_identity(&cfg.ip)
      } else {
        None
      };
      let username = match &online {
        Some((user, _)) => user.clone(),
        None => cfg.username.clone(),
      };
      if username.is_empty() {
        return Err("missing --username / saved credential / config".into());
      }
      let ip = match &online {
        Some((_, addr)) => addr.clone(),
        None => cfg.ip.clone(),
      };
      if !logout && !force && was_online {
        println!("already online (use `login --force` to re-authenticate)");
        return Ok(());
      }
      let password = if logout {
        None
      } else {
        Some(if cfg.password.is_empty() {
          prompt_password()?
        } else {
          cfg.password.clone()
        })
      };
      let mut attempt = 0;
      loop {
        attempt += 1;
        let v = client.login_logout(&username, password.as_deref(), &ip, &ac_id, logout)?;
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        if v.get("error").and_then(|e| e.as_str()) == Some("ok") {
          return Ok(());
        }
        if logout {
          return Err(format!("portal rejected logout: {v}"));
        }
        // Only policy-family errors may still resolve into a session; a
        // credential failure (E2531/E2553/E2901) will never do so.
        let ecode = v.get("ecode").and_then(|e| e.as_str()).unwrap_or("?");
        let policy_like = ecode.starts_with("E28") || ecode == "?" || ecode.is_empty();
        if was_online || !policy_like {
          return Err(format!("portal rejected: {v}"));
        }
        // The campus network may have moved us to another AC: re-detect
        // and retry once when the authoritative id actually changed.
        if attempt < 2 {
          if let Ok((fresh, true)) = client.detect_ac_id() {
            if fresh != ac_id {
              eprintln!("ac_id changed {ac_id} -> {fresh}, retrying");
              ac_id = fresh;
              continue;
            }
          }
        }
        eprintln!("note: server reported {ecode}; waiting up to 20s for the session to come up...");
        if client.wait_online(&ac_id, Duration::from_secs(20)) {
          println!(
            "note: server reported {ecode}, but the session is active (treating as success)"
          );
          return Ok(());
        }
        return Err(format!("portal rejected: {v}"));
      }
    }
    Cmd::Run { once } => {
      if cfg.username.is_empty() || cfg.password.is_empty() {
        return Err(format!(
          "config incomplete: fill username/password in {}",
          config::path().display()
        ));
      }
      let mut session = session::Session::new(cfg.clone());
      if once {
        println!("{}", session.ensure_online()?);
        return Ok(());
      }
      session.run_forever(|s, msg| {
        let state = match s.state {
          session::State::Online => "online",
          session::State::Offline => "offline",
          session::State::Authenticating => "authing",
        };
        println!("[{}] {state}: {msg}", now_hms());
      });
    }
  }
}

fn resolve_ac_id(client: &srun::Client, configured: &str) -> (String, bool) {
  if !configured.is_empty() && configured != "auto" {
    return (configured.to_string(), true);
  }
  match client.detect_ac_id() {
    Ok((id, confident)) => {
      if confident {
        println!("ac_id={id} (captive portal)");
      } else {
        eprintln!(
          "warning: ac_id={id} came from the default page and may be wrong; \
                     pass --ac-id <value> if login fails"
        );
      }
      (id, confident)
    }
    Err(e) => {
      eprintln!("ac_id detect failed ({e}), falling back to 1");
      ("1".into(), false)
    }
  }
}

fn now_hms() -> String {
  let secs = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  let t = secs % 86_400;
  format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

fn prompt_password() -> Result<String, String> {
  eprint!("TUNET password: ");
  std::io::stderr().flush().ok();
  let mut buf = String::new();
  std::io::stdin()
    .read_line(&mut buf)
    .map_err(|e| e.to_string())?;
  Ok(buf.trim_end_matches(['\r', '\n']).to_string())
}
