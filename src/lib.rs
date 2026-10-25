//! Shared library: the portal client, the session core, the window and the
//! desktop CLI. The desktop binary is a thin shim over [`desktop::main`];
//! Android will add its own entry point.

#[cfg(target_os = "android")]
mod android_jni;
mod autostart;
#[cfg(target_os = "android")]
mod background;
mod clipboard;
mod config;
mod core;
mod crypto;
mod diag;
mod elevate;
mod i18n;
mod icons;
mod ident;
mod ipc;
mod logline;
mod prompt;
mod proto;
mod secret;
#[cfg(target_os = "android")]
mod secure_wifi;
mod session;
mod shortcut;
mod single;
mod srun;
mod theme;
mod tray;
mod ui;
mod wifi;
#[cfg(windows)]
mod winsvc;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
pub mod desktop;

/// Path to the executable on disk.
///
/// After an in-place upgrade the running image is unlinked, and on Linux
/// `/proc/self/exe` then reads `"/usr/bin/thunet (deleted)"`. Spawning or
/// registering that path fails, so the suffix is stripped and the current file
/// on disk is used instead.
pub fn exe_path() -> Option<std::path::PathBuf> {
  Some(without_deleted_suffix(std::env::current_exe().ok()?))
}

/// `/proc/self/exe` reads `"/usr/bin/thunet (deleted)"` once the file was
/// replaced in place (an upgrade); the path on disk is what we want.
#[cfg(target_os = "linux")]
fn without_deleted_suffix(exe: std::path::PathBuf) -> std::path::PathBuf {
  match exe.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
    Some(path) => std::path::PathBuf::from(path),
    None => exe,
  }
}

#[cfg(not(target_os = "linux"))]
fn without_deleted_suffix(exe: std::path::PathBuf) -> std::path::PathBuf {
  exe
}

#[cfg(test)]
mod tests {
  use super::without_deleted_suffix;

  #[test]
  fn an_unlinked_image_resolves_to_the_path_on_disk() {
    // Right after an in-place upgrade `/proc/self/exe` keeps the marker.
    #[cfg(target_os = "linux")]
    assert_eq!(
      without_deleted_suffix(std::path::PathBuf::from("/usr/bin/thunet (deleted)")),
      std::path::PathBuf::from("/usr/bin/thunet")
    );
    assert_eq!(
      without_deleted_suffix(std::path::PathBuf::from("/usr/bin/thunet")),
      std::path::PathBuf::from("/usr/bin/thunet")
    );
  }
}
