//! Autostart registration.
//!
//! Two things are registered: the window/tray entry, and the background core
//! that owns the network session (`thunet core`). The core needs no display, so
//! it is the half that can start before anybody logs in.
//!
//! Both platforms call the core entry `thunet` and the frontend `thunet-tray`,
//! so the mental model is the same everywhere: on Linux that is
//! `thunet.service` plus `~/.config/autostart/thunet.desktop`, on Windows two
//! Task Scheduler logon tasks.
//!
//! Linux and macOS use the `auto-launch` crate (XDG autostart file and
//! LaunchAgent) for the tray; on Linux the core gets a `systemd --user` unit.
//! Windows registers **per-user Task Scheduler logon tasks** instead of `Run`
//! values: the Task Scheduler service fires logon tasks before `explorer.exe`
//! walks `Run`, so the app is up noticeably earlier. Both tasks run as
//! `InteractiveToken` + `LeastPrivilege`, i.e. with the user's own token and no
//! elevation, which is what keeps the credential store and location permission
//! working. A `Run` value written by an older release is migrated on the next
//! start, and so is a pre-split install that only had the tray task.

use std::path::PathBuf;

/// Task name on Windows; also the value name a previous release wrote to
/// `Run`, and the name `auto-launch` uses for the desktop entry.
pub const APP_NAME: &str = "thunet";

/// How much of the app starts with the system.
///
/// There are two entries (tray and background core). The tray never starts a
/// core by itself, so "tray only" is not offered: it would put an icon on
/// screen whose only message is "service not running".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// Nothing starts automatically.
  Off,
  /// Only `thunet core`: the session is kept alive without a tray icon.
  Core,
  /// Core and tray.
  All,
}

/// What `thunet service <action>` does. The names follow systemd, the Windows
/// side maps them onto Task Scheduler commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceAction {
  /// Register the unit / task without starting anything.
  Install,
  /// Remove the registration.
  Uninstall,
  /// Start automatically from now on.
  Enable,
  /// Stop starting automatically (a running core keeps running).
  Disable,
  Start,
  Stop,
  Restart,
  /// Print what the platform's service manager says.
  Status,
}

pub struct Autostart {
  exe: PathBuf,
  minimized: bool,
}

impl Autostart {
  /// `None` when the running executable path cannot be determined.
  pub fn new(minimized: bool) -> Option<Self> {
    Some(Self {
      exe: std::env::current_exe().ok()?,
      minimized,
    })
  }

  fn args(&self) -> &'static [&'static str] {
    if self.minimized {
      &["--minimized"]
    } else {
      &[]
    }
  }

  /// Which entries are registered right now.
  ///
  /// Derived from the OS, never cached in `config.json`: deleting a task or a
  /// unit by hand is reflected immediately, and there is no second source of
  /// truth to drift.
  pub fn mode(&self) -> Mode {
    if imp::tray_registered(self) {
      // A tray entry on its own cannot happen through this API; treat a
      // half-installed state as "all" and let `migrate` repair it.
      Mode::All
    } else if imp::core_registered() {
      Mode::Core
    } else {
      Mode::Off
    }
  }

  pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
    match mode {
      Mode::Off => {
        imp::disable_tray(self)?;
        imp::disable_core()
      }
      Mode::Core => {
        imp::disable_tray(self)?;
        imp::enable_core(self)
      }
      Mode::All => {
        imp::enable_tray(self)?;
        imp::enable_core(self)
      }
    }
  }

  /// Replace an entry left behind by an older release. `Ok(true)` means one
  /// was found and rewritten; other platforms have nothing to migrate.
  pub fn migrate(&self) -> Result<bool, String> {
    imp::migrate(self)
  }

  /// `thunet service <action>`: run it and return the line to print.
  pub fn service(action: ServiceAction, minimized: bool) -> Result<String, String> {
    let me = Autostart::new(minimized).ok_or("cannot determine the executable path")?;
    imp::service(action, &me)
  }
}

/// Is the core registered with the OS as something that starts on its own?
///
/// A registered core belongs to systemd / Task Scheduler, so the tray must ask
/// the manager to start it instead of spawning a second, unmanaged copy.
pub fn core_managed() -> bool {
  imp::core_registered()
}

