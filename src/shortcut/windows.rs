//! Windows: a `.lnk` on the desktop, pointing at the running executable.

use std::os::windows::process::CommandExt as _;
use std::path::Path;

/// Never flash a console window: this binary uses the GUI subsystem.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Create (or refresh) the desktop shortcut for this executable.
pub fn create_desktop_shortcut() -> Result<(), String> {
  let exe = std::env::current_exe().map_err(|e| e.to_string())?;
  shortcut("Desktop", "THUnet.lnk", &exe)
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
