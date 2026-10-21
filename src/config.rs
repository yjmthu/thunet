//! Persistent settings in `%APPDATA%/thunet/config.json`.
//!
//! The password is never written to the JSON file: `password` is skipped by
//! serde and the account/password pair is sealed into `credential.bin` (see
//! [`crate::secret`]), and only when the user ticks "remember password".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::secret;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
  pub username: String,
  /// Runtime only: never serialized. Persisted by [`crate::secret`].
  #[serde(skip)]
  pub password: String,
  #[serde(default)]
  pub remember_password: bool,
  pub host: String,
  pub https: bool,
  pub ac_id: String,
  /// Empty lets the portal infer the caller address.
  pub ip: String,
  pub auto_connect: bool,
  /// Whether the autostart entry starts the app hidden. Manual launches (Start
  /// Menu, desktop shortcut, installer) open the window regardless.
  pub start_minimized: bool,
  pub keepalive_target: String,
  pub keepalive_interval_secs: u64,
  pub reconnect_delay_secs: u64,
  /// "auto" (follow OS) | "zh" | "en"
  pub lang: String,
  /// Minimum level shown in the window log: "debug" | "info" | "warn" | "error".
  /// Defaults to `info`, so only diagnostics the user asked for show up.
  pub log_level: String,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      username: String::new(),
      password: String::new(),
      remember_password: false,
      host: "auth4.tsinghua.edu.cn".into(),
      https: true,
      ac_id: "auto".into(),
      ip: String::new(),
      auto_connect: true,
      start_minimized: false,
      keepalive_target: "https://www.tsinghua.edu.cn/".into(),
      keepalive_interval_secs: 300,
      reconnect_delay_secs: 5,
      lang: "auto".into(),
      log_level: "info".into(),
    }
  }
}

fn env_path(key: &str) -> PathBuf {
  std::env::var_os(key)
    .filter(|v| !v.is_empty())
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."))
}

/// Same code path on every platform, using each OS's conventional location:
///
/// | OS      | directory                              |
/// | ------- | -------------------------------------- |
/// | Windows | `%APPDATA%\thunet`                     |
/// | macOS   | `~/Library/Application Support/thunet`  |
/// | Linux   | `$XDG_CONFIG_HOME/thunet` (or `~/.config/thunet`) |
///
/// `THUNET_STATE_DIR` overrides all of them. The Windows service and the
/// Linux system unit pass it explicitly so the core uses the machine directory
/// while the window keeps using the user's own directory.
pub fn dir() -> PathBuf {
  state_dir()
}

/// Per-user settings: language, log level, tray autostart, window preferences.
pub fn user_dir() -> PathBuf {
  state_dir()
}

/// The user's own profile directory, or `THUNET_STATE_DIR` when set.
fn state_dir() -> PathBuf {
  if let Some(dir) = std::env::var_os("THUNET_STATE_DIR").filter(|v| !v.is_empty()) {
    return PathBuf::from(dir);
  }
  let base = if cfg!(windows) {
    env_path("APPDATA")
  } else if cfg!(target_os = "macos") {
    env_path("HOME").join("Library").join("Application Support")
  } else {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
      Some(xdg) => PathBuf::from(xdg),
      None => env_path("HOME").join(".config"),
    }
  };
  base.join("thunet")
}

/// Machine-wide state: the shared account, the portal host and the sealed
/// credential. Only the core (and an administrator) may read it.
pub fn machine_dir() -> PathBuf {
  if let Some(dir) = std::env::var_os("THUNET_STATE_DIR").filter(|v| !v.is_empty()) {
    return PathBuf::from(dir);
  }
  if cfg!(windows) {
    return env_path("ProgramData").join("thunet");
  }
  if cfg!(target_os = "linux") {
    return PathBuf::from("/var/lib/thunet");
  }
  // macOS has no machine-wide service in this design; keep the user directory.
  state_dir()
}

pub fn path() -> PathBuf {
  dir().join("config.json")
}

pub fn path_in(dir: &std::path::Path) -> PathBuf {
  dir.join("config.json")
}

impl Config {
  /// Load the per-user settings used by the window.
  pub fn load() -> Self {
    Self::load_from(&user_dir())
  }

  /// Load the machine-wide settings used by the core.
  pub fn load_machine() -> Self {
    Self::load_from(&machine_dir())
  }

  pub fn load_from(dir: &std::path::Path) -> Self {
    let path = path_in(dir);
    let mut cfg: Config = match std::fs::read_to_string(&path) {
      Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
      Err(e) => {
        // A file this account cannot open - an empty DACL left by an older
        // install, or EFS under another account - is unusable to us and would
        // also fail every later save. Remove it through the directory's
        // delete-child right so the next save creates a healthy one; the
        // defaults below are all that can be recovered anyway.
        if e.kind() != std::io::ErrorKind::NotFound {
          let _ = std::fs::remove_file(&path);
        }
        Config::default()
      }
    };
    if cfg.remember_password {
      match secret::load_in(dir) {
        Some((user, pass)) => {
          if cfg.username.is_empty() {
            cfg.username = user;
          }
          cfg.password = pass;
        }
        // Nothing usable is stored: a blob sealed by another machine, or a
        // cleared entry. Drop what is there — an unreadable blob helps nobody —
        // and fall back to typing the password.
        None => {
          cfg.remember_password = false;
          secret::delete_in(dir);
        }
      }
    }
    cfg
  }

  pub fn save(&self) -> Result<(), String> {
    self.save_to(&dir())
  }

  /// Save the JSON only. The window uses this when a core owns the credential:
  /// the password must never be written to the user's config directory.
  pub fn save_prefs(&self) -> Result<(), String> {
    let p = path();
    if let Some(parent) = p.parent() {
      std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    if let Err(first) = std::fs::write(&p, &body) {
      // Same recovery as on load: an unwritable file (a stale DACL) is
      // recreated instead of blocking every save.
      let _ = std::fs::remove_file(&p);
      std::fs::write(&p, &body)
        .map_err(|e| format!("{}: {e} (first attempt: {first})", p.display()))?;
    }
    Ok(())
  }

  pub fn save_to(&self, dir: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path_in(dir).parent() {
      std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if self.remember_password {
      if !self.password.is_empty() {
        secret::save_in(dir, &self.username, &self.password)?;
      }
    } else {
      // The user unticked "remember password": drop the stored entry.
      secret::delete_in(dir);
    }
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    let path = path_in(dir);
    if let Err(first) = std::fs::write(&path, &body) {
      // A `config.json` written by an older install can carry an empty DACL:
      // unreadable, and unwritable even for the service. Drop and recreate it
      // rather than failing every save for the rest of the installation.
      let _ = std::fs::remove_file(&path);
      std::fs::write(&path, &body)
        .map_err(|e| format!("{}: {e} (first attempt: {first})", path.display()))?;
    }
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_location_uses_platform_convention() {
    let dir = dir();
    assert_eq!(dir.file_name().unwrap(), "thunet");
    assert_eq!(path().file_name().unwrap(), "config.json");

    let normalized = dir.to_string_lossy().replace('\\', "/");
    if cfg!(target_os = "macos") {
      assert!(normalized.ends_with("Library/Application Support/thunet"));
    } else {
      assert!(normalized.ends_with("/thunet"));
    }
  }

  #[test]
  fn defaults_do_not_force_minimizing() {
    let cfg = Config::default();
    assert!(!cfg.start_minimized, "start-minimized is opt-in");
    assert!(cfg.password.is_empty());
    assert_eq!(cfg.log_level, "info", "debug output is opt-in");
  }
}
