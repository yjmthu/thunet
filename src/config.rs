//! Persistent settings, split by owner.
//!
//! Two files, because two processes write them:
//!
//! | file | owner | contents |
//! | --- | --- | --- |
//! | `config.json` | the window | language, log level, theme, privacy, start minimised |
//! | `core.json` | the core | account, portal host, session policy |
//!
//! A machine core runs as another account (a systemd `DynamicUser`, the
//! `NT SERVICE\thunet` service account), so it can never write the user's file
//! and the user can never write its machine directory. One owner per setting is
//! what keeps the two halves from disagreeing; the window reads the core's side
//! through `CoreInfo`/`CredentialState` instead of keeping a copy.
//!
//! The password never appears in either JSON: `password` is skipped by serde and
//! the pair is sealed into `credential.bin` (see [`crate::secret`]), next to the
//! core's file, and only when the user ticked "remember password".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::secret;

/// Settings the window owns: they only change how it renders and starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
  /// Whether the autostart entry starts the app hidden. Manual launches (Start
  /// Menu, desktop shortcut, installer) open the window regardless.
  pub start_minimized: bool,
  /// "auto" (follow OS) | "zh" | "en"
  pub lang: String,
  /// Minimum level shown in the window log: "debug" | "info" | "warn" | "error".
  /// Defaults to `info`, so only diagnostics the user asked for show up.
  pub log_level: String,
  /// Window theme: "auto" (follow the system) | "light" | "dark".
  pub theme: String,
  /// Whether the account and the addresses are masked on screen. Off by
  /// default: the app is for the machine it runs on, and a configuration
  /// written before this field existed has never been masked.
  #[serde(default)]
  pub privacy: bool,
}

/// Settings the core owns: it acts on them and it is the only half that may
/// write them down (the machine directory belongs to the service account).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
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
  /// session that drops later is handled by [`CoreConfig::keep_online`].
  pub auto_connect: bool,
  /// Stay connected once a session exists: sign back in after the session is
  /// lost behind the user's back, until the user disconnects. `false` leaves a
  /// dropped session dropped. Defaults to true so a configuration written
  /// before this field existed keeps its behaviour.
  #[serde(default = "default_true")]
  pub keep_online: bool,
  pub reconnect_delay_secs: u64,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      start_minimized: false,
      lang: "auto".into(),
      log_level: "info".into(),
      theme: "auto".into(),
      privacy: false,
    }
  }
}

impl Default for CoreConfig {
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
      reconnect_delay_secs: 5,
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
  } else if cfg!(target_os = "android") {
    // Android has no user profile. The app injects its private directory
    // through `THUNET_STATE_DIR`; this path is only reached before that
    // happens, and `temp_dir` is writable inside the sandbox.
    std::env::temp_dir()
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

/// The window's file.
pub fn path() -> PathBuf {
  dir().join("config.json")
}

pub fn path_in(dir: &std::path::Path) -> PathBuf {
  dir.join("config.json")
}

/// The core's file.
pub fn core_path_in(dir: &std::path::Path) -> PathBuf {
  dir.join("core.json")
}

/// Split the pre-split single file into the two owners, once.
///
/// Called for the state directory (window and core share it in portable mode)
/// before either half reads it, and for the machine directory by
/// [`CoreConfig::load_machine`]. Idempotent: once `core.json` exists it does
/// nothing.
pub fn migrate() {
  split_legacy(&dir(), true);
}

pub fn migrate_machine() {
  split_legacy(&machine_dir(), false);
}

fn split_legacy(dir: &std::path::Path, keep_user: bool) {
  let core_file = core_path_in(dir);
  if core_file.exists() {
    return;
  }
  let legacy = path_in(dir);
  let Ok(text) = std::fs::read_to_string(&legacy) else {
    return;
  };
  let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
    return;
  };
  // Both halves have serde defaults, so a partial file is enough. A value that
  // does not fit the shape is left alone rather than overwritten with defaults:
  // a typo must not cost the settings it was meant to carry.
  let Ok(core) = serde_json::from_value::<CoreConfig>(value.clone()) else {
    crate::diag::line("config: cannot split the old file, leaving it alone");
    return;
  };
  let Ok(core_body) = serde_json::to_string_pretty(&core) else {
    return;
  };
  if let Err(e) = write_file(&core_file, &core_body, 0o600) {
    crate::diag::line(&format!(
      "config: cannot write {}: {e}",
      core_file.display()
    ));
    return;
  }
  if keep_user {
    // The same directory serves the window: keep its half where it was.
    let Ok(user) = serde_json::from_value::<Config>(value) else {
      return;
    };
    if let Ok(body) = serde_json::to_string_pretty(&user) {
      let _ = write_file(&legacy, &body, 0o600);
    }
  } else {
    // A machine directory has no window of its own; the old file is the core's
    // and has just been rewritten as `core.json`.
    let _ = std::fs::remove_file(&legacy);
  }
  crate::diag::line(&format!(
    "config: split {} into {}",
    legacy.display(),
    core_file.display()
  ));
}

impl Config {
  /// Load the window's settings.
  pub fn load() -> Self {
    Self::load_from(&user_dir())
  }

  /// The window's half ignores the core's keys, so an old file (or a copy that
  /// still carries them) loads without a migration step.
  pub fn load_from(dir: &std::path::Path) -> Self {
    match std::fs::read_to_string(path_in(dir)) {
      Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
      // A file this account cannot read *right now* - an upgrade rewriting its
      // ACL, or the installer replacing it - must not be deleted: that turned
      // a transient permission error into permanent data loss. `save_to`
      // already replaces a file it cannot write, so nothing here needs to.
      Err(_) => Config::default(),
    }
  }

