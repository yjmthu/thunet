//! The desktop entry point: CLI parsing, the service verbs, and starting the
//! window. Android has its own entry point.

use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser, Subcommand};

use crate::proto::{Cmd as CoreCmd, Ev as CoreEv, ToastKind};
#[cfg(windows)]
use crate::winsvc;
use crate::{autostart, config, diag, ipc, prompt, proto, session, shortcut, single, ui};

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
  /// Start on screen even when the saved preference says to start in the tray.
  ///
  /// Internal: an upgrade passes it when the window it is putting back was
  /// open. It wins over both `--minimized` and `start_minimized`.
  #[arg(long, global = true, hide = true)]
  visible: bool,
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

-i asks one more question the pipe cannot carry: whether the service connects at
startup (default yes). Every report also says whether the app starts by itself
at login, and names the way to turn that on when it does not.

  Ask instead:  cmd /c thunet login -i      cmd.exe and PowerShell
                thunet login -i             bash, zsh, sh, fish, Linux, macOS
  Print one:    thunet login --cmd          also --pwsh
                thunet login --bash         also --zsh, --sh, --fish
  Scripts:      printf 'alice\npw\ny\n' | thunet login

This text in full: thunet login --verbose"#;

/// The questions, word for word: `-i` asks them, and the tests compare the
/// wording against these rather than repeating it.
const ASK_ACCOUNT: &str = "THUnet account";
const ASK_PASSWORD: &str = "Password (Enter keeps the stored one)";
const ASK_REMEMBER: &str = "Remember the password for the next start? [Y/n]";
const ASK_AUTO_CONNECT: &str = "Connect automatically at startup? [Y/n]";

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

With -i those answers are asked instead: the account (the stored one is offered
as the default, so Enter keeps it), the password (not echoed, and Enter keeps the
one the service holds), remember ([Y/n], where Enter means yes) and whether the
service connects at startup ([Y/n], also yes by default - it says what the next
start does, not whether this run connects; the pipe never carries this one, so a
script cannot change a setting it never sees). The flag is your promise that the
shell is waiting, and nothing here second-guesses it. Which line keeps that
promise depends on the shell, not on the platform:

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

Every run also reports whether the app starts by itself at login: the entry that
does it (`thunet.service`, the Windows service, or the window's own login entry in
portable mode) and, when it is disabled, the command or setting that turns it on.

The password never reaches argv and never enters the shell history: it is only
ever read from that pipe or from the console, so no process listing shows it."#;

/// A boolean a person types as `on` or `off`.
///
/// Used by the core-owned switches: `--auto-connect on` reads like the
/// settings page, and `off` cannot be confused with a missing flag the way a
/// bare `--flag`/`--no-flag` pair can.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Toggle {
  On,
  Off,
}

