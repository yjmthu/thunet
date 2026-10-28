//! Tray icon façade.
//!
//! The desktop builds a real tray icon; Android has no tray, so the same API
//! returns an inert object. Keeping the type here means the window code never
//! needs a `cfg` for the tray itself.

/// What a tray menu item or an icon click asks the window to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
  Connect,
  Disconnect,
  Show,
  Hide,
  Quit,
}

#[cfg(not(target_os = "android"))]
mod imp {
  use eframe::egui;
  use tray_icon::TrayIconBuilder;
  use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};

  use super::Action;
  use crate::i18n::Texts;
  use crate::icons::make_icon;

  pub struct Tray {
    icon: Option<tray_icon::TrayIcon>,
    connect: MenuItem,
    disconnect: MenuItem,
    show: MenuItem,
    hide: MenuItem,
    quit: MenuItem,
  }

  impl Tray {
    pub fn new(title: &str, texts: &Texts, fill: [u8; 3]) -> Self {
      let connect = MenuItem::with_id("connect", texts.connect, true, None);
      let disconnect = MenuItem::with_id("disconnect", texts.disconnect, true, None);
      let show = MenuItem::with_id("show", texts.show_window, true, None);
      let hide = MenuItem::with_id("hide", texts.hide_window, true, None);
      let quit = MenuItem::with_id("quit", texts.quit, true, None);
      let menu = Menu::new();
      let _ = menu.append_items(&[&connect, &disconnect]);
      let _ = menu.append(&PredefinedMenuItem::separator());
      let _ = menu.append_items(&[&show, &hide]);
      let _ = menu.append(&PredefinedMenuItem::separator());
      let _ = menu.append(&quit);
      let icon = TrayIconBuilder::new()
        // The menu lives on the right button; a left click brings the window
        // back.
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(title)
        .with_icon(make_icon(fill))
        .build()
        .ok();
      Self {
        icon,
        connect,
        disconnect,
        show,
        hide,
        quit,
      }
    }

    /// `false` when the platform refused to create an icon (no notification
    /// area, e.g. inside a validation sandbox). The window then stays
    /// window-only: closing quits instead of hiding, and a hidden start is not
    /// attempted.
    pub fn available(&self) -> bool {
      self.icon.is_some()
    }

    pub fn set_texts(&self, t: &Texts) {
      self.connect.set_text(t.connect);
      self.disconnect.set_text(t.disconnect);
      self.show.set_text(t.show_window);
      self.hide.set_text(t.hide_window);
      self.quit.set_text(t.quit);
    }

    pub fn set_enabled(&self, connect: bool, disconnect: bool) {
      self.connect.set_enabled(connect);
      self.disconnect.set_enabled(disconnect);
    }

    /// Recolour the icon, reporting whether the shell took it.
    ///
    /// `set_icon` rewrites a temp PNG and re-announces the indicator, so the
    /// caller only calls this when the wanted colour differs from the one the
    /// shell accepted.
    ///
    /// The notification area is rebuilt on resume and after an explorer
    /// restart, and a `NIM_MODIFY` that lands in that window fails. The caller
    /// retries on the next tick while this returns an error, so the colour
    /// cannot get stuck on an old bitmap. No icon at all is not an error: the
    /// platform refused to create one and there is nothing to recolour.
    pub fn set_color(&self, fill: [u8; 3]) -> Result<(), String> {
      match &self.icon {
        None => Ok(()),
        Some(icon) => icon
          .set_icon(Some(make_icon(fill)))
          .map_err(|e| e.to_string()),
      }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_tooltip(&self, tip: &str) {
      if let Some(icon) = &self.icon {
        let _ = icon.set_tooltip(Some(tip));
      }
    }

    /// One pending action, if any. Clicks are resolved before menu items.
    pub fn poll(&self) -> Option<Action> {
      if let Some(show) = click::take() {
        return Some(if show { Action::Show } else { Action::Hide });
      }
      while let Ok(ev) = MenuEvent::receiver().try_recv() {
        return match ev.id().0.as_str() {
          "connect" => Some(Action::Connect),
          "disconnect" => Some(Action::Disconnect),
          "quit" => Some(Action::Quit),
          "show" => Some(Action::Show),
          "hide" => Some(Action::Hide),
          _ => continue,
        };
      }
      None
    }
  }

  /// A second left click inside this window counts as a double click. 300 ms is
  /// snappier than the 500 ms most desktops use; the delay is a compromise,
  /// since a single click can only be acted on once no second click can follow.
  #[cfg_attr(not(windows), allow(dead_code))]
  const DOUBLE_CLICK_MS: u64 = 300;

  /// Does the click at `now` follow `previous` closely enough to be the second
  /// half of a double click? `previous == 0` means there was no click.
  #[cfg_attr(not(windows), allow(dead_code))]
  fn is_double_click(previous: u64, now: u64, deadline: u64) -> bool {
    previous != 0 && now.saturating_sub(previous) <= deadline
  }

  /// What a pending click burst means: `None` waits for the deadline to pass,
  /// `Some(true)` shows the window, `Some(false)` hides it.
  #[cfg_attr(not(windows), allow(dead_code))]
  fn click_outcome(clicks: u8, since_last: u64, deadline: u64) -> Option<bool> {
    match clicks {
      2 => Some(false),
      1 if since_last >= deadline => Some(true),
      _ => None,
    }
  }

  pub fn install_events(ctx: &egui::Context) {
    click::install(ctx);
  }

  /// Windows only: the Linux backend never emits a `TrayIconEvent`.
  #[cfg(windows)]
  mod click {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use eframe::egui;
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

    use super::{DOUBLE_CLICK_MS, click_outcome, is_double_click};

    /// 0 is the "no click yet" sentinel, so the clock starts at 1.
    fn clock() -> &'static Instant {
      static STARTED: OnceLock<Instant> = OnceLock::new();
      STARTED.get_or_init(Instant::now)
    }

    static CLICKS: AtomicU8 = AtomicU8::new(0);
    static LAST_CLICK: AtomicU64 = AtomicU64::new(0);

    fn now_ms() -> u64 {
      clock().elapsed().as_millis() as u64 + 1
    }

    pub fn install(ctx: &egui::Context) {
      let ctx = ctx.clone();
      TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if !matches!(
          event,
          TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
          }
        ) {
          return;
        }
        // `TrayIconEvent::DoubleClick` never arrives: tray-icon registers its
        // window class without `CS_DBLCLKS`, so Windows never posts
        // `WM_LBUTTONDBLCLK`. Count the clicks here instead.
        let now = now_ms();
        let previous = LAST_CLICK.swap(now, Ordering::Relaxed);
        let double = is_double_click(previous, now, DOUBLE_CLICK_MS);
        CLICKS.store(if double { 2 } else { 1 }, Ordering::Relaxed);
        // A single click is only acted on once the deadline has passed: acting
        // at once would show the window on the first click of a double click
        // and hide it again on the second, which looks like a flash.
        ctx.request_repaint_after(Duration::from_millis(DOUBLE_CLICK_MS));
        ctx.request_repaint();
      }));
    }

    pub fn take() -> Option<bool> {
      let since_last = now_ms().saturating_sub(LAST_CLICK.load(Ordering::Relaxed));
      let outcome = click_outcome(CLICKS.load(Ordering::Relaxed), since_last, DOUBLE_CLICK_MS);
      if outcome.is_some() {
        CLICKS.store(0, Ordering::Relaxed);
      }
      outcome
    }
  }

  #[cfg(not(windows))]
  mod click {
    use eframe::egui;

    pub fn install(_ctx: &egui::Context) {}
    pub fn take() -> Option<bool> {
      None
    }
  }

  #[cfg(test)]
  mod tests {
    use super::{click_outcome, is_double_click};

    #[test]
    fn tray_click_bursts_map_to_show_and_hide() {
      // A lone click waits for the deadline before it shows anything.
      assert_eq!(click_outcome(1, 0, 500), None);
      assert_eq!(click_outcome(1, 499, 500), None);
      assert_eq!(click_outcome(1, 500, 500), Some(true));
      // A second click inside the deadline hides the window.
      assert_eq!(click_outcome(2, 0, 500), Some(false));
      assert_eq!(click_outcome(2, 499, 500), Some(false));
      // Nothing pending.
      assert_eq!(click_outcome(0, 900, 500), None);
      // `0` means "no previous click", so the first click is never a double one.
      assert!(!is_double_click(0, 10, 500));
      assert!(is_double_click(10, 510, 500));
      assert!(!is_double_click(10, 511, 500));
    }
  }
}

#[cfg(target_os = "android")]
mod imp {
  use eframe::egui;

  use super::Action;
  use crate::i18n::Texts;

  pub struct Tray;

  impl Tray {
    pub fn new(_title: &str, _texts: &Texts, _fill: [u8; 3]) -> Self {
      Self
    }

    pub fn available(&self) -> bool {
      false
    }

    pub fn set_texts(&self, _texts: &Texts) {}

    pub fn set_enabled(&self, _connect: bool, _disconnect: bool) {}

    pub fn set_color(&self, _fill: [u8; 3]) -> Result<(), String> {
      Ok(())
    }

    #[allow(dead_code)]
    pub fn set_tooltip(&self, _tip: &str) {}

    pub fn poll(&self) -> Option<Action> {
      None
    }
  }

  pub fn install_events(_ctx: &egui::Context) {}
}

pub use imp::{Tray, install_events};
