//! Optional SSID detection on Android.
//!
//! Reading the Wi-Fi name needs the location permission, so this is a setting
//! rather than something the app does on its own. The Activity asks for the
//! permission when the row is ticked and writes the current name to
//! `files/ssid.txt` while the setting is on and the permission is held; without
//! it the file stays empty and `wifi::classify()` falls back to the network
//! probes, which is the behaviour the app had before this setting existed.

/// Is the user's SSID detection on?
pub fn enabled() -> bool {
  let path = crate::config::dir().join("secure.txt");
  std::fs::read_to_string(path)
    .map(|text| text.trim() == "on")
    .unwrap_or(false)
}

/// Ask the Activity for a new state.
///
/// Enabling can show the location permission dialog, so the answer is the file,
/// not this call: the window re-reads it on its next tick.
pub fn set(on: bool) -> Result<(), String> {
  crate::android_jni::call_bool("setSecureDetection", on)
}
