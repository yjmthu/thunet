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
mod icons;
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
mod theme;
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
    // possibility: `CONOUT$` below opens exactly when one exists, so it is both
    // the test and the fix - a GUI process starts without usable standard
    // handles, and a subcommand has to print somewhere. With no console at all
    // (the tray started from Explorer) it cannot be opened and nothing is
    // shown, which is the point.
    let _ = AttachConsole(ATTACH_PARENT_PROCESS);

    // Put back whatever the launcher provided (console, pipe or file).
    //
    // stdin is only ever put *back*: attaching points it at the console, which
    // would hide the pipe `thunet login` reads its three lines from. Nothing
    // reads the console itself, so a launch without an inherited stdin (a bare
    // shell, the tray) keeps the console handle and the login command sees a
    // terminal and reads nothing - exactly what it should do there.
    if inherited(in_before) {
      SetStdHandle(STD_INPUT_HANDLE, in_before);
    }
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
  /// Internal: raise the machine-wide quit signal and exit.
  ///
  /// This is the elevated half of `quit --all-sessions`: it needs administrator
  /// rights, does no waiting and starts no window, and the caller keeps the
  /// real budget for the windows in its own session.
  #[arg(long, global = true, hide = true)]
  signal_only: bool,

  #[command(subcommand)]
  cmd: Option<Cmd>,
}

/// What `thunet login --help` shows: the three lines and the two commands.
///
/// It stays short on purpose: each command is printed by a flag, and `-v` carries
/// the reasoning. Nothing here guesses which shell is running - a windowed
/// program cannot tell, and the reader knows.
///
/// The two commands are written out rather than interpolated: this text is
/// attached with `after_help`, which needs a constant, and the reader may be in
/// any shell no matter which platform this build runs on.
const LOGIN_HELP: &str = r#"The account and the password are read from stdin, never from an argument: three
lines - account, password, remember. An empty account keeps the stored one, an
empty password keeps the password the service holds, and only n/no/0/false skips
storing (which removes a stored one). Without a pipe the command connects with
the pair the service already holds.

  Ask instead:  cmd /c thunet login -i      cmd.exe and PowerShell
                thunet login -i             bash, zsh, sh, fish, Linux, macOS
  Print one:    thunet login --cmd          also --pwsh
                thunet login --bash         also --zsh, --sh, --fish
  Scripts:      printf 'alice\npw\ny\n' | thunet login

This text in full: thunet login --verbose"#;

/// The three questions, word for word: `-i` asks them, and the tests compare the
/// wording against these rather than repeating it.
const ASK_ACCOUNT: &str = "THUnet account";
const ASK_PASSWORD: &str = "Password (Enter keeps the stored one)";
const ASK_REMEMBER: &str = "Remember the password for the next start? [Y/n]";

/// The line that makes `-i` work in the shell the reader is using, printed by
/// `thunet login --cmd` (alias `--pwsh`).
///
/// cmd.exe and PowerShell return to their prompt as soon as they have started
/// this windowed program, so their line editor would take the answers; `cmd /c`
/// runs the rest of the line as a command, and cmd waits for a command's child.
#[cfg(windows)]
const INTERACTIVE_CMD: &str = "cmd /c thunet login -i";
#[cfg(not(windows))]
const INTERACTIVE_CMD: &str = "thunet login -i";

/// The same line for the POSIX shells, printed by `--bash` and its aliases.
///
/// They wait for a foreground child on every platform - Git Bash on Windows
/// included - so this one needs no `cfg`.
const INTERACTIVE_POSIX: &str = "thunet login -i";

/// What `thunet login -v` prints: everything `--help` leaves out.
///
/// The same text on every platform, because it is about the protocol and about
/// the shells that do not wait rather than about the one the reader happens to
/// use. The two commands are written out for the same reason `LOGIN_HELP` does
/// it: this is a constant, and a reader may be in a shell of either kind.
const LOGIN_LONG: &str = r#"thunet login takes the account and the password on stdin, three lines, and asks
nothing unless you pass -i: on Windows this executable is a windowed program, so
it owns no console of its own and attaches to the one the shell is reading at the
same time - an Enter would be taken by the shell's own line editor and the
answers split between the two. stdin is the one channel both sides can share.

  line 1  account    empty keeps the account the service already holds
  line 2  password   empty keeps the password the service already holds
  line 3  remember   stored unless this line says n, no, 0 or false