impl Toggle {
  fn value(self) -> bool {
    matches!(self, Self::On)
  }
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
  /// Without `-i` nothing is asked: the account and the password come from stdin,
  /// three lines, or from the pair the service already holds. With `-i` the
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
    /// Ask for the account, the password, whether to remember, and whether to
    /// connect at startup, on this console.
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
  /// A portable setup (`THUNET_STATE_DIR`) has no machine service: there
  /// `status`, `enable` and `disable` address the window's own login entry,
  /// `start`/`stop`/`restart` run and end a core of that state directory, and
  /// only `install`/`uninstall` are refused (`--machine` is the explicit way to
  /// the machine service).
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
  /// Delete the machine state directory after a real uninstall (installer
  /// internal; never called for an upgrade or repair).
  #[command(hide = true)]
  ServicePurge,
  /// Put the package back the way the install found it: start the core if it
  /// was running, then restore the windows the quit step closed (installer
  /// internal).
  #[command(hide = true)]
  ServiceRestore,
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
    /// End every window on the machine, not just this session's, and stop the
    /// core - the whole machine-wide half of an install.
    ///
    /// The per-session doorbell cannot cross a session, so windows in other
    /// sessions or other users' accounts would otherwise survive an upgrade
    /// that leaves their executable replaced. A machine-wide signal reaches
    /// them and needs no core, so it works even when the service is stopped;
    /// raising it needs administrator rights, which this command asks for
    /// through the same helper as the other privileged verbs. The core is
    /// stopped afterwards, and whether it was running is recorded so
    /// `service-restore` can bring it back in the same state. A portable setup
    /// treats the flag as a plain quit.
    #[arg(long)]
    all_sessions: bool,
  },
  /// Print the core's effective settings.
  ///
  /// Read from the running core, never from the window's file: the account,
  /// the portal host and the session policy belong to the core, which is also
  /// the only process that may write them down. The options change the same
  /// three switches the settings page has; they need a core as well.
  Config {
    /// Start online right after the core starts.
    #[arg(long, value_name = "on|off")]
    auto_connect: Option<Toggle>,
    /// Sign back in after the session is lost, until the user disconnects.
    #[arg(long, value_name = "on|off")]
    keep_online: Option<Toggle>,
    /// Keep the password for the next start; `off` deletes the stored copy.
    #[arg(
      long = "remember-password",
      visible_alias = "remember-pwd",
      value_name = "on|off"
    )]
    remember_password: Option<Toggle>,
  },
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

