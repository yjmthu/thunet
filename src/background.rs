//! How much of the app stays behind on Android.
//!
//! The three levels mirror the desktop's autostart modes: nothing, the core
//! alone, and the core plus the always-present entry point (there it is the
//! tray, here it is the keep-alive service and its notification). "Keep online"
//! is a different setting and keeps its desktop meaning: it decides whether the
//! *core* signs back in, not whether anything survives the app.
//!
//! `ThunetActivity` owns the service, the notification permission and the
//! wake-up registration; this module is the window's side of that. The level
//! travels through `files/background.txt`, the same file the boot receiver and
//! the notification's stop action write, so there is one answer instead of a
//! copy in `config.json` that could disagree with it.

/// What stays behind when the window is gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
  /// Nothing: no service, no wake-up registration, no periodic job. Opening the
  /// app is the only way back.
  #[default]
  Off,
  /// The live network callback while the process lives, the one-shot wake-up
  /// when Wi-Fi appears, and the 15-minute backstop. No notification.
  Wake,
  /// Everything above, plus a foreground service with a silent notification
  /// and a start at boot. This is what survives a swipe from recents.
  Service,
}

impl Mode {
  /// Read the current level, as written by the Activity.
  pub fn read() -> Self {
    let path = crate::config::dir().join("background.txt");
    match std::fs::read_to_string(path) {
      Ok(text) => match text.trim() {
        "wake" => Self::Wake,
        "service" => Self::Service,
        _ => Self::Off,
      },
      // Before the Activity ever wrote the file: nothing was ever enabled.
      Err(_) => Self::Off,
    }
  }

  /// The word the Activity uses.
  pub fn wire(self) -> &'static str {
    match self {
      Self::Off => "off",
      Self::Wake => "wake",
      Self::Service => "service",
    }
  }
}

/// Did the last request for [`Mode::Service`] lose the notification permission?
///
/// The effective level is then [`Mode::Wake`], and the row explains why instead
/// of pretending the service is running.
pub fn denied() -> bool {
  crate::config::dir().join("background.denied").exists()
}

/// Is the app exempt from battery optimisation?
///
/// This is not a cosmetic setting: while the exemption is missing, Android (and
/// MIUI in particular) blocks the app's network whenever it is in the
/// background - DNS included - so a wake-up cannot even sign in. The answer is
/// only known to the system, so it is asked for on every settings tick.
pub fn power_ok() -> bool {
  crate::android_jni::call_bool_get("backgroundPower").unwrap_or(false)
}

/// Has the user confirmed the OEM toggles the app cannot read?
///
/// MIUI's autostart and power policy are stored by the security centre and have
/// no public API, and its "unrestricted" policy is *separate* from the AOSP
/// battery whitelist (verified on a phone: removing the whitelist entry leaves
/// MIUI's policy untouched). Any state we displayed on our own would therefore
/// be a guess, so the row asks the user and keeps their answer.
pub fn allowed() -> bool {
  crate::config::dir().join("background.allowed").exists()
}

/// Record the user's answer for [`allowed`].
pub fn set_allowed(on: bool) -> Result<(), String> {
  crate::android_jni::call_bool("setBackgroundAllowed", on)
}

/// Ask the Activity for the exemption, then show the OEM page.
///
/// What comes back is not readable as a state, so nothing is recorded: the row
/// keeps only the user's own confirmation ([`set_allowed`]).
pub fn request_power() -> Result<(), String> {
  crate::android_jni::call_str("requestBackground", "")
}

/// Ask the Activity for a new level.
///
/// Choosing [`Mode::Service`] can open the notification permission dialog, so
/// the Activity decides the outcome and writes it; the caller picks that up with
/// [`Mode::read`] on its next tick instead of assuming the request worked.
pub fn set(mode: Mode) -> Result<(), String> {
  crate::android_jni::call_str("setMode", mode.wire())
}