#[cfg(windows)]
mod imp {
  use std::os::windows::process::CommandExt as _;
  use std::path::{Path, PathBuf};

  use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
  };

  use super::{APP_NAME, Autostart, ServiceAction};

  /// Task Scheduler location of the background core. Same name as the Linux
  /// unit (`thunet.service`), so the mental model is identical on both
  /// platforms: `thunet` *is* the service, `thunet-tray` is the frontend.
  const CORE_TASK: &str = "thunet";
  /// Task Scheduler location of the window/tray process.
  const TRAY_TASK: &str = "thunet-tray";
  /// `Run` value a previous release used instead of the logon task.
  const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
  /// Task Manager's "startup apps" enable flag for that value.
  const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
  const ERROR_MORE_DATA: u32 = 234;

  /// Never flash a console window: this binary uses the GUI subsystem.
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;

  pub fn tray_registered(_a: &Autostart) -> bool {
    task_exists(TRAY_TASK) || legacy_run_present()
  }

  pub fn core_registered() -> bool {
    task_exists(CORE_TASK)
  }

  pub fn enable_tray(a: &Autostart) -> Result<(), String> {
    register(&a.exe, a.args(), TRAY_TASK, false)
  }

  pub fn disable_tray(_a: &Autostart) -> Result<(), String> {
    remove_legacy_run();
    delete_task(TRAY_TASK)
  }

  /// A task always carries the trigger that starts it, so Windows cannot tell
  /// "installed" from "enabled": registering *is* enabling.
  pub fn enable_core(a: &Autostart) -> Result<(), String> {
    register(&a.exe, &["core"], CORE_TASK, true)
  }

  pub fn disable_core() -> Result<(), String> {
    delete_task(CORE_TASK)
  }

  /// `thunet service <action>`: the Task Scheduler equivalents of
  /// `systemctl --user …`.
  pub fn service(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    match action {
      ServiceAction::Install | ServiceAction::Enable => {
        enable_core(a)?;
        Ok(
          "core logon task registered (a Windows task cannot be installed without its trigger)"
            .into(),
        )
      }
      ServiceAction::Uninstall | ServiceAction::Disable => {
        disable_core()?;
        Ok("core logon task removed".into())
      }
      ServiceAction::Start => {
        expect_success(&["/Run", "/TN", CORE_TASK])?;
        Ok("core started".into())
      }
      ServiceAction::Stop => {
        expect_success(&["/End", "/TN", CORE_TASK])?;
        Ok("core stopped".into())
      }
      ServiceAction::Restart => {
        let _ = expect_success(&["/End", "/TN", CORE_TASK]);
        expect_success(&["/Run", "/TN", CORE_TASK])?;
        Ok("core restarted".into())
      }
      ServiceAction::Status => {
        let out = run(&["/Query", "/TN", CORE_TASK, "/V", "/FO", "LIST"])
          .map_err(|e| format!("schtasks: {e}"))?;
        let mut text = decode(&out.stdout);
        text.push_str(&decode(&out.stderr));
        Ok(text.trim_end().to_string())
      }
    }
  }

  fn delete_task(name: &str) -> Result<(), String> {
    if !task_exists(name) {
      return Ok(());
    }
    expect_success(&["/Delete", "/TN", name, "/F"])
  }

  /// `schtasks /XML` and `/FO LIST` print UTF-16 with a BOM on some consoles
  /// and plain (OEM/UTF-8) on others; accept either.
  fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
      let units: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
      String::from_utf16_lossy(&units)
    } else {
      String::from_utf8_lossy(bytes).into_owned()
    }
  }

  /// Bring an installation from an older layout onto the current task names.
  ///
  /// Two things exist in the wild: the `Run` value used by 0.1.1 and earlier,
  /// and the single tray task named `thunet` used by 0.1.2/0.1.3. The split
  /// build never shipped for Windows, so there is no third layout to handle.
  pub fn migrate(a: &Autostart) -> Result<bool, String> {
    let mut changed = false;
    // 0.1.x kept the tray task at `thunet`, which is now the core's name. The
    // two cannot be told apart afterwards, so rewrite the core task with the
    // arguments it must have: an old tray task then becomes a real core task
    // (the tray can be re-enabled in the settings), and a current Core-only
    // install is merely rewritten identically. Not reported as a change, so a
    // Core-only install does not log an upgrade it did not have.
    if task_exists(CORE_TASK) && !task_exists(TRAY_TASK) {
      register(&a.exe, &["core"], CORE_TASK, true)?;
    }
    // Move an entry written by an even older release to the logon task, keeping
    // the executable it recorded: `current_exe()` would repoint it at a build
    // that was merely launched once from another directory.
    if legacy_run_present() {
      let exe = legacy_exe().unwrap_or_else(|| a.exe.clone());
      register(&exe, a.args(), TRAY_TASK, false)?;
      changed = true;
    }
    // The tray no longer starts a core by itself, so an installation that
    // autostarts the tray also gets the core entry: an icon with no session
    // behind it would look broken.
    if task_exists(TRAY_TASK) && !task_exists(CORE_TASK) {
      register(&a.exe, &["core"], CORE_TASK, true)?;
      changed = true;
    }
    Ok(changed)
  }

  fn register(exe: &Path, args: &[&str], name: &str, restart: bool) -> Result<(), String> {
    let user = current_user()?;
    let xml = task_xml(exe, args, &user, restart);
    let path = std::env::temp_dir().join(format!("thunet-autostart-{}.xml", std::process::id()));
    write_utf16(&path, &xml)?;
    let result = expect_success(&[
      "/Create",
      "/TN",
      name,
      "/XML",
      &path.to_string_lossy(),
      "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    result?;
    // The task supersedes the `Run` value older releases wrote; leaving it in
    // place would start a second copy at logon.
    remove_legacy_run();
    Ok(())
  }

  fn task_exists(name: &str) -> bool {
    run(&["/Query", "/TN", name])
      .map(|out| out.status.success())
      .unwrap_or(false)
  }

  /// `Err` only when `schtasks.exe` cannot be started at all; a non-zero exit
  /// status is returned as `Ok` so callers can interpret it.
  fn run(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new(schtasks())
      .args(args)
      .creation_flags(CREATE_NO_WINDOW)
      .output()
      .map_err(|e| format!("schtasks: {e}"))
  }

  fn expect_success(args: &[&str]) -> Result<(), String> {
    let out = run(args)?;
    if out.status.success() {
      return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
      format!("schtasks exited with {:?}", out.status.code())
    } else {
      err
    })
  }

  /// Full path so a hijacked `PATH` cannot redirect the call.
  fn schtasks() -> std::ffi::OsString {
    std::env::var_os("SystemRoot")
      .map(|root| {
        Path::new(&root)
          .join("System32")
          .join("schtasks.exe")
          .into_os_string()
      })
      .unwrap_or_else(|| "schtasks.exe".into())
  }

  /// The task runs as its owner, so the account must be spelled out; the
  /// domain is the machine name for local accounts and the tenant for AAD ones.
  fn current_user() -> Result<String, String> {
    let name = std::env::var("USERNAME").map_err(|_| "USERNAME is not set".to_string())?;
    Ok(match std::env::var("USERDOMAIN") {
      Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
      _ => name,
    })
  }

  fn legacy_run_present() -> bool {
    read_legacy_run().is_some()
  }

  /// The command line a previous release stored under `Run`, if any.
  fn read_legacy_run() -> Option<String> {
    let key = wide(RUN_KEY);
    let name = wide(APP_NAME);
    let mut size = 0u32;
    // SAFETY: a null buffer and zero length only report the required size.
    let code = unsafe {
      RegGetValueW(
        HKEY_CURRENT_USER,
        key.as_ptr(),
        name.as_ptr(),
        RRF_RT_REG_SZ,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut size,
      )
    };
    if !(code == 0 || code == ERROR_MORE_DATA) || size < 2 {
      return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `size` is the byte count the first call asked for.
    let code = unsafe {
      RegGetValueW(
        HKEY_CURRENT_USER,
        key.as_ptr(),
        name.as_ptr(),
        RRF_RT_REG_SZ,
        std::ptr::null_mut(),
        buf.as_mut_ptr().cast(),
        &mut size,
      )
    };
    if code != 0 {
      return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
  }

  /// Executable recorded by the old `Run` value, when it still exists.
  fn legacy_exe() -> Option<PathBuf> {
    let path = parse_command(&read_legacy_run()?)?;
    path.is_file().then_some(path)
  }

  /// `auto-launch` stored `<path> <args>` without quoting the path, so an
  /// argument separator ends it; quoted command lines end at the quote.
  fn parse_command(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('"') {
      return Some(PathBuf::from(&rest[..rest.find('"')?]));
    }
    let end = raw.find(" --").unwrap_or(raw.len());
    Some(PathBuf::from(raw[..end].trim_end()))
  }

  /// Deleting a missing value or key just fails; nothing to clean up then.
  fn remove_legacy_run() {
    for key in [RUN_KEY, APPROVED_KEY] {
      let key = wide(key);
      let name = wide(APP_NAME);
      // SAFETY: both strings are NUL-terminated and outlive the call.
      unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
      }
    }
  }

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  /// `schtasks /Create /XML` wants a Unicode file with a BOM.
  fn write_utf16(path: &Path, text: &str) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in text.encode_utf16() {
      bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
  }

  /// A logon task with no delay: same trigger the other login-time clients
  /// use, so the app is scheduled alongside them rather than after the shell.
  fn task_xml(exe: &Path, args: &[&str], user: &str, restart: bool) -> String {
    let args = args.join(" ");
    let args = if args.is_empty() {
      String::new()
    } else {
      format!("      <Arguments>{}</Arguments>\n", esc(&args))
    };
    // The core must come back after a crash; the tray is restarted by the user.
    let restart = if restart {
      concat!(
        "    <RestartOnFailure>\n",
        "      <Count>3</Count>\n",
        "      <Interval>PT1M</Interval>\n",
        "    </RestartOnFailure>\n",
      )
    } else {
      ""
    };
    format!(
      concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n",
        "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n",
        "  <RegistrationInfo>\n",
        "    <Description>THUnet autostart</Description>\n",
        "  </RegistrationInfo>\n",
        "  <Triggers>\n",
        "    <LogonTrigger>\n",
        "      <Enabled>true</Enabled>\n",
        "      <UserId>{user}</UserId>\n",
        "    </LogonTrigger>\n",
        "  </Triggers>\n",
        "  <Principals>\n",
        "    <Principal id=\"Author\">\n",
        "      <UserId>{user}</UserId>\n",
        "      <LogonType>InteractiveToken</LogonType>\n",
        "      <RunLevel>LeastPrivilege</RunLevel>\n",
        "    </Principal>\n",
        "  </Principals>\n",
        "  <Settings>\n",
        "    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n",
        "{restart}",
        "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n",
        "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n",
        "    <AllowHardTerminate>true</AllowHardTerminate>\n",
        "    <StartWhenAvailable>false</StartWhenAvailable>\n",
        "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n",
        "    <IdleSettings>\n",
        "      <StopOnIdleEnd>false</StopOnIdleEnd>\n",
        "      <RestartOnIdle>false</RestartOnIdle>\n",
        "    </IdleSettings>\n",
        "    <AllowStartOnDemand>true</AllowStartOnDemand>\n",
        "    <Enabled>true</Enabled>\n",
        "    <Hidden>false</Hidden>\n",
        "    <RunOnlyIfIdle>false</RunOnlyIfIdle>\n",
        "    <WakeToRun>false</WakeToRun>\n",
        "    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n",
        "    <Priority>7</Priority>\n",
        "  </Settings>\n",
        "  <Actions Context=\"Author\">\n",
        "    <Exec>\n",
        "      <Command>{exe}</Command>\n",
        "{args}",
        "    </Exec>\n",
        "  </Actions>\n",
        "</Task>\n"
      ),
      user = esc(user),
      exe = esc(&exe.to_string_lossy()),
      args = args,
      restart = restart,
    )
  }

  fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
      .replace('<', "&lt;")
      .replace('>', "&gt;")
  }

  #[cfg(test)]
  mod tests {
    use super::{esc, parse_command, task_xml};
    use std::path::{Path, PathBuf};

    #[test]
    fn xml_carries_the_command_line() {
      let xml = task_xml(
        Path::new(r"C:\Tools\thunet.exe"),
        &["--minimized"],
        "PC\\me",
      );
      assert!(xml.contains("<Command>C:\\Tools\\thunet.exe</Command>"));
      assert!(xml.contains("<Arguments>--minimized</Arguments>"));
      assert!(xml.contains("<UserId>PC\\me</UserId>"));
    }

    #[test]
    fn xml_omits_empty_arguments() {
      let xml = task_xml(Path::new(r"C:\Tools\thunet.exe"), &[], "PC\\me");
      assert!(!xml.contains("<Arguments>"));
    }

    #[test]
    fn xml_escapes_markup() {
      assert_eq!(esc("a&b<c>d"), "a&amp;b&lt;c&gt;d");
      let xml = task_xml(Path::new(r"C:\a&b\thunet.exe"), &[], "PC\\me");
      assert!(xml.contains(r"<Command>C:\a&amp;b\thunet.exe</Command>"));
    }

    #[test]
    fn legacy_command_is_split_into_path_and_args() {
      let exe = PathBuf::from(r"C:\Tools\thunet.exe");
      assert_eq!(
        parse_command(r#""C:\Program Files\thunet\thunet.exe" --minimized"#),
        Some(PathBuf::from(r"C:\Program Files\thunet\thunet.exe"))
      );
      assert_eq!(
        parse_command(r"C:\Tools\thunet.exe --minimized"),
        Some(exe.clone())
      );
      assert_eq!(parse_command(r" C:\Tools\thunet.exe "), Some(exe));
    }
  }
}

