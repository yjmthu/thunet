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
/// `THUNET_STATE_DIR` overrides all of them. The Windows service needs it: it
/// runs under SYSTEM, whose `%APPDATA%` is the system profile, while the
/// settings and the sealed credential belong to the user who installed it.
pub fn dir() -> PathBuf {
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

pub fn path() -> PathBuf {
  dir().join("config.json")
}

impl Config {
  pub fn load() -> Self {
    let mut cfg: Config = std::fs::read_to_string(path())
      .ok()
      .and_then(|s| serde_json::from_str(&s).ok())
      .unwrap_or_default();
    if cfg.remember_password {
      match secret::load() {
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
          secret::delete();
        }
      }
    }
    cfg
  }

  pub fn save(&self) -> Result<(), String> {
    let p = path();
    if let Some(parent) = p.parent() {
      std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if self.remember_password {
      if !self.password.is_empty() {
        secret::save(&self.username, &self.password)?;
      }
    } else {
      // The user unticked "remember password": drop the stored entry.
      secret::delete();
    }
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    std::fs::write(&p, body).map_err(|e| e.to_string())
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