With -i the same three answers are asked instead: the account (the stored one is
offered as the default, so Enter keeps it), the password (not echoed, and Enter
keeps the one the service holds) and remember ([Y/n], where Enter means yes). The
flag is your promise that the shell is waiting, and nothing here second-guesses
it. Which line keeps that promise depends on the shell, not on the platform:

  cmd /c thunet login -i     cmd.exe and PowerShell (Windows)
  thunet login -i            bash, zsh, sh, fish, Linux, macOS

cmd.exe and PowerShell return to their prompt as soon as they have started a
windowed program, and their line editor would take the answers; `cmd /c` runs the
rest of the line as a command, and cmd waits for a command's child. bash waits
for a foreground child everywhere, Git Bash on Windows included. A single-slash
switch reads as a path to Git Bash, so `cmd /c` typed there merely starts cmd.exe
by itself - use the plain form.

Skipping does not merely leave the disk alone: the answer n, no, 0 or false
deletes a password stored earlier, the same clear the window does when you untick
"Remember password". The default is "store" because saving is what most people
want, and while the machine is online the core proves a pair before it writes it
- a refused password falls back to the credential it already had. Offline there
is nothing to prove it against, so the pair is stored first and used when the
network returns.

Without a pipe and without -i (a terminal, a bare launch) nothing is read at all;
the command only asks the service to connect with the pair it already holds.

The password never reaches argv and never enters the shell history: it is only
ever read from that pipe or from the console, so no process listing shows it."#;

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
  /// Without `-i` nothing is asked: the account and the password come from stdin,
  /// three lines, or from the pair the service already holds. With `-i` the three
  /// questions are asked instead, reading whatever stdin is - the flag is the
  /// promise that a shell is waiting to answer them. `--cmd` (alias `--pwsh`)
  /// prints the line that makes that promise in cmd.exe and PowerShell, and
  /// `--bash` (aliases `--zsh`, `--sh`, `--fish`) the one every POSIX shell
  /// already honours.
  #[command(
    after_help = LOGIN_HELP,
    group(clap::ArgGroup::new("recipe")
      .multiple(false)
      .conflicts_with_all(["verbose", "interactive"]))
  )]
  Login {
    /// Print the long explanation and exit.
    #[arg(short, long)]
    verbose: bool,
    /// Ask for the account, the password and whether to remember, on this console.
    #[arg(short, long)]
    interactive: bool,
    /// Print the command line for cmd.exe and PowerShell, then exit.
    #[arg(long, group = "recipe")]
    cmd: bool,
    /// Print the command line for any POSIX shell, then exit.
    #[arg(long, group = "recipe")]
    bash: bool,
    /// Alias for `--cmd`.
    #[arg(long, group = "recipe", hide = true)]
    pwsh: bool,
    /// Alias for `--bash`.
    #[arg(long, group = "recipe", hide = true)]
    zsh: bool,
    /// Alias for `--bash`.
    #[arg(long, group = "recipe", hide = true)]
    sh: bool,
    /// Alias for `--bash`.
    #[arg(long, group = "recipe", hide = true)]
    fish: bool,
  },
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
    /// End every window on the machine, not just this session's.
    ///
    /// The per-session doorbell cannot cross a session, so windows in other
    /// sessions or other users' accounts would otherwise survive an upgrade
    /// that leaves their executable replaced. A machine-wide signal reaches
    /// them and needs no core, so it works even when the service is stopped;
    /// raising it needs administrator rights, which this command asks for
    /// through the same helper as the other privileged verbs. A portable setup
    /// treats the flag as a plain quit.
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