pub fn main() {
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
    // give every window time to answer, write the record an upgrade reads, stop
    // the core, and exit. Nothing else - no config, no window.
    //
    // This half has no console and usually runs as the installer's helper, which
    // looks at the exit code alone: without a log file a failed answer ("zero
    // windows answered") is what an install that silently restores nothing looks
    // like. The machine directory is the log the service already writes.
    #[cfg(windows)]
    diag::set_file(config::machine_dir().join("core.log"));
    finish_machine_quit(single::machine_quit()?)?;
    return Ok(());
  }
  // The window's settings. The core's own file is read where it is needed: by
  // the service itself, and through the IPC by every command that acts on it.
  if ipc::portable() {
    // A portable setup kept both owners in one file before the split; move the
    // core's half out before the window can save over it.
    config::migrate();
  }
  let cfg = config::Config::load();

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
    // user asked for that in the settings page. `--visible` overrides both: an
    // upgrade restoring a window that was open must not honour a preference
    // that would hide it.
    let minimized = !cli.visible && (cli.minimized || cfg.start_minimized);
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
    Cmd::Config {
      auto_connect,
      keep_online,
      remember_password,
    } => {
      if auto_connect.is_none() && keep_online.is_none() && remember_password.is_none() {
        // The core's own view: the window's file is not consulted at all. The
        // path is printed so the reader knows where the parts `CoreInfo` does
        // not carry (`https`, `ip`, the reconnect delay) live on disk.
        let dir = if ipc::portable() {
          config::dir()
        } else {
          config::machine_dir()
        };
        println!("path: {}", config::core_path_in(&dir).display());
        let link = with_core()?;
        let info = core_snapshot(&link)?;
        let settings = serde_json::json!({
          "host": info.host,
          "ac_id": info.ac_id,
          "username": info.credential.username,
          "has_password": info.credential.has_password,
          "remember_password": info.credential.remembered,
          "auto_connect": info.auto_connect,
          "keep_online": info.keep_online,
        });
        println!("{}", serde_json::to_string_pretty(&settings).unwrap());
        return Ok(());
      }
      // The three switches belong to the core: asking it is both the only way
      // to make the change and the only way to report what is actually in
      // force. A machine core keeps its own `core.json` in the machine
      // directory, which an ordinary caller cannot write.
      let link = with_core()?;
      let before = core_snapshot(&link)?;
      if remember_password.is_some() && matches!(before.env, proto::LoginEnv::EnterpriseWifi(_)) {
        // The OS owns that session; there is no portal pair to seal, and the
        // core would answer with the state it already has, which reads like a
        // success.
        return Err(
          "this network signs in with 802.1X, so there is no portal account to remember".into(),
        );
      }
      if let Some(toggle) = auto_connect {
        link.send(CoreCmd::SetAutoConnect(toggle.value()));
      }
      if let Some(toggle) = keep_online {
        link.send(CoreCmd::SetKeepOnline(toggle.value()));
      }
      if let Some(toggle) = remember_password {
        let on = toggle.value();
        if !on && !before.credential.has_password {
          // Nothing is sealed and nothing is held, so there is no copy to
          // delete. Sending `remember = false` anyway would make the core
          // rewrite its config, and with no password to keep the account name
          // has no business surviving that write either.
          say("credentials: nothing is stored, so there is nothing to forget");
        } else {
          // An empty password with `keep_password` asks the core to seal - or,
          // with `off`, to delete - the copy it already holds: the plaintext
          // never leaves the core, and the account has to be the one it holds.
          link.send(CoreCmd::SetCredentials {
            username: before.credential.username.clone(),
            password: String::new(),
            remember: on,
            keep_password: true,
          });
          match wait_for(&link, Duration::from_secs(10), |ev| {
            matches!(ev, CoreEv::Credentials(_) | CoreEv::Error(Some(_)))
          })? {
            CoreEv::Credentials(state) => say(&credential_line(&state, on)),
            CoreEv::Error(Some(e)) => return Err(e.to_string()),
            _ => return Err("the service did not confirm the credentials".into()),
          }
        }
      }
      // Ask again rather than repeating what was requested: the answer is what
      // the core holds now, and a failed write (it logs instead of returning an
      // error) has been printed by then.
      if auto_connect.is_some() || keep_online.is_some() {
        let after = core_snapshot(&link)?;
        if auto_connect.is_some() {
          say(&format!("auto connect: {}", onoff(after.auto_connect)));
        }
        if keep_online.is_some() {
          say(&format!("keep online: {}", onoff(after.keep_online)));
        }
      }
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
      // The core holds the account, so the prompt's default and the run's
      // fallback come from it. Connecting here changes nothing, and cancelling
      // still leaves the service and the sealed credential exactly as they were.
      let link = with_core()?;
      // Both facts come from the core itself. `status` would cost a portal
      // round trip, and asking the core to hold a pair just to learn which one
      // it holds would rewrite the stored credential.
      let info = core_snapshot(&link)?;
      let held = info.credential.username.trim().to_string();
      // `-i` asks on whatever stdin is: the shell that started this command is
      // the user's promise that it is waiting, and nothing here second-guesses
      // it.
      let asked = if interactive {
        let (account, password, remember, auto_connect) = ask_credentials(&held)?;
        Some(Piped {
          account,
          password,
          remember,
          auto_connect: Some(auto_connect),
        })
      } else {
        None
      };
      // The account this run would use: the answers, else line 1 of the pipe,
      // else the one the core holds.
      let answers = asked.or_else(piped_pair);
      let account = match answers.as_ref().map(|p| p.account.as_str()) {
        Some(account) if !account.is_empty() => account.to_string(),
        _ if !held.is_empty() => held.clone(),
        _ => String::new(),
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
      // The start-up preference is asked only with `-i`, and it is persisted by
      // the core: it says what the *next* start does, not whether this run
      // connects (that is the `Connect` below).
      if let Some(on) = answers.as_ref().and_then(|p| p.auto_connect) {
        link.send(CoreCmd::SetAutoConnect(on));
        say(&format!("auto connect: {}", if on { "on" } else { "off" }));
      }
      say(&autostart_line(cfg.start_minimized));
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
      // does: `thunet --host … core` has to talk to the host it was given.
      let mut core = if machine {
        config::CoreConfig::load_machine()
      } else {
        config::CoreConfig::load()
      };
      if let Some(h) = cli.host.clone() {
        core.host = h;
      }
      if cli.http {
        core.https = false;
      }
      if !cli.ac_id.is_empty() {
        core.ac_id = cli.ac_id.clone();
      }
      if !cli.ip.is_empty() {
        core.ip = cli.ip.clone();
      }
      match ipc::serve(core, machine)? {
        ipc::Serve::Started => Ok(()),
        ipc::Serve::AlreadyRunning => {
          // Nothing to do: whoever owns the endpoint keeps serving the frontends.
          eprintln!("another core is already running");
          Ok(())
        }
      }
    }
    Cmd::Service { action } => {
      // Portable mode has no machine service: the core is a thread of the window
      // (or, after `service start`, a process of its own). Only `install` and
      // `uninstall` address the machine service, and running them here would
      // register this very executable as the machine core while the user is
      // looking at their own portable setup. The rest have a portable meaning
      // and are handled in `portable_service`. `service-admin` is deliberately
      // not gated (see its arm below).
      if ipc::portable() {
        if autostart::service_refused_in_portable(action) {
          return Err(
            "portable mode has no machine service to install: the core runs inside the window, \
             so it lives exactly as long as the window does. `service start` runs a core of this \
             state directory without a window, `service stop` ends it, and `service \
             enable|disable` control this window's login entry. To act on the machine service, \
             run `thunet --machine service …`"
              .into(),
          );
        }
        return portable_service(action, cli.minimized || cfg.start_minimized);
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
    // One command for the whole upgrade tail, because the two halves belong to
    // one transaction: the core first (the windows connect to it), then each
    // window in the state it answered. Both run even when the core fails to
    // start - a slow service must not also swallow the windows - and the first
    // error is what the installer sees.
    Cmd::ServiceRestore => {
      let core = autostart::Autostart::resume_after_update();
      let windows = single::restore_windows();
      core.and(windows)
    }
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
        // The machine-wide signal is the whole mechanism: it needs no core to
        // relay it, so it reaches windows whose core is already gone. Both halves
        // need administrator rights, and the second one cannot be guessed: an
        // immediate custom action runs with the installer's unelevated token, and
        // binding the signal can succeed there while writing the record and
        // stopping the service do not. So the token alone decides - a process
        // that is not elevated delegates the whole job to the same helper every
        // other privileged verb uses, and the elevated copy raises the signal,
        // holds it for a moment, writes the record and stops the core. This
        // session's window is closed by the doorbell afterwards.
        if !crate::elevate::is_elevated() {
          crate::elevate::run_as_admin(&["--machine", "--signal-only"])?;
          return single::quit_and_wait(budget);
        }
        let signal = single::machine_quit()?;
        let waited = single::quit_and_wait(budget);
        return waited.and(finish_machine_quit(signal));
      }
      // Portable: one window per state directory, so "all sessions" is this
      // setup and nothing machine-wide (or administrator-only) is involved.
      single::quit_and_wait(budget)
    }
  }
}

