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
  /// Sign in by itself right after the core starts. It decides the initial
  /// value of the core's runtime intent (`want_online`), and nothing else: a
  /// session that drops later is handled by [`Config::keep_online`].
  pub auto_connect: bool,
  /// Stay connected once a session exists: sign back in after the session is
  /// lost behind the user's back, until the user disconnects. `false` leaves a
  /// dropped session dropped. Defaults to true so a configuration written
  /// before this field existed keeps its behaviour.
  #[serde(default = "default_true")]
  pub keep_online: bool,
  /// Whether the autostart entry starts the app hidden. Manual launches (Start
  /// Menu, desktop shortcut, installer) open the window regardless.
  pub start_minimized: bool,
  pub reconnect_delay_secs: u64,
  /// "auto" (follow OS) | "zh" | "en"
  pub lang: String,
  /// Minimum level shown in the window log: "debug" | "info" | "warn" | "error".
  /// Defaults to `info`, so only diagnostics the user asked for show up.
  pub log_level: String,
  /// Window theme: "auto" (follow the system) | "light" | "dark".
  pub theme: String,
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
      keep_online: true,
      start_minimized: false,
      reconnect_delay_secs: 5,
      lang: "auto".into(),
      log_level: "info".into(),
      theme: "auto".into(),
    }
  }
}

/// Serde default for a flag that means "do this unless told otherwise".
fn default_true() -> bool {
  true
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

/// A short, stable tag for a state directory.
///
/// Portable mode is keyed by *(user, state directory)*, not by user alone: the
/// control endpoint and the single-instance guard both carry this tag, so two
/// portable setups of the same user never share a core, and a window can never
/// silently talk to the core of another setup.
///
/// FNV-1a over the lexically absolute path: no filesystem access, so both
/// halves compute the same tag whether or not the directory exists yet, and the
/// value does not change between builds. Eight hex digits are plenty - a
/// collision would need two state directories of one user to hash together.
pub fn state_tag_for(dir: &std::path::Path) -> String {
  let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  for byte in dir.to_string_lossy().as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }
  format!("{:08x}", hash >> 32)
}

/// The tag for the state directory this process uses.
pub fn state_tag() -> String {
  state_tag_for(&state_dir())
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
      // A file this account cannot read *right now* - an upgrade rewriting its
      // ACL, or the installer replacing it - must not be deleted: that turned
      // a transient permission error into permanent data loss. `save_to`
      // already replaces a file it cannot write, so nothing here needs to.
      Err(_) => Config::default(),
    };
    // The sealed blob is the authoritative record of the account: it survives a
    // lost or unreadable `config.json`, and it is bound to this machine, so it
    // cannot have been written by somebody else.
    match secret::load_in(dir) {
      Some((user, pass)) => {
        if cfg.username.is_empty() {
          crate::diag::line("config: recovered the account from credential.bin");
          cfg.username = user;
        }
        cfg.password = pass;
        cfg.remember_password = true;
      }
      // Nothing usable is stored: a blob sealed by another machine, a key this
      // account cannot read, or a cleared entry. Drop what is there — an
      // unreadable blob helps nobody — and fall back to typing the password.
      None => {
        if secret::stored(dir) {
          crate::diag::line(
            "config: credential.bin is present but cannot be opened (missing key or ACL)",
          );
        }
        if cfg.remember_password {
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
    self.save_prefs_to(&dir())
  }

  /// Write the JSON into `dir`, leaving the sealed credential alone.
  ///
  /// Preference changes (auto connect, keep online) use this: they never alter
  /// `remember_password`, so re-sealing the blob on every checkbox click would
  /// only add a way to fail.
  pub fn save_prefs_to(&self, dir: &std::path::Path) -> Result<(), String> {
    let p = path_in(dir);
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
      // A `config.json` whose ACL went wrong (an interrupted install, a DACL
      // repair in between) can be unreadable and unwritable even for the
      // service. Drop and recreate it rather than failing every save for the
      // rest of the installation.
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
    assert!(cfg.keep_online, "a client that drops silently is useless");
    assert!(cfg.password.is_empty());
    assert_eq!(cfg.log_level, "info", "debug output is opt-in");
  }

  /// A configuration written before `keep_online` existed has to keep the old
  /// behaviour (the client stayed online), and the two removed keepalive keys
  /// must not stop it from loading.
  #[test]
  fn an_old_config_keeps_online_by_default() {
    let old = r#"{
      "username": "alice", "host": "auth4.tsinghua.edu.cn", "https": true,
      "ac_id": "auto", "ip": "", "auto_connect": false,
      "start_minimized": false, "reconnect_delay_secs": 5,
      "keepalive_target": "https://example.test/", "keepalive_interval_secs": 300,
      "lang": "auto", "log_level": "info", "theme": "auto"
    }"#;
    let cfg: Config = serde_json::from_str(old).expect("an old config still loads");
    assert!(!cfg.auto_connect, "the stored value is kept");
    assert!(cfg.keep_online, "absent means the previous behaviour");
  }

  /// The endpoint and the single-instance guard are keyed by this tag, so it
  /// has to be short, deterministic, and different for every state directory.
  #[test]
  fn every_state_directory_gets_its_own_tag() {
    let one = state_tag_for(std::path::Path::new("/tmp/p-one"));
    let two = state_tag_for(std::path::Path::new("/tmp/p-two"));
    assert_eq!(one.len(), 8, "the tag has to stay a short name: {one}");
    assert_ne!(one, two, "two setups must never share an endpoint");
    // The same directory has to hash the same every time - the core and the
    // window compute it independently.
    assert_eq!(one, state_tag_for(std::path::Path::new("/tmp/p-one")));
  }
}