#[cfg(not(windows))]
mod imp {
  use std::path::PathBuf;

  use auto_launch::{AutoLaunch, AutoLaunchBuilder};

  use super::{APP_NAME, Autostart, ServiceAction};

  /// The background core as a systemd *user* unit. It needs no display, so it
  /// can start with the user manager (`WantedBy=default.target`) and — with
  /// `loginctl enable-linger` — at boot, before anybody logs in.
  ///
  /// Named `thunet.service` so the habitual
  /// `systemctl --user start|stop|status thunet` works without the suffix.
  const CORE_UNIT: &str = "thunet.service";

  fn launcher(a: &Autostart) -> Result<AutoLaunch, String> {
    AutoLaunchBuilder::new()
      .set_app_name(APP_NAME)
      .set_app_path(&a.exe.to_string_lossy())
      .set_args(a.args())
      .build()
      .map_err(|e| e.to_string())
  }

  pub fn tray_registered(a: &Autostart) -> bool {
    launcher(a)
      .and_then(|l| l.is_enabled().map_err(|e| e.to_string()))
      .unwrap_or(false)
  }

  pub fn core_registered() -> bool {
    unit_enabled()
  }

  /// The tray keeps using the XDG autostart entry: every desktop honours it,
  /// including the ones that never pull `graphical-session.target` (i3, sway).
  pub fn enable_tray(a: &Autostart) -> Result<(), String> {
    launcher(a)?.enable().map_err(|e| e.to_string())
  }