/// Point the process at the state directory the command line asks for.
///
/// The help text and `run` both read the directory from the environment, so the
/// two hidden globals are applied here once. `--machine` is the one way to say
/// "this process is not portable", whatever the environment says: an installer
/// inherits the environment of its caller, and a leftover `THUNET_STATE_DIR`
/// would silently retarget its commands at a portable endpoint that nothing is
/// listening on. `--state-dir` is what the Windows service is registered with,
/// because the token it runs under has no profile of its own.
fn point_at_state_dir(cli: &Cli) -> Result<(), String> {
  if cli.machine {
    if cli.state_dir.is_some() {
      return Err("`--machine` and `--state-dir` contradict each other".into());
    }
    // SAFETY: this runs on the main thread before anything reads the
    // environment (no other thread exists yet), and `config::dir` is only
    // reached afterwards.
    unsafe { std::env::remove_var("THUNET_STATE_DIR") };
  }
  if let Some(dir) = &cli.state_dir {
    // SAFETY: same reason - still the single-threaded startup path.
    unsafe { std::env::set_var("THUNET_STATE_DIR", dir) };
  }
  Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
  // `--machine` is the one way to say "this process is not portable", whatever
  // the environment says: an installer inherits the environment of its caller,
  // and a leftover `THUNET_STATE_DIR` would silently retarget its commands at a
  // portable endpoint that nothing is listening on. Clearing the variable here
  // is enough - `portable()`, `open_result` and the single-instance names all
  // derive from it, so every downstream decision follows.
  point_at_state_dir(&cli)?;
  // The elevated half of `quit --all-sessions`: raise the machine-wide signal
  // and stay up just long enough for every window to see it. Nothing else -
  // no config, no core, no window.
  if cli.signal_only {
    // The elevated half of `quit --all-sessions`: raise the machine-wide signal,
    // give every window time to answer, write the record an upgrade reads, and
    // exit. Nothing else - no config, no core, no window.
    finish_machine_quit(single::machine_quit()?);
    return Ok(());
  }
  // Effective settings: config file first, explicit CLI flags win.
  let mut cfg = config::Config::load();
  if let Some(h) = cli.host.clone() {
    cfg.host = h;
  }
  if cli.http {
    cfg.https = false;
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
    Cmd::Login {
      verbose,
      interactive,
      pwsh,
      cmd,
      bash,
      zsh,
      sh,
      fish,
    } => {
      // A printed command is output and nothing else: it works without a
      // service, a console or a terminal, and it never looks at the shell that
      // is running.
      if cmd || pwsh {
        say(INTERACTIVE_CMD);
        return Ok(());
      }
      if bash || zsh || sh || fish {
        say(INTERACTIVE_POSIX);
        return Ok(());
      }
      // Before anything touches the service: the long form is documentation, so
      // it has to work on a machine where no core is running at all.
      if verbose {
        say(LOGIN_LONG);
        return Ok(());
      }
      // `-i` asks on whatever stdin is: the shell that started this command is
      // the user's promise that it is waiting, and nothing here second-guesses
      // it. The questions come first, so cancelling - or a typo in the account -
      // leaves both the service and the sealed credential as they were.
      let asked = if interactive {
        let (account, password, remember) = ask_credentials(&cfg)?;
        Some(Piped {
          account,
          password,
          remember,
        })
      } else {
        None
      };
      let link = with_core()?;
      // Both facts come from the core itself. `status` would cost a portal
      // round trip, and asking the core to hold a pair just to learn which one
      // it holds would rewrite the stored credential.
      let info = core_snapshot(&link)?;
      let held = info.credential.username.trim().to_string();
      // The account this run would use: the answers, else line 1 of the pipe,
      // else the one the core holds, else the one the sealed blob carries (the
      // config loader reads `credential.bin` too, so this also covers a core
      // that started before the pair was stored).
      let answers = asked.or_else(piped_pair);
      let account = match answers.as_ref().map(|p| p.account.as_str()) {
        Some(account) if !account.is_empty() => account.to_string(),
        _ if !held.is_empty() => held.clone(),
        _ => cfg.username.trim().to_string(),
      };
      if account.is_empty() {
        return Err(format!(
          "no account: nothing was piped and the service holds no credential. Put the account, \
           the password and `y` or `no` on the first three lines of stdin and pipe them into \
           `thunet login`, or run `{INTERACTIVE_CMD}` to be asked; `thunet login --verbose` \
           shows the details"
        ));
      }
      // Never silent: where the account came from, what the service is doing,
      // and then what the answers do to the stored credential.
      match (&answers, interactive) {
        (Some(_), true) => say(&format!("account: {account} (asked)")),
        (Some(p), false) => say(&format!(
          "account: {account} (stdin, remember {})",
          if p.remember { "yes" } else { "no" }
        )),
        (None, _) => say(&format!("account: {account} (stored)")),
      }
      say(&session_line(info.state, &held));
      match &answers {
        Some(p) => {
          link.send(CoreCmd::SetCredentials {
            username: account.clone(),
            password: p.password.clone(),
            remember: p.remember,
            // An empty password line means "keep the one the core already
            // holds", which the core only does for the account it was proven
            // for - exactly the account line 1 named, or the held one when it
            // was left empty.
            keep_password: p.password.is_empty(),
          });
          // The answer says whether the pair was actually stored, which is the
          // difference the user asked about; staying silent would let a typo in
          // the account name pass as "saved".
          match wait_for(&link, Duration::from_secs(10), |ev| {
            matches!(ev, CoreEv::Credentials(_) | CoreEv::Error(Some(_)))
          })? {
            CoreEv::Credentials(state) => say(&credential_line(&state, p.remember)),
            CoreEv::Error(Some(e)) => return Err(e.to_string()),
            _ => return Err("the service did not confirm the credentials".into()),
          }
        }
        // A bare `login` uses the pair the core holds and leaves the remember
        // setting - and therefore the sealed copy - exactly as it is. When that
        // is already the case there is nothing to send at all.
        None if already_online_with(info.state, &account, &held) => {
          say("nothing to change");
          return Ok(());
        }
        None => {}
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
      // A service never receives command-line overrides, but a portable core
      // does: `thunet --host … core` has to talk to the host it was given, so
      // the settings merged at the top of `run()` are what it uses.
      let cfg = if machine {
        config::Config::load_machine()
      } else {
        cfg
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
      match autostart::Autostart::service(action, cli.minimized) {
        Ok(line) => {
          println!("{line}");
          Ok(())
        }
        // `pkexec` answers 126 both for a dismissed dialog and for a session
        // that could never show one - a terminal outside the logind session
        // (VS Code's integrated one, an SSH login) fails this way even with
        // the right password. Repeating the bare code leaves the user with no
        // way forward, so name the command the dialog would have run.
        Err(e) if e == crate::elevate::NO_AUTHORIZATION => Err(format!(
          "no administrator authorization was granted: the polkit dialog was dismissed, or this \
           session cannot show one. Run it as root instead:\n  sudo {} --machine service {}",
          crate::exe_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "thunet".into()),
          action.name()
        )),
        Err(e) => Err(e),
      }
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
    // Same rule as `service-admin`: the installer passes `--machine`, and a
    // portable caller must not delete the window's own state directory with a
    // verb that claims to be about the machine.
    Cmd::ServicePurge => {
      if ipc::portable() {
        return Err(
          "portable mode has no machine state to purge; \
           `thunet --machine service-purge` removes the machine directory explicitly"
            .into(),
        );
      }
      autostart::purge_machine_state()
    }
    Cmd::Quit {
      timeout,
      all_sessions,
    } => {
      let budget = Duration::from_millis(timeout);
      if all_sessions && !ipc::portable() {
        // The machine-wide signal is the whole mechanism: it needs no core, and
        // an uninstall often runs with the service already stopped. Raising it
        // needs administrator rights, so an attempt that fails without them is
        // retried through the same elevation helper every other privileged
        // verb uses; the elevated copy raises the signal, holds it for a
        // moment, and exits. This session's window is closed by the doorbell
        // afterwards, whichever half ran the signal.
        match single::machine_quit() {
          Ok(signal) => {
            let waited = single::quit_and_wait(budget);
            finish_machine_quit(signal);
            return waited;
          }
          Err(e) if crate::elevate::is_elevated() => return Err(e),
          Err(_) => {
            crate::elevate::run_as_admin(&["--machine", "--signal-only"])?;
            return single::quit_and_wait(budget);
          }
        }
      }
      // Portable: one window per state directory, so "all sessions" is this
      // setup and nothing machine-wide (or administrator-only) is involved.
      single::quit_and_wait(budget)
    }
  }
}

/// Keep the machine-wide signal up long enough for every window to see it and
/// answer, then write the record an upgrade uses to put those windows back.
///
/// Only an elevated process may write the record: it names other users, and an
/// installer is the only thing that reads it.
fn finish_machine_quit(signal: single::MachineQuit) {
  std::thread::sleep(Duration::from_millis(1000));
  if crate::elevate::is_elevated() {
    let acks = signal.acks();
    if let Err(e) = single::write_quit_record(&acks) {
      eprintln!("{e}");
    }
  }
  drop(signal);
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

/// The core's own state, for a command that must not cost a portal request.
///
/// `with_core` already consumed the replay marker it asks for, so this is one
/// extra local round trip and no network traffic.
fn core_snapshot(link: &ipc::Link) -> Result<proto::CoreInfo, String> {
  link.send(CoreCmd::CoreInfo);
  match wait_for(link, Duration::from_secs(10), |ev| {
    matches!(ev, CoreEv::Core(_) | CoreEv::Error(Some(_)))
  })? {
    CoreEv::Core(info) => Ok(info),
    CoreEv::Error(Some(e)) => Err(e.to_string()),
    _ => Err("the service did not report its own state".into()),
  }
}

/// The three lines `thunet login` reads, once they were actually piped.
struct Piped {
  account: String,
  password: String,
  remember: bool,
}

/// Read account, password and remember from stdin, or `None` for "nothing was
/// piped".
///
/// A terminal is not a pipe: nothing was written there for this command, and
/// asking is `-i`'s job. A read that fails (a launch with no stdin handle at
/// all) is the same case.
fn piped_pair() -> Option<Piped> {
  // Nobody typed an answer into a terminal.
  if prompt::available() {
    return None;
  }
  // The first line decides whether there is an answer at all: no line at all is
  // an empty pipe (or a closed handle), while an empty *line* is an answer -
  // "keep the stored account". A missing second and third line keep the
  // password the core holds and store it, which is what pressing Enter would do.
  let account = prompt::piped_line()?.trim().to_string();
  // Only the terminator was stripped: a password may end in spaces.
  let password = prompt::piped_line().unwrap_or_default();
  let remember = wants_remember(&prompt::piped_line().unwrap_or_default());
  Some(Piped {
    account,
    password,
    remember,
  })
}

/// Whether the third line of the pipe asks for the password to be stored.
///
/// The default is "store": the password is kept for the next start unless the
/// answer is explicitly negative, so `n`, `no`, `0` and `false` mean "this
/// session only" and everything else - `y`, `yes`, an empty line, a missing
/// line, a typo - means "save it". The user's rule is "save by default", and a
/// wrong password does not end up on disk in the case that matters: while online
/// the core proves the pair before it writes and falls back to the credential it
/// already had when the portal refuses it.
fn wants_remember(answer: &str) -> bool {
  !matches!(
    answer.trim().to_ascii_lowercase().as_str(),
    "n" | "no" | "0" | "false"
  )
}

/// Ask the account, the password and the remember question.
///
/// Reads whatever stdin is - the console when `-i` was promised one. The stored
/// account is offered as the default so that Enter keeps it, an empty password
/// answer means "keep the one the service already holds", and Enter at the last
/// question stores the password (only `n`/`no`/`0`/`false` does not).
fn ask_credentials(cfg: &config::Config) -> Result<(String, String, bool), String> {
  prompt::arm_cancel();
  let default = cfg.username.trim();
  let question = if default.is_empty() {
    format!("{ASK_ACCOUNT}: ")
  } else {
    format!("{ASK_ACCOUNT} [{default}]: ")
  };
  let username = match prompt::line(&question)? {
    answer if answer.is_empty() => default.to_string(),
    answer => answer,
  };
  if username.is_empty() {
    return Err("no account given".into());
  }
  let password = prompt::secret(&format!("{ASK_PASSWORD}: "))?;
  let remember = wants_remember(&prompt::line(&format!("{ASK_REMEMBER} "))?);
  Ok((username, password, remember))
}

/// Print one line to stdout, ignoring a closed stdout.
///
/// `println!` panics when the reader is already gone, and PowerShell does close
/// the pipe as soon as it has started a windowed program: missing output must
/// not leave a crash record behind.
fn say(line: &str) {
  let _ = writeln!(std::io::stdout(), "{line}");
}

/// Whether a bare `login` has nothing left to do: the service is already online
/// with the very account it would use, so neither a `SetCredentials` nor a
/// `Connect` is sent and the stored pair stays as it is.
fn already_online_with(state: session::State, account: &str, held: &str) -> bool {
  state == session::State::Online && account == held
}

/// One line about the service: whether a session is up, and the account the
/// core holds when it has one.
fn session_line(state: session::State, held: &str) -> String {
  let verb = match state {
    session::State::Online => "online",
    session::State::Authenticating => "authenticating",
    session::State::Offline => "offline",
  };
  match (held.is_empty(), state) {
    (false, session::State::Offline) => format!("session: {verb} (credential: {held})"),
    (false, _) => format!("session: {verb} as {held}"),
    (true, _) => format!("session: {verb}"),
  }
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
      format!("credentials: {account}, NOT saved (an account and a password are both needed)")
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

  /// The three-line stdin protocol: the password is stored unless the third
  /// line says otherwise, and only an explicit `n`/`no`/`0`/`false` does.
  #[test]
  fn the_third_line_skips_storing_only_when_it_says_so() {
    for answer in [
      "", " ", "y", "Y", "yes", "YES", "maybe", "yess", "true", "1",
    ] {
      assert!(
        wants_remember(answer),
        "{answer:?} should store the password"
      );
    }
    for answer in ["n", "N", "no", " NO ", "0", "false", "FALSE"] {
      assert!(
        !wants_remember(answer),
        "{answer:?} should keep the password for this session only"
      );
    }
  }

  /// `--help` is the short form and `-v` the long one, so every fact the reader
  /// needs to pick a command has to be in one of them.
  #[test]
  fn the_short_help_names_both_commands_and_their_flags() {
    for needle in [
      INTERACTIVE_CMD,
      INTERACTIVE_POSIX,
      "--cmd",
      "--pwsh",
      "--bash",
      "n/no/0/false",
      "printf 'alice\\npw\\ny\\n' | thunet login",
      "thunet login --verbose",
    ] {
      assert!(
        LOGIN_HELP.contains(needle),
        "missing {needle:?} in {LOGIN_HELP}"
      );
    }
  }

  /// The long form is the one that keeps the rules and the reasoning: the third
  /// line, the promise `-i` makes, and why a Windows shell needs `cmd /c`.
  #[test]
  fn the_long_help_carries_the_rules_and_the_reasoning() {
    for needle in [
      "cmd.exe and PowerShell",
      "cmd /c thunet login -i",
      "Git Bash",
      "n, no, 0 or false",
      "deletes a password stored earlier",
      "proves a pair before it writes it",
      "argv",
      "nothing here",
    ] {
      assert!(LOGIN_LONG.contains(needle), "missing {needle:?}");
    }
  }

  /// The two printed commands differ exactly where the shells do: cmd.exe and
  /// PowerShell need `cmd /c`, every POSIX shell (Git Bash included) does not.
  #[test]
  fn the_printed_commands_match_the_shells_that_wait() {
    assert_eq!(INTERACTIVE_POSIX, "thunet login -i");
    #[cfg(windows)]
    assert_eq!(INTERACTIVE_CMD, "cmd /c thunet login -i");
    #[cfg(not(windows))]
    assert_eq!(INTERACTIVE_CMD, INTERACTIVE_POSIX);
  }

  /// A printed command is output only, so exactly one of the flags can be meant.
  #[test]
  fn the_printed_command_flags_are_exclusive() {
    for flag in ["--pwsh", "--cmd", "--bash", "--zsh", "--sh", "--fish"] {
      assert!(
        Cli::try_parse_from(["thunet", "login", flag]).is_ok(),
        "{flag} should be accepted"
      );
    }
    for both in [
      ["--pwsh", "--bash"],
      ["--cmd", "--fish"],
      ["--bash", "-i"],
      ["--bash", "-v"],
    ] {
      assert!(
        Cli::try_parse_from(["thunet", "login", both[0], both[1]]).is_err(),
        "{both:?} should be refused"
      );
    }
  }

  #[test]
  fn a_bare_login_leaves_an_online_session_alone() {
    assert!(already_online_with(
      session::State::Online,
      "alice",
      "alice"
    ));
    assert!(!already_online_with(session::State::Online, "bob", "alice"));
    assert!(!already_online_with(
      session::State::Offline,
      "alice",
      "alice"
    ));
    assert!(!already_online_with(
      session::State::Authenticating,
      "alice",
      "alice"
    ));
  }

  #[test]
  fn the_session_line_names_the_account_the_core_holds() {
    assert_eq!(
      session_line(session::State::Online, "bob"),
      "session: online as bob"
    );
    assert_eq!(
      session_line(session::State::Offline, "alice"),
      "session: offline (credential: alice)"
    );
    assert_eq!(
      session_line(session::State::Offline, ""),
      "session: offline"
    );
    assert_eq!(
      session_line(session::State::Authenticating, "alice"),
      "session: authenticating as alice"
    );
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
