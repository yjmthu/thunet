//! The desktop's own light/dark preference.
//!
//! winit reports a system theme on Windows and macOS only: on Linux
//! `ActiveEventLoop::system_theme()` returns `None` and no `ThemeChanged` is
//! ever emitted, so `egui::ThemePreference::System` can only fall back to dark
//! there. The preference is published by the desktop through the XDG settings
//! portal (`org.freedesktop.portal.Settings`, key
//! `org.freedesktop.appearance` / `color-scheme`), the interface GNOME, KDE and
//! the other desktops implement, so that is what is read here.

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU8, Ordering};

/// `color-scheme` values from the portal, plus this module's own "not known"
/// state (also what it stays at when there is no portal to ask).
#[cfg(target_os = "linux")]
const UNKNOWN: u8 = 0;
#[cfg(target_os = "linux")]
const DARK: u8 = 1;
#[cfg(target_os = "linux")]
const LIGHT: u8 = 2;

/// The last value the portal reported.
#[cfg(target_os = "linux")]
static SCHEME: AtomicU8 = AtomicU8::new(UNKNOWN);

/// The session bus, held for the life of the process: the `SettingChanged`
/// subscription lives on the connection.
#[cfg(target_os = "linux")]
static BUS: std::sync::OnceLock<gtk::gio::DBusConnection> = std::sync::OnceLock::new();

/// The desktop's preference, once it has been read.
///
/// `None` means "nothing known (yet)", and `egui` then keeps its own `System`
/// handling: on Windows and macOS the back end reports the theme itself, and a
/// Linux desktop without a portal has no preference to express.
#[cfg(target_os = "linux")]
pub fn system() -> Option<eframe::egui::Theme> {
  match SCHEME.load(Ordering::Relaxed) {
    DARK => Some(eframe::egui::Theme::Dark),
    LIGHT => Some(eframe::egui::Theme::Light),
    _ => None,
  }
}

#[cfg(not(target_os = "linux"))]
pub fn system() -> Option<eframe::egui::Theme> {
  None
}

/// Read the desktop's preference now and follow it for the rest of the run.
///
/// The portal emits `SettingChanged` on the session bus; GLib delivers it in
/// the thread-default context of the subscribing thread, which is the one
/// `App::logic` pumps every frame (`gtk::main_iteration_do`), so no thread of
/// its own is needed. `ctx` is woken from the callback so the new colours show
/// up on the next frame instead of on the next tick.
#[cfg(target_os = "linux")]
pub fn watch(ctx: eframe::egui::Context) {
  use gtk::gio;
  use gtk::glib;
  use gtk::glib::ToVariant as _;

  const PORTAL: &str = "org.freedesktop.portal.Desktop";
  const PATH: &str = "/org/freedesktop/portal/desktop";
  const IFACE: &str = "org.freedesktop.portal.Settings";
  const NAMESPACE: &str = "org.freedesktop.appearance";
  const KEY: &str = "color-scheme";
  /// A portal that is not answering must not hold up the first frame.
  const TIMEOUT_MS: i32 = 1_000;

  let publish = |value: Option<u32>| {
    let scheme = match value {
      Some(1) => DARK,
      Some(2) => LIGHT,
      _ => UNKNOWN,
    };
    SCHEME.store(scheme, Ordering::Relaxed);
  };

  // No session bus means no desktop preference to follow; `auto` then stays on
  // egui's fallback.
  let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
    return;
  };
  let bus = BUS.get_or_init(|| conn);

  // Subscribed before the initial read, so a change published in between is
  // queued rather than missed; the two can then only apply in the order the
  // portal sent them.
  let _ = bus.signal_subscribe(
    Some(PORTAL),
    Some(IFACE),
    Some("SettingChanged"),
    Some(PATH),
    None,
    gio::DBusSignalFlags::NONE,
    move |_, _, _, _, _, params: &glib::Variant| {
      let Some((namespace, key, value)) = params.get::<(String, String, glib::Variant)>() else {
        return;
      };
      if namespace == NAMESPACE && key == KEY {
        publish(value.get::<u32>());
        ctx.request_repaint();
      }
    },
  );

  // `ReadOne` answers `(v)`, and the portal's value is a `u32`.
  let reply = bus.call_sync(
    Some(PORTAL),
    PATH,
    IFACE,
    "ReadOne",
    Some(&(NAMESPACE, KEY).to_variant()),
    None,
    gio::DBusCallFlags::NONE,
    TIMEOUT_MS,
    gio::Cancellable::NONE,
  );
  publish(
    reply
      .ok()
      .and_then(|r| r.get::<(glib::Variant,)>())
      .and_then(|(v,)| v.get::<u32>()),
  );
}

#[cfg(not(target_os = "linux"))]
pub fn watch(_ctx: eframe::egui::Context) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use gtk::glib::{ToVariant as _, Variant};

  /// `SettingChanged` carries `(ssv)`, and `color-scheme` is the `u32` inside
  /// the trailing variant. The callback reads it exactly like this.
  #[test]
  fn setting_changed_body_yields_the_scheme() {
    let body = Variant::tuple_from_iter([
      "org.freedesktop.appearance".to_variant(),
      "color-scheme".to_variant(),
      Variant::from_variant(&1u32.to_variant()),
    ]);
    let (namespace, key, value) = body
      .get::<(String, String, Variant)>()
      .expect("(ssv) deserializes");
    assert_eq!(namespace, "org.freedesktop.appearance");
    assert_eq!(key, "color-scheme");
    assert_eq!(value.get::<u32>(), Some(1));
  }
}
