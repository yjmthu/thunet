//! Desktop shortcut creation.
//!
//! THUnet is installed by a package (`.msi` / `.deb`), so the application itself
//! never copies files or writes uninstall entries. The only self-service action
//! left is putting a launcher on the desktop.

pub const APP_TITLE: &str = "THUnet";

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::create_desktop_shortcut;
#[cfg(windows)]
pub use windows::create_desktop_shortcut;
#[cfg(windows)]
pub use windows::remove_legacy_start_menu;

#[cfg(not(windows))]
pub fn remove_legacy_start_menu() -> Result<bool, String> {
  Ok(false)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn create_desktop_shortcut() -> Result<(), String> {
  Err("desktop shortcuts are only implemented for Windows and Linux".into())
}
