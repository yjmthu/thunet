//! Autostart registration.
//!
//! Linux and macOS use the `auto-launch` crate (XDG autostart file and
//! LaunchAgent). Windows registers a **per-user Task Scheduler logon task**
//! instead of a `Run` value: the Task Scheduler service fires logon tasks
//! before `explorer.exe` walks `Run`, so the app is up noticeably earlier.
//! A `Run` value written by an older release is migrated on the next start.

use std::path::PathBuf;

/// Task name on Windows; also the value name a previous release wrote to
/// `Run`, and the name `auto-launch` uses for the desktop entry.
pub const APP_NAME: &str = "thunet";

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

  pub fn is_enabled(&self) -> bool {
    imp::is_enabled(self)
  }

  pub fn enable(&self) -> Result<(), String> {
    imp::enable(self)
  }

  pub fn disable(&self) -> Result<(), String> {
    imp::disable(self)
  }

  /// Replace an entry left behind by an older release. `Ok(true)` means one
  /// was found and rewritten; other platforms have nothing to migrate.
  pub fn migrate(&self) -> Result<bool, String> {
    imp::migrate(self)
  }
}

#[cfg(windows)]
mod imp {
  use std::os::windows::process::CommandExt as _;
  use std::path::{Path, PathBuf};

  use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
  };

  use super::{APP_NAME, Autostart};

  /// Task Scheduler location (`\thunet`).
  const TASK_NAME: &str = "thunet";
  /// `Run` value a previous release used instead of the logon task.
  const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
  /// Task Manager's "startup apps" enable flag for that value.
  const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
  const ERROR_MORE_DATA: u32 = 234;

  /// Never flash a console window: this binary uses the GUI subsystem.
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;

  pub fn is_enabled(_a: &Autostart) -> bool {
    task_exists() || legacy_run_present()
  }

  pub fn enable(a: &Autostart) -> Result<(), String> {
    register(&a.exe, a.args())
  }

  pub fn disable(_a: &Autostart) -> Result<(), String> {
    remove_legacy_run();
    if !task_exists() {
      return Ok(());
    }
    expect_success(&["/Delete", "/TN", TASK_NAME, "/F"])
  }

  /// Move an entry written by an older release to the logon task, keeping the
  /// executable it recorded: migrating to `current_exe()` would repoint the
  /// entry at a build that was merely launched once from another directory.
  pub fn migrate(a: &Autostart) -> Result<bool, String> {
    if !legacy_run_present() {
      return Ok(false);
    }
    let exe = legacy_exe().unwrap_or_else(|| a.exe.clone());
    register(&exe, a.args())?;
    Ok(true)
  }

  fn register(exe: &Path, args: &[&str]) -> Result<(), String> {
    let user = current_user()?;
    let xml = task_xml(exe, args, &user);
    let path = std::env::temp_dir().join(format!("thunet-autostart-{}.xml", std::process::id()));
    write_utf16(&path, &xml)?;
    let result = expect_success(&[
      "/Create",
      "/TN",
      TASK_NAME,
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

  fn task_exists() -> bool {
    run(&["/Query", "/TN", TASK_NAME])
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
  fn task_xml(exe: &Path, args: &[&str], user: &str) -> String {
    let args = args.join(" ");
    let args = if args.is_empty() {
      String::new()
    } else {
      format!("      <Arguments>{}</Arguments>\n", esc(&args))
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
  use auto_launch::{AutoLaunch, AutoLaunchBuilder};

  use super::{APP_NAME, Autostart};

  fn launcher(a: &Autostart) -> Result<AutoLaunch, String> {
    AutoLaunchBuilder::new()
      .set_app_name(APP_NAME)
      .set_app_path(&a.exe.to_string_lossy())
      .set_args(a.args())
      .build()
      .map_err(|e| e.to_string())
  }

  pub fn is_enabled(a: &Autostart) -> bool {
    launcher(a)
      .and_then(|l| l.is_enabled().map_err(|e| e.to_string()))
      .unwrap_or(false)
  }

  pub fn enable(a: &Autostart) -> Result<(), String> {
    launcher(a)?.enable().map_err(|e| e.to_string())
  }

  pub fn disable(a: &Autostart) -> Result<(), String> {
    launcher(a)?.disable().map_err(|e| e.to_string())
  }

  pub fn migrate(_a: &Autostart) -> Result<bool, String> {
    Ok(false)
  }
}
