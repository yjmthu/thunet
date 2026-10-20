//! Linux: a `.desktop` launcher on the desktop, pointing at this executable.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
  std::env::var_os("HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
  std::env::var_os(var)
    .filter(|v| !v.is_empty())
    .map(PathBuf::from)
    .unwrap_or_else(|| home().join(fallback))
}

fn data_home() -> PathBuf {
  xdg_dir("XDG_DATA_HOME", ".local/share")
}

fn config_home() -> PathBuf {
  xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// `XDG_DESKTOP_DIR` from `~/.config/user-dirs.dirs`, else `~/Desktop`.
fn desktop_dir() -> PathBuf {
  if let Ok(text) = fs::read_to_string(config_home().join("user-dirs.dirs")) {
    for line in text.lines() {
      let Some(rest) = line.trim().strip_prefix("XDG_DESKTOP_DIR=") else {
        continue;
      };
      let value = rest
        .trim()
        .trim_matches('"')
        .replace("$HOME", &home().to_string_lossy());
      if !value.is_empty() {
        return PathBuf::from(value);
      }
    }
  }
  home().join("Desktop")
}

/// Text-only icon so no PNG encoder (and no extra crate) is needed.
const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <circle cx="32" cy="32" r="26" fill="#2ea043"/>
  <path d="M21 32h22M32 21v22" stroke="#ffffff" stroke-width="6" stroke-linecap="round"/>
</svg>
"##;

fn make_executable(path: &Path) -> Result<(), String> {
  let mut perm = fs::metadata(path)
    .map_err(|e| format!("{}: {e}", path.display()))?
    .permissions();
  perm.set_mode(0o755);
  fs::set_permissions(path, perm).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_desktop_file(path: &Path, exe: &Path) -> Result<(), String> {
  let body = format!(
    "[Desktop Entry]\n\
     Type=Application\n\
     Version=1.0\n\
     Name={title}\n\
     Comment=Tsinghua campus network client (login, keepalive, status)\n\
     Exec=\"{exe}\"\n\
     Icon=thunet\n\
     Terminal=false\n\
     Categories=Network;Utility;\n\
     Keywords=tsinghua;tunet;network;login;\n\
     StartupWMClass={title}\n",
    title = crate::shortcut::APP_TITLE,
    exe = exe.display(),
  );
  fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))?;
  make_executable(path)
}

fn install_icon() -> Result<(), String> {
  let path = data_home().join("icons/hicolor/scalable/apps/thunet.svg");
  if let Some(dir) = path.parent() {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
  }
  fs::write(&path, ICON_SVG).map_err(|e| format!("{}: {e}", path.display()))
}

/// Create (or refresh) `~/Desktop/thunet.desktop`.
pub fn create_desktop_shortcut() -> Result<(), String> {
  let exe = std::env::current_exe().map_err(|e| e.to_string())?;
  let dir = desktop_dir();
  fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
  let path = dir.join("thunet.desktop");
  write_desktop_file(&path, &exe)?;
  install_icon()?;
  // GNOME refuses to launch desktop files that are not marked as trusted.
  let _ = std::process::Command::new("gio")
    .args(["set", &path.to_string_lossy(), "metadata::trusted", "true"])
    .status();
  Ok(())
}