  pub fn disable_tray(a: &Autostart) -> Result<(), String> {
    launcher(a)?.disable().map_err(|e| e.to_string())
  }

  /// Write the unit (if needed) and put it in the boot/login path.
  pub fn enable_core(a: &Autostart) -> Result<(), String> {
    install_unit(a)?;
    enable_unit()
  }

  /// Stop starting the core automatically. A running core keeps running: the
  /// switch is about autostart, not about killing the session.
  pub fn disable_core() -> Result<(), String> {
    disable_unit()
  }

  /// Keep an existing installation working: an older build only had the tray
  /// entry, and a portable one has no unit at all.
  pub fn migrate(a: &Autostart) -> Result<bool, String> {
    let mut changed = false;
    if tray_registered(a) && !unit_enabled() {
      enable_core(a)?;
      changed = true;
    }
    if !unit_installed() {
      // Installed but not enabled: `systemctl --user start thunet` works
      // without turning autostart on.
      install_unit(a)?;
      changed = true;
    }
    Ok(changed)
  }

  pub fn service(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    match action {
      ServiceAction::Install => {
        install_unit(a)?;
        Ok(format!("installed {CORE_UNIT} (not enabled)"))
      }
      ServiceAction::Uninstall => {
        remove_unit()?;
        Ok(format!("removed {CORE_UNIT}"))
      }
      ServiceAction::Enable => {
        enable_core(a)?;
        Ok("autostart enabled for the core".into())
      }
      ServiceAction::Disable => {
        disable_core()?;
        Ok("autostart disabled for the core".into())
      }
      ServiceAction::Start => {
        systemctl(&["start", CORE_UNIT])?;
        Ok("core started".into())
      }
      ServiceAction::Stop => {
        systemctl(&["stop", CORE_UNIT])?;
        Ok("core stopped".into())
      }
      ServiceAction::Restart => {
        systemctl(&["restart", CORE_UNIT])?;
        Ok("core restarted".into())
      }
      ServiceAction::Status => Ok(systemctl_output(&["status", "--no-pager", CORE_UNIT])),
    }
  }

