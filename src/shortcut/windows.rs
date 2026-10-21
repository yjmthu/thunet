//! Windows: a `.lnk` on the desktop, pointing at the running executable.

use std::os::windows::process::CommandExt as _;
use std::path::Path;

/// Never flash a console window: this binary uses the GUI subsystem.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Create (or refresh) the desktop shortcut for this executable.
pub fn create_desktop_shortcut() -> Result<(), String> {
  let exe = crate::exe_path().ok_or("cannot determine the executable path")?;
  shortcut("Desktop", "THUnet.lnk", &exe)
}

/// Remove the per-user Start Menu folder older packages created.
///
/// A package built before the switch to `CommonProgramMenuFolder` put the
/// entry in the profile of whoever installed it, so upgrading left two
/// identical entries. Only the installed copy may do this, and only while the
/// machine-wide entry exists: then the user copy is provably a duplicate, and
/// deleting the only way to start the app is worse than leaving a stray one.
pub fn remove_legacy_start_menu() -> Result<bool, String> {
  let exe = match crate::exe_path() {
    Some(exe) => exe,
    None => return Ok(false),
  };
  let installed = std::env::var_os("ProgramFiles")
    .map(std::path::PathBuf::from)
    .is_some_and(|dir| exe.starts_with(dir));
  if !installed {
    return Ok(false);
  }
  let (Some(appdata), Some(program_data)) = (
    std::env::var_os("APPDATA").map(std::path::PathBuf::from),
    std::env::var_os("ProgramData").map(std::path::PathBuf::from),
  ) else {
    return Ok(false);
  };
  let menu = |base: std::path::PathBuf| {
    base
      .join("Microsoft")
      .join("Windows")
      .join("Start Menu")
      .join("Programs")
      .join(crate::shortcut::APP_TITLE)
  };
  let legacy = menu(appdata);
  let common = menu(program_data);
  if !legacy.is_dir() || !common.is_dir() {
    return Ok(false);
  }
  std::fs::remove_dir_all(&legacy).map_err(|e| format!("{}: {e}", legacy.display()))?;
  Ok(true)
}

/// `[Environment]::GetFolderPath` follows OneDrive-redirected folders, which is
/// why the `.lnk` handling is delegated to PowerShell instead of hardcoding
/// `%USERPROFILE%\Desktop`.
fn shortcut(folder: &str, name: &str, target: &Path) -> Result<(), String> {
  let workdir = target.parent().unwrap_or(Path::new("."));
  let target = esc(&target.to_string_lossy());
  let script = format!(
    concat!(
      "$p = Join-Path ([Environment]::GetFolderPath('{folder}')) '{name}'; ",
      "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($p); ",
      "$s.TargetPath = '{target}'; ",
      "$s.WorkingDirectory = '{workdir}'; ",
      "$s.IconLocation = '{target},0'; ",
      "$s.Description = '{title}'; ",
      "$s.Save()"
    ),
    folder = folder,
    name = name,
    target = target,
    workdir = esc(&workdir.to_string_lossy()),
    title = crate::shortcut::APP_TITLE,
  );
  powershell(&script)
}

/// Escape a value for embedding in a single-quoted PowerShell string.
fn esc(s: &str) -> String {
  s.replace('\'', "''")
}

fn powershell(script: &str) -> Result<(), String> {
  let out = std::process::Command::new("powershell")
    .args([
      "-NoProfile",
      "-NonInteractive",
      "-WindowStyle",
      "Hidden",
      "-Command",
      script,
    ])
    .creation_flags(CREATE_NO_WINDOW)
    .output()
    .map_err(|e| format!("powershell: {e}"))?;
  if out.status.success() {
    return Ok(());
  }
  let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
  Err(if err.is_empty() {
    format!("powershell exited with {:?}", out.status.code())
  } else {
    err
  })
}