/// Keep the machine-wide signal up long enough for every window to see it and
/// answer, write the record an upgrade uses to put those windows back, then stop
/// the core.
///
/// The order is the whole point: the answers name windows that are about to
/// disappear, and whether the core is running is only knowable before it stops.
/// Both are written before `stop_for_update`.
///
/// Only an elevated process may write the record or stop the service: the record
/// names other users, and a service belongs to the machine. The stop's failure
/// is returned, because an installer has to abort rather than replace an image
/// that is still mapped.
fn finish_machine_quit(signal: single::MachineQuit) -> Result<(), String> {
  std::thread::sleep(Duration::from_millis(1000));
  let acks = signal.acks();
  crate::diag::line(&format!("quit: {} window(s) answered", acks.len()));
  let mut result = Ok(());
  if crate::elevate::is_elevated() {
    if let Err(e) = single::write_quit_record(&acks) {
      eprintln!("{e}");
    }
    result = autostart::stop_for_update();
  } else {
    // The next install would restore nothing, and the only other symptom is a
    // silent upgrade: say why, in the log an installer leaves behind.
    crate::diag::line("quit: not elevated, so the record and the core are left alone");
  }
  drop(signal);
  result
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
    // mode: portable mode has no machine service, and `thunet service enable`
    // is refused there, so pointing at it would send the user to a dead end. It
    // also has to name the state directory: a bare `thunet` is machine mode,
    // so "start the window" only helps if it is the same portable setup.
    let hint = if ipc::portable() {
      let dir = crate::config::dir();
      format!(
        "portable mode: start the window with `thunet --state-dir {}`, or a core without one \
         with `thunet --state-dir {} service start`",
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

/// The one line that explains a link which died without answering.
///
/// Both facts are read off the [`ipc::Link`], which is the only place left that
/// knows them once the connection is gone: an old core announced its protocol,
/// and a core that refuses this frontend left one warning before closing.
/// Naming that warning is the difference between "closed the connection" and a
/// reason the user can act on.
fn closed_reason(speaks: Option<u8>, warn: Option<String>) -> String {
  match speaks {
    // The core refused every line this build sent, so nothing will work
    // until the service is restarted on a matching build.
    Some(v) => format!(
      "the background service speaks protocol {v} but this build speaks {}; restart the service",
      proto::VERSION
    ),
    None => match warn {
      Some(warn) => format!("the background service closed the connection: {warn}"),
      None => "the background service closed the connection".into(),
    },
  }
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
      return Err(closed_reason(link.speaks(), link.last_warn()));
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

/// The lines `thunet login` reads, once they were actually piped.
struct Piped {
  account: String,
  password: String,
  remember: bool,
  /// Only the interactive path answers this: the pipe has three lines, and a
  /// script must not silently rewrite a preference it never saw.
  auto_connect: Option<bool>,
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
  let remember = unless_negative(&prompt::piped_line().unwrap_or_default());
  Some(Piped {
    account,
    password,
    remember,
    auto_connect: None,
  })
}

/// Whether an answer means "yes" when the default is yes.
///
/// Only an explicit `n`, `no`, `0` or `false` means no; an empty line, a
/// missing line or a typo keeps the default. Both questions that default to
/// yes - remember the password and connect at startup - read answers this way.
///
/// For the password the default is "store": it is kept for the next start
/// unless the answer is explicitly negative, so `n`, `no`, `0` and `false`
/// mean "this session only" and everything else means "save it". A wrong
/// password does not end up on disk in the case that matters: while online the
/// core proves the pair before it writes and falls back to the credential it
/// already had when the portal refuses it.
fn unless_negative(answer: &str) -> bool {
  !matches!(
    answer.trim().to_ascii_lowercase().as_str(),
    "n" | "no" | "0" | "false"
  )
}

/// Ask the account, the password, the remember question and the start-up one.
///
/// Reads whatever stdin is - the console when `-i` was promised one. The stored
/// account is offered as the default so that Enter keeps it, an empty password
/// answer means "keep the one the service already holds", and Enter at either
/// yes/no question takes its default, which is yes for both.
fn ask_credentials(default: &str) -> Result<(String, String, bool, bool), String> {
  prompt::arm_cancel();
  let default = default.trim();
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
  let remember = unless_negative(&prompt::line(&format!("{ASK_REMEMBER} "))?);
  let auto_connect = unless_negative(&prompt::line(&format!("{ASK_AUTO_CONNECT} "))?);
  Ok((username, password, remember, auto_connect))
}

/// Print one line to stdout, ignoring a closed stdout.
///
/// `println!` panics when the reader is already gone, and PowerShell does close
/// the pipe as soon as it has started a windowed program: missing output must
/// not leave a crash record behind.
fn say(line: &str) {
  let _ = writeln!(std::io::stdout(), "{line}");
}

/// `on`/`off`, the words the config switches are typed with.
fn onoff(on: bool) -> &'static str {
  if on { "on" } else { "off" }
}

/// Whether a bare `login` has nothing left to do: the service is already online
/// with the very account it would use, so neither a `SetCredentials` nor a
/// `Connect` is sent and the stored pair stays as it is.
fn already_online_with(state: session::State, account: &str, held: &str) -> bool {
  state == session::State::LoggedIn && account == held
}

/// One line about the service: whether a session is up, and the account the
/// core holds when it has one.
fn session_line(state: session::State, held: &str) -> String {
  let verb = match state {
    session::State::LoggedIn => "online",
    session::State::Authenticating => "authenticating",
    session::State::LoggedOut => "offline",
  };
  match (held.is_empty(), state) {
    (false, session::State::LoggedOut) => format!("session: {verb} (credential: {held})"),
    (false, _) => format!("session: {verb} as {held}"),
    (true, _) => format!("session: {verb}"),
  }
}

/// The line `thunet login` prints about starting by itself.
///
/// Machine mode names the service; a portable window names its own login entry,
/// because it has no service to point at. Both are read from the OS on every
/// run, so deleting an entry by hand is reflected at once.
fn autostart_line(minimized: bool) -> String {
  let entry = if ipc::portable() {
    autostart::Autostart::portable(minimized, config::dir())
  } else {
    autostart::Autostart::new(minimized)
  };
  match entry.map(|entry| entry.login_status()) {
    Some(status) => autostart_words(&status),
    // Both constructors need the executable path; without it there is nothing
    // to check, and an "unknown" line beats one that looks like "off".
    None => "autostart: unknown (cannot locate the executable)".into(),
  }
}

/// `autostart: enabled (…)`, or a disabled answer plus the way to turn it on.
fn autostart_words(status: &autostart::LoginStatus) -> String {
  if status.enabled {
    format!("autostart: enabled ({})", status.entry)
  } else {
    format!("autostart: disabled ({})\n  {}", status.entry, status.hint)
  }
}

/// `thunet service <enable|disable|status>` in portable mode.
///
/// There is no machine service here; the only thing that can start this core is
/// the window itself, so these three verbs address the window's login entry -
/// the CLI half of the settings checkbox. `--machine service …` stays the way
/// to the machine service, and the caller has already refused every other verb.
fn portable_service(action: autostart::ServiceAction, minimized: bool) -> Result<(), String> {
  match action {
    autostart::ServiceAction::Status => {
      say(&autostart_words(&portable_entry(minimized)?.login_status()));
    }
    autostart::ServiceAction::Enable => {
      let entry = portable_entry(minimized)?;
      entry.set_tray(true)?;
      say(&format!(
        "window autostart enabled ({})",
        entry.login_status().entry
      ));
    }
    autostart::ServiceAction::Disable => {
      let entry = portable_entry(minimized)?;
      entry.set_tray(false)?;
      say(&format!(
        "window autostart disabled ({})",
        entry.login_status().entry
      ));
    }
    autostart::ServiceAction::Start => portable_start()?,
    autostart::ServiceAction::Stop => portable_stop()?,
    autostart::ServiceAction::Restart => {
      portable_stop()?;
      portable_start()?;
    }
    // `service_refused_in_portable` filters these out before the call.
    other => {
      return Err(format!(
        "`service {}` means nothing to a portable window",
        other.name()
      ));
    }
  }
  Ok(())
}

/// The login entry of this portable window, for the autostart verbs.
fn portable_entry(minimized: bool) -> Result<autostart::Autostart, String> {
  autostart::Autostart::portable(minimized, config::dir())
    .ok_or_else(|| "cannot determine the executable path".to_string())
}

/// Start the core of this portable setup as a process of its own.
///
/// A CLI process cannot host the thread a window hosts, so this runs the
/// documented headless core (`thunet core --state-dir …`) instead. An open
/// window attaches to it like any other frontend. Its output goes next to the
/// state directory: there is no console anybody would see it on.
fn portable_start() -> Result<(), String> {
  if ipc::open().is_some() {
    say("the core is already running");
    return Ok(());
  }
  let log = spawn_portable_core()?;
  // The endpoint is bound before anything else happens in `serve`, so a short
  // poll is enough to tell "started" from "died on the way".
  let deadline = Instant::now() + Duration::from_secs(5);
  while Instant::now() < deadline {
    if ipc::open().is_some() {
      say(&format!(
        "core started as a separate process (log: {})",
        log.display()
      ));
      return Ok(());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  Err(format!(
    "the core did not answer within 5 s; see {}",
    log.display()
  ))
}

/// Stop the core of this portable setup, wherever it lives.
///
/// `Cmd::Shutdown` ends a thread hosted by the window or a process of its own;
/// the window, if it is open, stays open in both cases.
fn portable_stop() -> Result<(), String> {
  let Some(link) = ipc::open() else {
    say("the core is not running");
    return Ok(());
  };
  link.send(CoreCmd::Shutdown);
  // A hosted core cannot report this over the socket: both halves of the
  // connection live in the window's process, so no EOF is sent and `alive()`
  // would stay true. What does change is the endpoint - `serve` drops the
  // listener when the host asked it to stop - so wait for that instead. This
  // also covers the separate-process case, which exits outright.
  drop(link);
  let deadline = Instant::now() + Duration::from_secs(10);
  while ipc::open().is_some() {
    if Instant::now() >= deadline {
      return Err("the core did not stop within 10 s".into());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  say("core stopped");
  Ok(())
}

/// Run `thunet core --state-dir …` detached, with its log in the state
/// directory.
fn spawn_portable_core() -> Result<std::path::PathBuf, String> {
  let exe = crate::exe_path().ok_or("cannot determine the executable path")?;
  let dir = config::dir();
  std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
  let log_path = dir.join("core.log");
  let log = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(&log_path)
    .map_err(|e| format!("{}: {e}", log_path.display()))?;
  let mut cmd = std::process::Command::new(&exe);
  cmd
    .arg("core")
    .arg("--state-dir")
    .arg(&dir)
    .stdin(std::process::Stdio::null())
    .stdout(log.try_clone().map_err(|e| e.to_string())?)
    .stderr(log);
  // Outlive the shell that asked for it: a terminal hangup would otherwise end
  // the core together with the CLI.
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
  }
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
  }
  cmd
    .spawn()
    .map_err(|e| format!("cannot start the core: {e}"))?;
  Ok(log_path)
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
      CoreEv::State(session::State::LoggedIn) | CoreEv::Notify { .. } | CoreEv::Error(Some(_))
    )
  })? {
    CoreEv::State(session::State::LoggedIn) => {
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

  /// A link that dies under a command has to say what the core said: the phrase
  /// "closed the connection" alone is what made a refused frontend look like a
  /// crash, while the reason existed only in the service's own log file.
  #[test]
  fn a_dead_link_names_its_reason() {
    assert_eq!(
      closed_reason(None, None),
      "the background service closed the connection"
    );
    assert_eq!(
      closed_reason(None, Some("core rejected frontend: pid 1".into())),
      "the background service closed the connection: core rejected frontend: pid 1"
    );
    // A protocol mismatch is the more precise answer and must win.
    assert!(
      closed_reason(Some(7), Some("ignored".into())).contains("speaks protocol 7"),
      "the protocol reason must not be replaced by a stale warning"
    );
  }

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
        unless_negative(answer),
        "{answer:?} should store the password"
      );
    }
    for answer in ["n", "N", "no", " NO ", "0", "false", "FALSE"] {
      assert!(
        !unless_negative(answer),
        "{answer:?} should keep the password for this session only"
      );
    }
  }

  /// The start-up question defaults to yes like the remember one: only an
  /// explicit negative turns auto-connect off, so Enter keeps the default.
  #[test]
  fn the_startup_answer_defaults_to_yes() {
    assert!(unless_negative(""));
    assert!(unless_negative("\n"));
    assert!(unless_negative("Y"));
    assert!(!unless_negative("n"));
    assert!(!unless_negative("no"));
  }

  /// The autostart line names the entry either way; only the disabled form
  /// spends a second line on how to turn it on.
  #[test]
  fn a_disabled_autostart_line_names_the_way_out() {
    let on = autostart::LoginStatus {
      enabled: true,
      entry: "thunet.service".into(),
      hint: "unused".into(),
    };
    assert_eq!(autostart_words(&on), "autostart: enabled (thunet.service)");
    let off = autostart::LoginStatus {
      enabled: false,
      entry: "thunet-portable-0badc0de.desktop".into(),
      hint: "tick it in the settings page".into(),
    };
    assert_eq!(
      autostart_words(&off),
      "autostart: disabled (thunet-portable-0badc0de.desktop)\n  tick it in the settings page"
    );
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
      "startup (default yes)",
      "starts by itself",
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
      "connects at startup",
      "starts by itself at login",
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

  /// The three core-owned switches take `on`/`off`; leaving them out keeps the
  /// plain "show the file" form, and anything else is rejected.
  #[test]
  fn config_switches_are_on_or_off() {
    let cli = Cli::try_parse_from([
      "thunet",
      "config",
      "--auto-connect",
      "on",
      "--keep-online",
      "off",
      "--remember-password",
      "on",
    ])
    .expect("the switches parse");
    match cli.cmd {
      Some(Cmd::Config {
        auto_connect,
        keep_online,
        remember_password,
      }) => {
        assert_eq!(auto_connect, Some(Toggle::On));
        assert_eq!(keep_online, Some(Toggle::Off));
        assert_eq!(remember_password, Some(Toggle::On));
      }
      _ => panic!("wrong command"),
    }
    let bare = Cli::try_parse_from(["thunet", "config"]).expect("bare config parses");
    match bare.cmd {
      Some(Cmd::Config {
        auto_connect,
        keep_online,
        remember_password,
      }) => {
        assert!(auto_connect.is_none() && keep_online.is_none() && remember_password.is_none());
      }
      _ => panic!("wrong command"),
    }
    assert!(
      Cli::try_parse_from(["thunet", "config", "--auto-connect", "yes"]).is_err(),
      "only on and off are accepted"
    );
    // The short spelling is an alias, not another switch: both reach the same
    // field, and `--remember` alone is deliberately not a name any more.
    let alias =
      Cli::try_parse_from(["thunet", "config", "--remember-pwd", "off"]).expect("the alias parses");
    match alias.cmd {
      Some(Cmd::Config {
        remember_password, ..
      }) => {
        assert_eq!(remember_password, Some(Toggle::Off));
      }
      _ => panic!("wrong command"),
    }
    assert!(Cli::try_parse_from(["thunet", "config", "--remember", "on"]).is_err());
  }

  #[test]
  fn a_bare_login_leaves_an_online_session_alone() {
    assert!(already_online_with(
      session::State::LoggedIn,
      "alice",
      "alice"
    ));
    assert!(!already_online_with(
      session::State::LoggedIn,
      "bob",
      "alice"
    ));
    assert!(!already_online_with(
      session::State::LoggedOut,
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
      session_line(session::State::LoggedIn, "bob"),
      "session: online as bob"
    );
    assert_eq!(
      session_line(session::State::LoggedOut, "alice"),
      "session: offline (credential: alice)"
    );
    assert_eq!(
      session_line(session::State::LoggedOut, ""),
      "session: offline"
    );
    assert_eq!(
      session_line(session::State::Authenticating, "alice"),
      "session: authenticating as alice"
    );
  }
}