  #[cfg(target_os = "linux")]
  fn unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
      .map(PathBuf::from)
      .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("systemd").join("user").join(CORE_UNIT))
  }

  #[cfg(target_os = "linux")]
  fn unit_text(a: &Autostart) -> String {
    format!(
      "[Unit]\n\
       Description=THUnet campus network client (background core)\n\
       After=network.target\n\
       \n\
       [Service]\n\
       Type=simple\n\
       ExecStart=\"{}\" core\n\
       Restart=on-failure\n\
       RestartSec=3\n\
       \n\
       [Install]\n\
       WantedBy=default.target\n",
      a.exe.display()
    )
  }

  #[cfg(target_os = "linux")]
  fn unit_installed() -> bool {
    unit_path().map(|p| p.exists()).unwrap_or(false)
  }

  #[cfg(target_os = "linux")]
  fn install_unit(a: &Autostart) -> Result<(), String> {
    let path = unit_path().ok_or("cannot locate ~/.config")?;
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = unit_text(a);
    // Rewriting an unchanged unit would trigger a reload for nothing.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
      std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
      let _ = systemctl(&["daemon-reload"]);
    }
    Ok(())
  }

  #[cfg(target_os = "linux")]
  fn unit_enabled() -> bool {
    systemctl(&["is-enabled", "--quiet", CORE_UNIT]).is_ok()
  }

  #[cfg(target_os = "linux")]
  fn enable_unit() -> Result<(), String> {
    // `--now` also starts it, so the switch takes effect without a re-login.
    systemctl(&["enable", "--now", CORE_UNIT])?;
    // Best effort: without linger the unit starts at login instead of at boot.
    // Granting it may need a policy prompt, so a failure is not an error here.
    let _ = std::process::Command::new("loginctl")
      .arg("enable-linger")
      .output();
    Ok(())
  }

  #[cfg(target_os = "linux")]
  fn disable_unit() -> Result<(), String> {
    systemctl(&["disable", CORE_UNIT])
  }

  #[cfg(target_os = "linux")]
  fn remove_unit() -> Result<(), String> {
    let _ = systemctl(&["disable", "--now", CORE_UNIT]);
    if let Some(path) = unit_path() {
      let _ = std::fs::remove_file(path);
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
  }

  #[cfg(target_os = "linux")]
  fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
      .arg("--user")
      .args(args)
      .output()
      .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
      Ok(())
    } else {
      Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
  }

  /// Whatever `systemctl` printed, for `thunet service status`. An inactive
  /// unit exits non-zero, which is information rather than an error here.
  #[cfg(target_os = "linux")]
  fn systemctl_output(args: &[&str]) -> String {
    match std::process::Command::new("systemctl")
      .arg("--user")
      .args(args)
      .output()
    {
      Ok(out) => {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        text.trim_end().to_string()
      }
      Err(e) => format!("systemctl: {e}"),
    }
  }

  /// macOS: the core is started by the tray (a LaunchAgent has no equivalent of
  /// a display-less unit), so there is nothing extra to register.
  #[cfg(not(target_os = "linux"))]
  fn unit_installed() -> bool {
    false
  }

  #[cfg(not(target_os = "linux"))]
  fn install_unit(_a: &Autostart) -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn unit_enabled() -> bool {
    // The LaunchAgent for the tray is what starts the core, so "core managed"
    // is not a separate thing here.
    false
  }

  #[cfg(not(target_os = "linux"))]
  fn enable_unit() -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn disable_unit() -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn remove_unit() -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn systemctl_output(_args: &[&str]) -> String {
    "no service manager on this platform".into()
  }
}