  pub fn save(&self) -> Result<(), String> {
    self.save_to(&dir())
  }

  pub fn save_to(&self, dir: &std::path::Path) -> Result<(), String> {
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    write_file(&path_in(dir), &body, 0o600)
  }
}

impl CoreConfig {
  /// Load the core's settings from this process's state directory.
  pub fn load() -> Self {
    Self::load_from(&dir())
  }

  /// Load the machine-wide settings: the service account's directory.
  pub fn load_machine() -> Self {
    migrate_machine();
    Self::read(&machine_dir())
  }

  pub fn load_from(dir: &std::path::Path) -> Self {
    split_legacy(dir, true);
    Self::read(dir)
  }

  fn read(dir: &std::path::Path) -> Self {
    let mut cfg: CoreConfig = match std::fs::read_to_string(core_path_in(dir)) {
      Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
      Err(_) => CoreConfig::default(),
    };
    // The sealed blob is the authoritative record of the account: it survives a
    // lost or unreadable `core.json`, and it is bound to this machine, so it
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

  /// Write the JSON into `dir`, leaving the sealed credential alone.
  ///
  /// Preference changes (auto connect, keep online) use this: they never alter
  /// `remember_password`, so re-sealing the blob on every checkbox click would
  /// only add a way to fail.
  pub fn save_prefs_to(&self, dir: &std::path::Path) -> Result<(), String> {
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    write_file(&core_path_in(dir), &body, 0o600)
  }

  pub fn save_to(&self, dir: &std::path::Path) -> Result<(), String> {
    if self.remember_password {
      if !self.password.is_empty() {
        secret::save_in(dir, &self.username, &self.password)?;
      }
    } else {
      // The user unticked "remember password": drop the stored entry.
      secret::delete_in(dir);
    }
    let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
    write_file(&core_path_in(dir), &body, 0o600)
  }
}

/// Write `body` to `path`, recreating the file when a stale ACL blocks it.
///
/// A `config.json` whose ACL went wrong (an interrupted install, a DACL repair
/// in between) can be unreadable and unwritable even for the account that owns
/// it. Drop and recreate rather than failing every save for the rest of the
/// installation.
fn write_file(path: &std::path::Path, body: &str, mode: u32) -> Result<(), String> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
  }
  if let Err(first) = std::fs::write(path, body) {
    let _ = std::fs::remove_file(path);
    std::fs::write(path, body)
      .map_err(|e| format!("{}: {e} (first attempt: {first})", path.display()))?;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
  }
  #[cfg(not(unix))]
  let _ = mode;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_location_uses_platform_convention() {
    let dir = dir();
    assert_eq!(dir.file_name().unwrap(), "thunet");
    assert_eq!(path().file_name().unwrap(), "config.json");
    assert_eq!(core_path_in(&dir).file_name().unwrap(), "core.json");

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
    assert_eq!(cfg.log_level, "info", "debug output is opt-in");
    assert!(cfg.lang == "auto" && cfg.theme == "auto");

    let core = CoreConfig::default();
    assert!(core.keep_online, "a client that drops silently is useless");
    assert!(core.password.is_empty());
    assert!(core.username.is_empty());
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
    let core: CoreConfig = serde_json::from_str(old).expect("an old core config still loads");
    assert!(!core.auto_connect, "the stored value is kept");
    assert!(core.keep_online, "absent means the previous behaviour");
    assert_eq!(core.username, "alice");
    // The window's half ignores the keys it does not own.
    let user: Config = serde_json::from_str(old).expect("an old user config still loads");
    assert_eq!(user.log_level, "info");
    assert!(!user.start_minimized);
  }

  /// The pre-split file carried both owners. The core takes its half, the
  /// window keeps the rest, and a second load finds the split already done.
  #[test]
  fn the_old_single_file_is_split_into_two_owners() {
    let dir = std::env::temp_dir().join(format!("thunet-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(
      path_in(&dir),
      r#"{"username":"alice","remember_password":false,
          "auto_connect":false,"keep_online":true,"host":"auth4.tsinghua.edu.cn",
          "lang":"zh","log_level":"debug","theme":"dark","start_minimized":true}"#,
    )
    .expect("legacy file");

    let core = CoreConfig::load_from(&dir);
    assert_eq!(core.username, "alice");
    assert!(!core.auto_connect);
    assert!(core.keep_online);

    let user_text = std::fs::read_to_string(path_in(&dir)).expect("user file");
    assert!(
      !user_text.contains("username"),
      "the core's account stays out of the window's file: {user_text}"
    );
    let user: Config = serde_json::from_str(&user_text).expect("user half");
    assert_eq!(user.lang, "zh");
    assert_eq!(user.log_level, "debug");
    assert!(user.start_minimized);

    let core_text = std::fs::read_to_string(core_path_in(&dir)).expect("core file");
    assert!(core_text.contains("\"username\": \"alice\""));
    assert!(
      !core_text.contains("log_level"),
      "the window's half stays out"
    );
    assert!(
      !core_text.contains("\"password\""),
      "the password is never written"
    );

    // Idempotent: the second load does not rewrite anything, so a later edit of
    // either file survives.
    std::fs::write(path_in(&dir), r#"{"lang":"en"}"#).expect("user edit");
    let again = CoreConfig::load_from(&dir);
    assert_eq!(again.username, "alice");
    assert_eq!(
      std::fs::read_to_string(path_in(&dir)).unwrap(),
      r#"{"lang":"en"}"#
    );

    let _ = std::fs::remove_dir_all(&dir);
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
