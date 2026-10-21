//! Tray icon + minimal egui window.
//!
//! The network work runs in the background core (`thunet core`, see
//! [`crate::core`]); this process only renders the events it publishes. Closing
//! the window therefore no longer drops the session, and the window never
//! blocks on HTTP.
//!
//! `logic()` handles tray/menu/close events and keeps ticking even while the
//! window is hidden (that is what eframe calls instead of `ui()` then).

use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::Value;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::autostart::{Autostart, Mode};
use crate::config::Config;
use crate::i18n::{Lang, Texts};
use crate::ipc::Link;
use crate::logline::{Level, Line};
use crate::proto::{Cmd, CoreInfo, Ev, LoginEnv, ToastKind};
use crate::session::State;

impl LoginEnv {
  /// Is a login attempt possible here at all?
  fn allows_login(&self) -> bool {
    matches!(self, LoginEnv::Campus)
  }

  /// Why *Connect* is unavailable, or `None` on a campus network.
  fn blocked_reason(&self, t: &Texts) -> Option<String> {
    match self {
      LoginEnv::Campus => None,
      LoginEnv::EnterpriseWifi(ssid) => Some(t.secure_wifi.replacen("{}", ssid, 1)),
      LoginEnv::NoNetwork => Some(t.network_unavailable.to_string()),
      LoginEnv::OtherPortal => Some(t.not_campus_network.to_string()),
      LoginEnv::NoPortal => Some(t.no_login_needed.to_string()),
    }
  }
}

#[derive(Default, Clone)]
struct Info {
  ip4: String,
  ip6: String,
  bytes_in: u64,
  bytes_out: u64,
  name: String,
}

/// The portal reports `::` (or an all-zero address) when nothing is assigned;
/// treat those as "no address" so the copy button stays disabled.
fn norm_ip(s: &str) -> String {
  let t = s.trim();
  if t.is_empty() || t == "::" || t == "0:0:0:0:0:0:0:0" || t == "0.0.0.0" {
    String::new()
  } else {
    t.to_string()
  }
}

fn info_from(v: &Value) -> Info {
  let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
  Info {
    ip4: {
      let ip = norm_ip(s("online_ip"));
      if ip.is_empty() { norm_ip(s("ip")) } else { ip }
    },
    ip6: norm_ip(s("online_ip6")),
    bytes_in: v.get("bytes_in").and_then(Value::as_u64).unwrap_or(0),
    bytes_out: v.get("bytes_out").and_then(Value::as_u64).unwrap_or(0),
    name: s("user_name").to_string(),
  }
}

fn human(n: u64) -> String {
  const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
  let mut v = n as f64;
  let mut i = 0;
  while v >= 1024.0 && i + 1 < U.len() {
    v /= 1024.0;
    i += 1;
  }
  if i == 0 {
    format!("{n} B")
  } else {
    format!("{v:.1} {}", U[i])
  }
}

fn color_for(state: State) -> [u8; 3] {
  match state {
    State::Online => [46, 160, 67],
    State::Authenticating => [210, 153, 34],
    State::Offline => [140, 140, 140],
  }
}

/// The same palette, as an egui colour.
fn state_color(state: State) -> egui::Color32 {
  let c = color_for(state);
  egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// How long the service has been up, in the coarsest useful unit.
fn human_uptime(secs: u64) -> String {
  let days = secs / 86_400;
  let hours = (secs % 86_400) / 3_600;
  let minutes = (secs % 3_600) / 60;
  if days > 0 {
    format!("{days}d {hours}h")
  } else if hours > 0 {
    format!("{hours}h {minutes:02}m")
  } else {
    format!("{minutes}m")
  }
}

fn level_color(level: Level, visuals: &egui::Visuals) -> egui::Color32 {
  match level {
    Level::Debug => egui::Color32::from_gray(135),
    Level::Info => visuals.text_color(),
    Level::Warn => egui::Color32::from_rgb(216, 160, 48),
    Level::Error => egui::Color32::from_rgb(228, 112, 112),
    Level::Fatal => egui::Color32::from_rgb(255, 138, 138),
  }
}

/// Disc with a white "T": the app icon, drawn in code so the binary stays
/// asset-free. `fill` lets the tray reuse the same artwork with the state colour
/// (green online, amber authenticating, grey offline) instead of a plain dot.
fn disc_rgba(size: usize, fill: [u8; 3]) -> Vec<u8> {
  let mut rgba = vec![0u8; size * size * 4];
  let s = size as f32;
  let c = (s - 1.0) / 2.0;
  let radius = s * 0.47;
  let u = s / 64.0;
  for y in 0..size {
    for x in 0..size {
      let dx = x as f32 - c;
      let dy = y as f32 - c;
      let d = (dx * dx + dy * dy).sqrt();
      let alpha = if d <= radius {
        1.0
      } else if d <= radius + 1.5 * u {
        (radius + 1.5 * u - d) / (1.5 * u)
      } else {
        0.0
      };
      if alpha <= 0.0 {
        continue;
      }
      // Letter "T": a horizontal bar plus a centred stem (metrics for 64 px).
      let bar = dx.abs() <= 11.0 * u && dy >= -13.0 * u && dy <= -5.5 * u;
      let stem = dx.abs() <= 4.0 * u && dy >= -13.0 * u && dy <= 15.0 * u;
      let i = (y * size + x) * 4;
      let (r, g, b) = if bar || stem {
        (255, 255, 255)
      } else {
        (fill[0], fill[1], fill[2])
      };
      rgba[i] = r;
      rgba[i + 1] = g;
      rgba[i + 2] = b;
      rgba[i + 3] = (alpha * 255.0) as u8;
    }
  }
  rgba
}

fn make_icon(rgb: [u8; 3]) -> Icon {
  const S: usize = 64;
  Icon::from_rgba(disc_rgba(S, rgb), S as u32, S as u32).expect("valid rgba")
}

/// Window / taskbar icon: the packaged app icon (green disc with a white "T").
fn app_icon() -> egui::IconData {
  const S: usize = 64;
  egui::IconData {
    rgba: disc_rgba(S, [46, 160, 67]),
    width: S as u32,
    height: S as u32,
  }
}

// (the session loop now lives in `core.rs`)

/// Which pane the window is showing: the everyday status view, or the
/// rarely changed settings (startup behaviour, credentials, language).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
  Status,
  Settings,
}

struct Toast {
  msg: String,
  at: Instant,
  kind: ToastKind,
}

pub struct App {
  /// Connection to the background core; `None` until one answers. The app keeps
  /// retrying, so a core that is restarted does not leave the window deaf.
  link: Option<Link>,
  next_link_try: Instant,
  /// Last reported state of the core itself (version, uptime, pid).
  core: Option<CoreInfo>,
  next_core_query: Instant,
  core_version_warned: bool,
  state: State,
  info: Info,
  logs: Vec<Line>,
  last_error: Option<String>,
  cfg: Config,
  lang: Lang,
  /// Lowest level rendered in the log pane; older lines are kept and simply
  /// hidden, so raising the threshold again brings them back.
  min_level: Level,
  autostart: Option<Autostart>,
  autostart_mode: Mode,
  user_edit: String,
  pass_edit: String,
  remember: bool,
  /// Last network verdict from the worker; decides whether *Connect* is usable.
  env: LoginEnv,
  page: Page,
  toast: Option<Toast>,
  /// `None` when the platform refused to create a tray icon (no notification
  /// area, e.g. inside a validation sandbox). The app then stays window-only:
  /// closing quits instead of hiding, and a hidden start is not attempted.
  tray: Option<TrayIcon>,
  /// Colour of the icon currently installed in the tray; `set_icon` rewrites a
  /// temp PNG and re-announces the indicator, so it must only run on a change.
  tray_rgb: Option<[u8; 3]>,
  connect_item: MenuItem,
  disconnect_item: MenuItem,
  autostart_item: CheckMenuItem,
  show_item: MenuItem,
  hide_item: MenuItem,
  quit_item: MenuItem,
  started: bool,
  /// `--minimized`/`start_minimized`: the first painted frame must not be
  /// left on screen (eframe shows the root window after it unconditionally).
  hide_at_start: bool,
  /// The root window is DWM-cloaked until the first real "show" request, so
  /// eframe's forced first show never reaches the compositor.
  cloaked: bool,
  /// Mirrors the window state we asked the platform for. eframe does not track
  /// visibility, so the app keeps its own idea of whether the window is up.
  shown: bool,
  quitting: bool,
  quit_at: Option<Instant>,
}

/// Build the auto-launch entry; `--minimized` is only passed when the user
/// asked the app to start hidden.
fn autostart_entry(minimized: bool) -> Option<Autostart> {
  Autostart::new(minimized)
}

impl App {
  pub fn new(cfg: Config, minimized: bool, font: Option<&'static str>) -> Self {
    // The root window already exists at this point (eframe creates it before
    // calling the app creator) but has not been shown yet, so cloaking it here
    // means the forced show after the first painted frame stays invisible.
    if minimized {
      cloak::set(true);
    }

    // `main` normally started a core already; if it did not, `logic()` keeps
    // retrying (see `ensure_link`).
    let link = Link::connect();
    let lang = Lang::from_setting(&cfg.lang);
    let min_level = Level::from_setting(&cfg.log_level);
    let t = lang.texts();

    let connect_item = MenuItem::with_id("connect", t.connect, true, None);
    let disconnect_item = MenuItem::with_id("disconnect", t.disconnect, true, None);
    let show_item = MenuItem::with_id("show", t.show_window, true, None);
    let hide_item = MenuItem::with_id("hide", t.hide_window, true, None);
    let autostart_item = CheckMenuItem::with_id("autostart", t.autostart, true, false, None);
    let quit_item = MenuItem::with_id("quit", t.quit, true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&connect_item, &disconnect_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append_items(&[&show_item, &hide_item, &autostart_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let tray = TrayIconBuilder::new()
      .with_menu(Box::new(menu.clone()))
      // The menu lives on the right button; a left click brings the window back.
      .with_menu_on_left_click(false)
      .with_tooltip("THUnet")
      .with_icon(make_icon(color_for(State::Offline)))
      .build()
      .ok();
    // Without a tray icon there is no way back into a hidden window, so an
    // invisible start has to be cancelled and the window left on screen.
    let start_hidden = minimized && tray.is_some();
    if minimized && tray.is_none() {
      cloak::set(false);
    }

    let autostart = autostart_entry(cfg.start_minimized);

    // Surface where settings live; also proves the file exists on first run.
    let mut logs = vec![Line::info(format!(
      "config: {}",
      crate::config::path().display()
    ))];
    // An entry written by an older release is rewritten once (on Windows this
    // moves the `Run` value to a logon task, which fires earlier), and a
    // portable install gets the systemd unit so `systemctl --user start`
    // works without touching the settings page.
    match autostart.as_ref().map(Autostart::migrate) {
      Some(Ok(true)) => logs.push(Line::debug("autostart: entry upgraded")),
      Some(Err(e)) => logs.push(Line::warn(format!("autostart: {e}"))),
      _ => {}
    }
    // Read the mode *after* migrating: an older layout may just have been
    // renamed, and the radio buttons have to show the result.
    let autostart_mode = autostart.as_ref().map(Autostart::mode).unwrap_or(Mode::Off);
    autostart_item.set_checked(autostart_mode != Mode::Off);

    // The portal host is a CLI/config-only setting, so log the effective value.
    logs.push(Line::debug(format!("portal: {}", cfg.host)));
    if tray.is_none() {
      logs.push(Line::warn(
        "tray: unavailable, the window stays visible and closing quits",
      ));
    }
    match font {
      Some(path) => logs.push(Line::debug(format!("CJK font: {path}"))),
      None => logs.push(Line::warn("no CJK font found; text may render as boxes")),
    }

    Self {
      link,
      next_link_try: Instant::now(),
      core: None,
      next_core_query: Instant::now(),
      core_version_warned: false,
      state: State::Offline,
      info: Info::default(),
      logs,
      last_error: None,
      user_edit: cfg.username.clone(),
      pass_edit: cfg.password.clone(),
      remember: cfg.remember_password,
      // Until the worker's first probe lands, assume a login is possible: a
      // wrongly disabled button is worse than a click that reports why not.
      env: LoginEnv::Campus,
      page: Page::Status,
      toast: None,
      cfg,
      lang,
      min_level,
      autostart,
      autostart_mode,
      tray,
      tray_rgb: Some(color_for(State::Offline)),
      connect_item,
      disconnect_item,
      autostart_item,
      show_item,
      hide_item,
      quit_item,
      started: false,
      hide_at_start: start_hidden,
      cloaked: start_hidden,
      shown: !start_hidden,
      quitting: false,
      quit_at: None,
    }
  }

  fn t(&self) -> &'static Texts {
    self.lang.texts()
  }

  /// Send a command to the core; ignored when nothing is connected.
  fn send(&self, cmd: Cmd) {
    if let Some(link) = &self.link {
      link.send(cmd);
    }
  }

  fn next_event(&self) -> Option<Ev> {
    self.link.as_ref()?.try_recv()
  }

  /// Reconnect to the background core, without starting one.
  ///
  /// Only `main` starts a core. A window that kept respawning it would make
  /// `systemctl --user stop` / `schtasks /End` look broken.
  fn ensure_link(&mut self) {
    if self.link.as_ref().is_some_and(Link::alive) || Instant::now() < self.next_link_try {
      return;
    }
    self.next_link_try = Instant::now() + Duration::from_secs(2);
    match crate::ipc::open() {
      Some(link) => {
        self.link = Some(link);
        self.last_error = None;
        self.push_log(Line::info("connected to the background service"));
        self.send(Cmd::SetLang(self.lang));
        // Ask who we are talking to on the next pass.
        self.next_core_query = Instant::now();
      }
      None => {
        let msg = "the background service is not running".to_string();
        if self.last_error.as_deref() != Some(msg.as_str()) {
          self.push_log(Line::warn(&msg));
          self.last_error = Some(msg);
        }
      }
    }
  }

  /// Bounded ring buffer: the log keeps the newest lines only.
  fn push_log(&mut self, line: Line) {
    const MAX_LINES: usize = 500;
    self.logs.push(line);
    if self.logs.len() > MAX_LINES {
      let drop = self.logs.len() - MAX_LINES;
      self.logs.drain(..drop);
    }
  }

  fn apply_lang_setting(&mut self, setting: &str) {
    self.cfg.lang = setting.to_string();
    self.lang = Lang::from_setting(setting);
    let t = self.lang.texts();
    self.connect_item.set_text(t.connect);
    self.disconnect_item.set_text(t.disconnect);
    self.autostart_item.set_text(t.autostart);
    self.show_item.set_text(t.show_window);
    self.hide_item.set_text(t.hide_window);
    self.quit_item.set_text(t.quit);
    self.send(Cmd::SetLang(self.lang));
    if let Err(e) = self.cfg.save() {
      self.logs.push(Line::error(format!("config: {e}")));
    }
  }

  /// Persist the edited credentials and hand them to the worker.
  fn apply_credentials(&mut self) {
    self.cfg.username = self.user_edit.trim().to_string();
    self.cfg.password = self.pass_edit.clone();
    self.cfg.remember_password = self.remember;
    match self.cfg.save() {
      Ok(()) => {
        // Say what actually happened: unticking the checkbox deletes the stored
        // credential instead of writing one.
        let (msg, kind) = self.credentials_toast();
        self.set_toast(msg.to_string(), kind);
        self.send(Cmd::SetCredentials {
          username: self.cfg.username.clone(),
          password: self.cfg.password.clone(),
        });
      }
      Err(e) => {
        self.toast_err(e);
      }
    }
  }

  /// Throw away the edits and go back to what is stored, which is what *Revert*
  /// next to *Apply* does.
  fn revert_credentials(&mut self) {
    self.user_edit = self.cfg.username.clone();
    self.pass_edit = self.cfg.password.clone();
    self.remember = self.cfg.remember_password;
    self.toast = None;
  }

  fn credentials_toast(&self) -> (&'static str, ToastKind) {
    let t = self.t();
    if !self.cfg.remember_password {
      (t.password_cleared, ToastKind::Info)
    } else if self.cfg.password.is_empty() {
      (t.password_missing, ToastKind::Info)
    } else {
      (t.password_saved, ToastKind::Ok)
    }
  }

  fn set_toast(&mut self, msg: String, kind: ToastKind) {
    self.toast = Some(Toast {
      msg,
      at: Instant::now(),
      kind,
    });
  }

  fn toast_ok(&mut self, msg: String) {
    self.set_toast(msg, ToastKind::Ok);
  }

  fn toast_err(&mut self, msg: String) {
    self.set_toast(msg, ToastKind::Err);
  }

  /// Every switch in Settings reports the state it just moved to: nothing else
  /// on screen changes, so a silent flip looks like it did nothing.
  fn toast_toggle(&mut self, label: &'static str, on: bool) {
    let state = if on { self.t().on } else { self.t().off };
    self.set_toast(format!("{label}: {state}"), ToastKind::Info);
  }

  /// Apply one of the three autostart modes and report the result.
  fn apply_autostart_mode(&mut self, mode: Mode) {
    let t = self.t();
    let result = match &self.autostart {
      Some(a) => a.set_mode(mode),
      None => Err(t.autostart_failed.to_string()),
    };
    match result {
      Ok(()) => {
        self.autostart_mode = mode;
        self.autostart_item.set_checked(mode != Mode::Off);
        let label = match mode {
          Mode::Off => t.autostart_off,
          Mode::Core => t.autostart_core,
          Mode::All => t.autostart_all,
        };
        self.toast_ok(format!("{}: {label}", t.autostart));
      }
      Err(e) => {
        let msg = format!("{}: {e}", t.autostart_failed);
        self.push_log(Line::error(&msg));
        self.toast_err(msg);
      }
    }
  }

  /// "Start minimised" and "start with the system" are independent settings,
  /// so changing one rewrites the auto-launch command line.
  fn apply_start_minimized(&mut self, on: bool) {
    self.cfg.start_minimized = on;
    if let Err(e) = self.cfg.save() {
      self.toast_err(e);
      return;
    }
    let mode = self.autostart_mode;
    self.autostart = autostart_entry(on);
    let mut failure = None;
    if mode != Mode::Off
      && let Some(a) = &self.autostart
      && let Err(e) = a.set_mode(mode)
    {
      failure = Some(format!("{}: {e}", self.t().autostart_failed));
    }
    match failure {
      Some(msg) => self.toast_err(msg),
      None => self.toast_toggle(self.t().start_minimized, on),
    }
  }

  fn show_window(&mut self, ctx: &egui::Context) {
    if self.cloaked {
      cloak::set(false);
      self.cloaked = false;
    }
    self.shown = true;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
  }

  fn hide_window(&mut self, ctx: &egui::Context) {
    self.shown = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
  }

  fn handle_menu(&mut self, ctx: &egui::Context) {
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
      match ev.id().0.as_str() {
        "connect" => self.send(Cmd::Connect),
        "disconnect" => self.send(Cmd::Disconnect),
        "autostart" => {
          // The menu keeps the binary shortcut; the three-way choice lives in
          // the settings page.
          let next = if self.autostart_mode == Mode::Off {
            Mode::All
          } else {
            Mode::Off
          };
          self.apply_autostart_mode(next);
        }
        "quit" => {
          self.send(Cmd::Quit);
          self.quitting = true;
          self.quit_at = Some(Instant::now());
        }
        "show" => self.show_window(ctx),
        "hide" => self.hide_window(ctx),
        _ => {}
      }
    }
  }

  fn drain_events(&mut self) {
    while let Some(ev) = self.next_event() {
      match ev {
        Ev::State(s) => self.state = s,
        Ev::Info(v) => self.info = info_from(&v),
        Ev::Log(line) => self.push_log(line),
        Ev::Error(e) => self.last_error = e,
        Ev::Notify { msg, kind } => {
          self.toast = Some(Toast {
            msg,
            at: Instant::now(),
            kind,
          })
        }
        Ev::Env(env) => self.env = env,
        Ev::Core(info) => {
          // A half-finished upgrade leaves the old core running next to the new
          // window; say so once instead of showing mismatched numbers silently.
          if !self.core_version_warned && info.version != env!("CARGO_PKG_VERSION") {
            self.core_version_warned = true;
            self.push_log(Line::warn(format!(
              "service runs {} but this window runs {}; restart the service after upgrading",
              info.version,
              env!("CARGO_PKG_VERSION")
            )));
          }
          self.core = Some(info);
        }
        // Answers to the CLI's one-off questions; the window has no use for them.
        Ev::Status(_) | Ev::AcId(_) => {}
      }
    }
  }

  fn paint(&mut self, ui: &mut egui::Ui) {
    // Only the frame that actually paints the log pane may claim it, so a right
    // click on another page never looks like one on the log.
    LogCopyClick::set_log_rect(ui.ctx(), egui::Rect::NOTHING);
    // A right click on a log selection was redirected into a copy before the
    // press could reach the labels (see `LogCopyClick`); the labels only pick
    // the copy up while they are laid out, so inject it here.
    if LogCopyClick::take_copy(ui.ctx()) {
      ui.ctx().input_mut(|i| i.events.push(egui::Event::Copy));
      self.toast_ok(self.t().copied.to_string());
    }
    let t = self.t();
    let (label, rgb) = match self.state {
      State::Online => (t.online, color_for(State::Online)),
      State::Authenticating => (t.authenticating, color_for(State::Authenticating)),
      State::Offline => (t.offline, color_for(State::Offline)),
    };

    egui::Frame::new()
      .fill(ui.visuals().panel_fill)
      .inner_margin(egui::Margin::same(14))
      .show(ui, |ui| {
        ui.horizontal(|ui| {
          if self.page == Page::Status {
            ui.label(
              egui::RichText::new("●")
                .color(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                .size(18.0),
            );
            ui.label(egui::RichText::new(label).size(18.0).strong());
          } else {
            ui.label(egui::RichText::new(t.settings).size(18.0).strong());
          }
          ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let action = if self.page == Page::Status {
              t.settings
            } else {
              t.back
            };
            if ui.button(action).clicked() {
              self.page = match self.page {
                Page::Status => Page::Settings,
                Page::Settings => Page::Status,
              };
            }
          });
        });

        ui.add_space(8.0);

        if self.page == Page::Status {
          if let Some(err) = &self.last_error {
            egui::Frame::new()
              .fill(egui::Color32::from_rgb(90, 30, 30))
              .inner_margin(egui::Margin::same(8))
              .corner_radius(6)
              .show(ui, |ui| {
                ui.label(
                  egui::RichText::new(format!("{}: {err}", t.login_failed))
                    .color(egui::Color32::from_rgb(255, 190, 190)),
                );
              });
            ui.add_space(8.0);
          }

          let account = if !self.info.name.is_empty() {
            self.info.name.clone()
          } else if !self.cfg.username.is_empty() {
            self.cfg.username.clone()
          } else {
            "-".to_string()
          };
          egui::Grid::new("info")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
              ui.label(t.account);
              ui.label(egui::RichText::new(account).monospace());
              ui.end_row();
              ui.label(t.ipv4);
              ui.horizontal(|ui| {
                ui.label(egui::RichText::new(dash(&self.info.ip4)).monospace());
                if ui
                  .add_enabled(!self.info.ip4.is_empty(), egui::Button::new(t.copy))
                  .clicked()
                {
                  ui.ctx().copy_text(self.info.ip4.clone());
                  self.toast_ok(t.copied.to_string());
                }
              });
              ui.end_row();
              ui.label(t.ipv6);
              ui.horizontal(|ui| {
                ui.label(egui::RichText::new(dash(&self.info.ip6)).monospace());
                if ui
                  .add_enabled(!self.info.ip6.is_empty(), egui::Button::new(t.copy))
                  .clicked()
                {
                  ui.ctx().copy_text(self.info.ip6.clone());
                  self.toast_ok(t.copied.to_string());
                }
              });
              ui.end_row();
              ui.label(t.session_traffic);
              ui.label(format!(
                "↓ {}   ↑ {}",
                human(self.info.bytes_in),
                human(self.info.bytes_out)
              ));
              ui.end_row();
              // The core is a separate process; say whether one is answering and
              // which version it runs, so a half-finished upgrade is visible.
              ui.label(t.service);
              match &self.core {
                Some(core) => {
                  ui.horizontal(|ui| {
                    ui.label(
                      egui::RichText::new(t.service_running).color(state_color(State::Online)),
                    );
                    let version = format!("v{} · {}", core.version, human_uptime(core.uptime_secs));
                    let text = egui::RichText::new(version).monospace();
                    ui.label(if core.version == env!("CARGO_PKG_VERSION") {
                      text.weak()
                    } else {
                      text.color(state_color(State::Authenticating))
                    });
                  });
                }
                None => {
                  ui.label(egui::RichText::new(t.service_gone).color(state_color(State::Offline)));
                }
              }
              ui.end_row();
            });

          ui.add_space(10.0);
          ui.horizontal(|ui| {
            let online = self.state == State::Online;
            // Off-campus (or with no link at all) a login attempt can only be
            // rejected, so the button is greyed out and says why on hover.
            let blocked = self.env.blocked_reason(t);
            let can_connect = !online && blocked.is_none();
            let mut connect = ui.add_enabled(can_connect, egui::Button::new(t.connect));
            // Only explain the greyed-out state when the network is the reason;
            // while online the button is off for an obvious reason already.
            if let (false, Some(why)) = (online, &blocked) {
              connect = connect.on_disabled_hover_text(why);
            }
            if connect.clicked() {
              self.send(Cmd::Connect);
            }
            if ui
              .add_enabled(online, egui::Button::new(t.disconnect))
              .clicked()
            {
              self.send(Cmd::Disconnect);
            }
            if ui.button(t.refresh).clicked() {
              self.send(Cmd::Refresh);
            }
          });
        } else {
          // Denser spacing: the settings page has to fit the fixed window.
          ui.spacing_mut().item_spacing.y = 4.0;

          // Language and log level lead the page: they are the two settings
          // people look for first.
          let mut choice = match self.cfg.lang.as_str() {
            "zh" => 1,
            "en" => 2,
            _ => 0,
          };
          let before_lang = choice;
          let before_level = self.min_level;
          // Both combos share a row: the settings page has to fit the fixed
          // window height.
          ui.horizontal(|ui| {
            ui.label(t.language);
            egui::ComboBox::from_id_salt("lang")
              .selected_text(match choice {
                1 => t.lang_zh,
                2 => t.lang_en,
                _ => t.lang_auto,
              })
              .show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, 0, t.lang_auto);
                ui.selectable_value(&mut choice, 1, t.lang_zh);
                ui.selectable_value(&mut choice, 2, t.lang_en);
              });
            ui.add_space(10.0);
            ui.label(t.log_level);
            egui::ComboBox::from_id_salt("log-level")
              .selected_text(self.min_level.tag())
              .show_ui(ui, |ui| {
                for level in Level::CHOICES {
                  ui.selectable_value(&mut self.min_level, level, level.tag());
                }
              });
          });
          if choice != before_lang {
            let setting = match choice {
              1 => "zh",
              2 => "en",
              _ => "auto",
            };
            self.apply_lang_setting(setting);
          }
          if self.min_level != before_level {
            self.cfg.log_level = self.min_level.setting().to_string();
            let _ = self.cfg.save();
          }

          ui.add_space(2.0);
          // Three modes, not two checkboxes: "tray without core" is not worth
          // offering, because the tray would start a core a second later. With
          // the short labels from `i18n.rs` all three fit on the label's row,
          // which is what keeps the page inside the fixed window height.
          let mut mode = self.autostart_mode;
          ui.horizontal(|ui| {
            ui.label(t.autostart);
            for (value, label) in [
              (Mode::Off, t.autostart_off),
              (Mode::Core, t.autostart_core),
              (Mode::All, t.autostart_all),
            ] {
              ui.radio_value(&mut mode, value, label);
            }
          });
          if mode != self.autostart_mode {
            self.apply_autostart_mode(mode);
          }

          // Only meaningful when the tray is part of the autostart mode.
          let mut start_min = self.cfg.start_minimized;
          let tray_starts = self.autostart_mode != Mode::Core;
          if ui
            .add_enabled(
              tray_starts,
              egui::Checkbox::new(&mut start_min, t.start_minimized),
            )
            .changed()
          {
            self.apply_start_minimized(start_min);
          }

          let mut auto_conn = self.cfg.auto_connect;
          if ui.checkbox(&mut auto_conn, t.auto_connect).changed() {
            self.cfg.auto_connect = auto_conn;
            self.send(Cmd::SetAutoConnect(auto_conn));
            match self.cfg.save() {
              Ok(()) => self.toast_toggle(t.auto_connect, auto_conn),
              Err(e) => self.toast_err(e),
            }
          }

          ui.add_space(2.0);
          if ui.button(t.desktop_shortcut).clicked() {
            match crate::shortcut::create_desktop_shortcut() {
              Ok(()) => self.toast_ok(t.done.to_string()),
              Err(e) => {
                self.push_log(Line::error(&e));
                self.toast_err(e);
              }
            }
          }

          ui.add_space(2.0);
          ui.separator();
          // Comment on each field separately: the marker belongs next to the
          // input that actually differs from what is stored.
          let stored =
            CredentialEdits::new(&self.user_edit, &self.pass_edit, self.remember, &self.cfg);
          ui.horizontal(|ui| {
            ui.label(t.account);
            ui.add(egui::TextEdit::singleline(&mut self.user_edit).desired_width(190.0));
            if stored.username {
              unsaved_marker(ui, t);
            }
          });
          ui.horizontal(|ui| {
            ui.label(t.password);
            ui.add(
              egui::TextEdit::singleline(&mut self.pass_edit)
                .password(true)
                .desired_width(190.0),
            );
            if stored.password {
              unsaved_marker(ui, t);
            }
          });
          ui.horizontal(|ui| {
            if ui
              .checkbox(&mut self.remember, t.remember_password)
              .on_hover_text(t.remember_password_hint)
              .changed()
            {
              self.toast = None;
            }
            // This row is the only one that does not save as you type, so the
            // button shows when there is anything left to apply.
            let mut apply = egui::Button::new(t.apply);
            if stored.any() {
              apply = apply.fill(ui.visuals().selection.bg_fill);
            }
            if ui.add(apply).clicked() {
              self.apply_credentials();
            }
            // Cancelling an edit only makes sense while a password is actually
            // remembered: without one there is nothing to restore.
            if ui
              .add_enabled(self.remember, egui::Button::new(t.revert))
              .on_hover_text(t.revert_hint)
              .clicked()
            {
              self.revert_credentials();
            }
            if stored.remember {
              unsaved_marker(ui, t);
            }
          });
        }
        let visible: Vec<&Line> = self
          .logs
          .iter()
          .filter(|l| l.level >= self.min_level)
          .collect();
        if self.page == Page::Status && !visible.is_empty() {
          ui.add_space(8.0);
          ui.separator();
          let everything = visible
            .iter()
            .map(|l| l.render())
            .collect::<Vec<_>>()
            .join("\n");
          // The context menu cannot call back into `self`, so record the
          // requested text and copy it once the scroll area has finished.
          let mut copy_request: Option<String> = None;
          // Fill whatever is left: with a fixed window height the log pane is
          // the only elastic element, so no empty band is left underneath it.
          let pane_w = ui.available_width();
          let pane_h = ui.available_height().max(60.0);
          let pane = ui.allocate_ui_with_layout(
            egui::vec2(pane_w, pane_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
              egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                  for line in &visible {
                    let color = {
                      let visuals = ui.visuals();
                      level_color(line.level, visuals)
                    };
                    let resp = ui.add(log_entry(ui, line.render(), color));
                    // A right click on a selection is turned into a copy before
                    // egui sees it (`LogCopyClick`), so this menu only shows up
                    // when nothing is selected.
                    resp.context_menu(|ui| {
                      if ui.button(t.copy_line).clicked() {
                        copy_request = Some(line.render());
                        ui.close();
                      }
                      if ui.button(t.copy_all).clicked() {
                        copy_request = Some(everything.clone());
                        ui.close();
                      }
                    });
                  }
                });
            },
          );
          LogCopyClick::set_log_rect(ui.ctx(), pane.response.rect);
          if let Some(text) = copy_request {
            ui.ctx().copy_text(text);
            self.toast_ok(self.t().copied.to_string());
          }
        }

        // The settings page ends with a build stamp pinned to the bottom edge:
        // laying it out bottom-up puts it at the panel edge without having to
        // guess its height.
        if self.page == Page::Settings {
          ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(2.0);
            // Version on the left, build time on the right: they answer
            // different questions, so the two ends of the strip read better
            // than one run-on line.
            ui.horizontal(|ui| {
              ui.label(egui::RichText::new(build_version()).small().weak());
              ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(build_time_label(t)).small().weak());
              });
            });
            ui.separator();
          });
        }
        // Consume the leftover vertical space so the frame background reaches
        // the bottom edge instead of leaving an unpainted band.
        let rest = ui.available_height();
        if rest > 0.0 {
          ui.allocate_space(egui::vec2(ui.available_width(), rest));
        }
      });
  }
}

fn dash(s: &str) -> String {
  if s.is_empty() {
    "-".to_string()
  } else {
    s.to_string()
  }
}

/// Which part of the account row differs from what is stored. The other
/// settings save as they change; this row only commits on *Apply*, and each
/// field carries its own marker so the reader can see what is unsaved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CredentialEdits {
  username: bool,
  password: bool,
  remember: bool,
}

impl CredentialEdits {
  fn new(user_edit: &str, pass_edit: &str, remember: bool, cfg: &Config) -> Self {
    Self {
      // Applying trims the account, so a stray space is not a change.
      username: user_edit.trim() != cfg.username,
      password: pass_edit != cfg.password,
      remember: remember != cfg.remember_password,
    }
  }

  fn any(self) -> bool {
    self.username || self.password || self.remember
  }
}

/// Small "Unsaved" hint placed next to the input it belongs to.
fn unsaved_marker(ui: &mut egui::Ui, t: &Texts) {
  ui.label(
    egui::RichText::new(t.unsaved)
      .small()
      .color(ui.visuals().warn_fg_color),
  );
}

/// Is any label text selected in this viewport? egui keeps label selections in
/// a context plugin; the selected characters are only assembled while the
/// labels are laid out, which is why a copy is queued as an event.
fn has_label_selection(ctx: &egui::Context) -> bool {
  ctx
    .plugin_opt::<egui::text_selection::LabelSelectionState>()
    .is_some_and(|plugin| plugin.lock().has_selection())
}

/// Turns a right click on a log selection into a copy.
///
/// egui cannot do this by itself: any mouse press over a selectable label
/// collapses the selection to a caret (`ViewportLabelSelectionState::on_label`,
/// which reacts to `pointer.any_pressed()`, not just the primary button), and
/// `selected_text` treats such an empty range as "the whole label" — which is
/// why a right click used to copy the entire line instead of the selection.
///
/// The press is therefore removed from the raw input before egui processes it,
/// and the frame that follows injects `Event::Copy`, so the labels copy the
/// selection while it is still intact. The selection also stays visible.
struct LogCopyClick {
  /// A right click was redirected; the next painted frame injects the copy.
  copy_pending: bool,
  /// The release belonging to a swallowed press still has to be dropped.
  swallow_release: bool,
  /// The log pane in global coordinates, as laid out by the previous frame.
  log_rect: egui::Rect,
}

impl Default for LogCopyClick {
  fn default() -> Self {
    Self {
      copy_pending: false,
      swallow_release: false,
      log_rect: egui::Rect::NOTHING,
    }
  }
}

impl LogCopyClick {
  /// Consume a redirected right click.
  fn take_copy(ctx: &egui::Context) -> bool {
    ctx
      .plugin_opt::<Self>()
      .is_some_and(|plugin| std::mem::take(&mut plugin.lock().copy_pending))
  }

  fn set_log_rect(ctx: &egui::Context, rect: egui::Rect) {
    if let Some(plugin) = ctx.plugin_opt::<Self>() {
      plugin.lock().log_rect = rect;
    }
  }
}

impl egui::plugin::Plugin for LogCopyClick {
  fn debug_name(&self) -> &'static str {
    "LogCopyClick"
  }

  fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
    if !has_label_selection(ctx) {
      self.swallow_release = false;
      return;
    }
    input.events.retain(|event| {
      let egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Secondary,
        pressed,
        ..
      } = event
      else {
        return true;
      };
      if !pressed {
        // Only the release of a press taken above is dropped; without a
        // matching press it cannot become a secondary click anyway.
        return !std::mem::take(&mut self.swallow_release);
      }
      if self.log_rect.contains(*pos) {
        self.copy_pending = true;
        self.swallow_release = true;
        return false;
      }
      true
    });
  }
}

/// Log rows are laid out with `break_anywhere` so long unbroken text (paths,
/// URLs) wraps instead of being clipped at the panel edge.
fn log_entry(ui: &egui::Ui, text: String, color: egui::Color32) -> egui::Label {
  let font = egui::FontId::monospace(10.0);
  let job = egui::text::LayoutJob {
    text: text.clone(),
    sections: vec![egui::text::LayoutSection {
      leading_space: 0.0,
      byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(text.len()),
      format: egui::TextFormat {
        font_id: font,
        color,
        ..Default::default()
      },
    }],
    wrap: egui::text::TextWrapping {
      max_width: ui.available_width(),
      break_anywhere: true,
      ..Default::default()
    },
    ..Default::default()
  };
  let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
  egui::Label::new(galley).selectable(true)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Right-clicking a log selection relies on egui copying label selections
  /// when a `Copy` event is present while it lays the labels out. This pins
  /// that behaviour (and the fact that a right-click keeps the selection).
  #[test]
  fn copy_event_copies_the_label_selection() {
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

    let ctx = egui::Context::default();
    let run = |ctx: &egui::Context, events: Vec<Event>| {
      let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(240.0, 120.0))),
        events,
        ..Default::default()
      };
      let mut out = ctx.run_ui(input, |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
          ui.add(egui::Label::new("hello selection").selectable(true));
        });
      });
      // Nothing here uploads textures; dropping them unapplied would panic.
      out.textures_delta.clear();
      out
    };
    let press = |pressed, pos: Pos2| Event::PointerButton {
      pos,
      button: PointerButton::Primary,
      pressed,
      modifiers: Default::default(),
    };

    // Lay out once, then drag over the text to select it.
    let _ = run(&ctx, vec![]);
    let start = pos2(12.0, 12.0);
    let end = pos2(70.0, 12.0);
    let _ = run(&ctx, vec![Event::PointerMoved(start)]);
    let _ = run(&ctx, vec![press(true, start)]);
    let _ = run(&ctx, vec![Event::PointerMoved(end)]);
    let _ = run(&ctx, vec![press(false, end)]);
    assert!(
      has_label_selection(&ctx),
      "drag should select the label text"
    );

    // A right-click keeps the selection (that is what makes the log menu skippable) ...
    let _ = run(
      &ctx,
      vec![Event::PointerButton {
        pos: end,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Default::default(),
      }],
    );
    assert!(
      has_label_selection(&ctx),
      "a right-click must not clear the selection"
    );

    // ... and the queued copy event then yields the selected text.
    let out = run(&ctx, vec![Event::Copy]);
    let copied = out
      .platform_output
      .commands
      .iter()
      .find_map(|cmd| match cmd {
        egui::OutputCommand::CopyText(text) => Some(text.clone()),
        _ => None,
      });
    let copied = copied.expect("a Copy event should produce clipboard output");
    assert!(
      copied.contains("hello"),
      "copied text should be the selection, got {copied:?}"
    );
  }

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

  /// Right-clicking a log selection must copy only the selection. egui collapses
  /// a label selection as soon as any button is pressed over it, so `LogCopyClick`
  /// removes that press from the raw input and lets the labels copy the intact
  /// range — this pins that end-to-end behaviour.
  #[test]
  fn right_click_copies_only_the_selection() {
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, pos2, vec2};

    let ctx = egui::Context::default();
    ctx.add_plugin(LogCopyClick::default());
    let run = |ctx: &egui::Context, events: Vec<Event>| {
      let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(240.0, 120.0))),
        events,
        ..Default::default()
      };
      let mut out = ctx.run_ui(input, |ui| {
        let pane = egui::CentralPanel::default().show(ui, |ui| {
          ui.add(log_entry(
            ui,
            "abcd efgh ijkl".to_string(),
            egui::Color32::WHITE,
          ));
          ui.add(log_entry(
            ui,
            "second line here".to_string(),
            egui::Color32::WHITE,
          ));
        });
        LogCopyClick::set_log_rect(ui.ctx(), pane.response.rect);
      });
      out.textures_delta.clear();
      out
    };
    let button = |pressed, button, pos: Pos2| Event::PointerButton {
      pos,
      button,
      pressed,
      modifiers: Default::default(),
    };
    let copied = |out: &egui::FullOutput| {
      out
        .platform_output
        .commands
        .iter()
        .find_map(|cmd| match cmd {
          egui::OutputCommand::CopyText(t) => Some(t.clone()),
          _ => None,
        })
    };

    let _ = run(&ctx, vec![]);
    let start = pos2(9.0, 10.0);
    let end = pos2(24.0, 10.0);
    let _ = run(&ctx, vec![Event::PointerMoved(start)]);
    let _ = run(&ctx, vec![button(true, PointerButton::Primary, start)]);
    let _ = run(&ctx, vec![Event::PointerMoved(end)]);
    let _ = run(&ctx, vec![button(false, PointerButton::Primary, end)]);
    assert!(has_label_selection(&ctx), "drag should select some text");

    // The right click is redirected into a copy, and the press never reaches egui.
    let _ = run(&ctx, vec![button(true, PointerButton::Secondary, end)]);
    assert!(
      LogCopyClick::take_copy(&ctx),
      "the right click should request a copy"
    );
    let _ = run(&ctx, vec![button(false, PointerButton::Secondary, end)]);

    let out = run(&ctx, vec![Event::Copy]);
    let copied = copied(&out).expect("the copy event should reach the clipboard");
    assert_eq!(
      copied, "abc",
      "only the selected characters may be copied, not the whole line"
    );
    assert!(
      has_label_selection(&ctx),
      "the selection should stay visible after the copy"
    );
  }

  #[test]
  fn credential_edits_flag_the_field_that_changed() {
    let cfg = Config {
      username: "alice".into(),
      password: "pw".into(),
      remember_password: true,
      ..Config::default()
    };
    let same = CredentialEdits::new("alice", "pw", true, &cfg);
    assert_eq!(same, CredentialEdits::default(), "unchanged");
    assert!(!same.any());
    // A stray space is trimmed away when applying, so it is not a change.
    assert!(!CredentialEdits::new("  alice  ", "pw", true, &cfg).username);

    let user = CredentialEdits::new("other", "pw", true, &cfg);
    assert!(user.username && !user.password && !user.remember);
    let pass = CredentialEdits::new("alice", "pw2", true, &cfg);
    assert!(!pass.username && pass.password && !pass.remember);
    let remember = CredentialEdits::new("alice", "pw", false, &cfg);
    assert!(!remember.username && !remember.password && remember.remember);
    assert!(remember.any());
  }

  /// Toasts used to break over two lines because the area is sized from the
  /// previous frame; a short message must stay on one line, a long one may wrap
  /// but has to stay inside the window.
  #[test]
  fn toast_wraps_only_when_it_has_to() {
    fn area_of(msg: &str) -> egui::Rect {
      let ctx = egui::Context::default();
      let frame = |ctx: &egui::Context| {
        let input = egui::RawInput {
          screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(420.0, 300.0),
          )),
          ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
          let toast = Toast {
            msg: msg.to_owned(),
            at: Instant::now(),
            kind: ToastKind::Ok,
          };
          paint_toast(ui.ctx(), Some(&toast));
        });
        out.textures_delta.clear();
      };
      // The second pass uses the size measured by the first one.
      frame(&ctx);
      frame(&ctx);
      ctx
        .memory(|m| m.area_rect(egui::Id::unique("thunet_toast")))
        .expect("the toast area should exist")
    }

    let short = area_of("Password saved");
    assert!(short.height() < 40.0, "must stay on one line: {short:?}");
    assert!(short.width() < 200.0, "must hug the text: {short:?}");

    let long = area_of("No network connection (cable unplugged / Wi-Fi down), not logging in");
    assert!(long.width() <= 420.0, "must stay in the window: {long:?}");
    assert!(long.height() > short.height(), "must wrap: {long:?}");
  }

  /// The footer is the only place the build date shows, so pin its shape:
  /// `THUnet <version>` on the left, `Built at YYYY-MM-DD HH:MM:SS +HH:MM` right.
  #[test]
  fn build_stamp_ends_with_a_seconds_precision_local_time() {
    let version = build_version();
    assert_eq!(
      version,
      format!(
        "{} {}",
        crate::shortcut::APP_TITLE,
        env!("CARGO_PKG_VERSION")
      ),
      "the left half is name plus version"
    );

    let label = build_time_label(crate::i18n::Lang::En.texts());
    let rest = label.strip_prefix("Built at ").expect(&label);
    let (date, rest) = rest.split_once(' ').expect("date first: {rest}");
    let (clock, offset) = rest.split_once(' ').expect("offset last: {rest}");
    assert_eq!(date.len(), 10, "{label}");
    assert_eq!(clock.len(), 8, "seconds are part of the stamp: {label}");
    assert_eq!(offset.len(), 6, "numeric UTC offset: {label}");
    assert!(
      offset.starts_with(['+', '-']) && offset.as_bytes()[3] == b':',
      "{label}"
    );

    let shaped = |s: &str, sep: char, seps: &[usize]| {
      s.chars().enumerate().all(|(i, c)| {
        if seps.contains(&i) {
          c == sep
        } else {
          c.is_ascii_digit()
        }
      })
    };
    assert!(shaped(date, '-', &[4, 7]), "{label}");
    assert!(shaped(clock, ':', &[2, 5]), "{label}");
    assert!(shaped(&offset[1..], ':', &[2]), "{label}");
  }

  /// Off-campus networks must leave *Connect* disabled with an explanation;
  /// only a campus portal makes a login attempt possible.
  #[test]
  fn only_a_campus_network_allows_connect() {
    let t = crate::i18n::Lang::En.texts();
    assert!(LoginEnv::Campus.allows_login());
    assert_eq!(LoginEnv::Campus.blocked_reason(t), None);

    let blocked = [
      LoginEnv::EnterpriseWifi("Tsinghua-Secure".into()),
      LoginEnv::NoNetwork,
      LoginEnv::OtherPortal,
      LoginEnv::NoPortal,
    ];
    for env in &blocked {
      assert!(!env.allows_login(), "{env:?}");
      let why = env.blocked_reason(t).expect("a reason has to be shown");
      assert!(!why.trim().is_empty(), "{env:?}");
    }

    // The SSID is named in the hint instead of leaving its placeholder behind.
    let hint = blocked[0].blocked_reason(t).unwrap_or_default();
    assert!(hint.contains("Tsinghua-Secure"), "{hint}");
    assert!(!hint.contains("{}"), "{hint}");
  }

  #[test]
  fn placeholder_addresses_are_empty() {
    assert_eq!(norm_ip("::"), "");
    assert_eq!(norm_ip("  ::  "), "");
    assert_eq!(norm_ip("0:0:0:0:0:0:0:0"), "");
    assert_eq!(norm_ip("0.0.0.0"), "");
    assert_eq!(norm_ip(""), "");
    assert_eq!(norm_ip("2001:db8::abcd"), "2001:db8::abcd");
    assert_eq!(norm_ip("192.0.2.10"), "192.0.2.10");
  }

  #[test]
  fn info_from_normalizes_ips() {
    // Documentation addresses (RFC 5737 / RFC 3849) and a made-up account.
    let v: Value = serde_json::from_str(
      r#"{"online_ip":"192.0.2.10","online_ip6":"::","bytes_in":1024,"user_name":"1234567890"}"#,
    )
    .unwrap();
    let info = info_from(&v);
    assert_eq!(info.ip4, "192.0.2.10");
    assert_eq!(info.ip6, "", "`::` must not look copyable");
    assert_eq!(info.name, "1234567890");

    // Falls back to `ip` when no session address is reported.
    let v: Value = serde_json::from_str(r#"{"ip":"198.51.100.7"}"#).unwrap();
    assert_eq!(info_from(&v).ip4, "198.51.100.7");
  }
}

impl eframe::App for App {
  fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    // GTK delivers the appindicator menu callbacks through its own event loop,
    // but eframe owns the main loop here. Pump GTK every pass, otherwise menu
    // clicks are queued forever and never reach `handle_menu`.
    #[cfg(target_os = "linux")]
    while gtk::events_pending() {
      gtk::main_iteration_do(false);
    }

    if !self.started {
      self.started = true;
      // Started visible: make sure the window is not left behind the app that
      // was on screen (the usual case when an installer launches it).
      if self.shown {
        foreground::raise(crate::shortcut::APP_TITLE);
      }
      // The worker reads `auto_connect` itself on startup.
      if let Some(start) = STARTED.get() {
        self.push_log(Line::debug(format!(
          "startup: {} ms to tray",
          start.elapsed().as_millis()
        )));
      }
    }

    // eframe always calls `Window::set_visible(true)` after the first painted
    // frame, ignoring `ViewportBuilder::with_visible(false)` (its viewport info
    // does not track visibility). So when the app was asked to start in the
    // tray, hide the window again in the same event-loop pass: the commands
    // sent from `logic` are applied *after* that forced show, before the
    // desktop compositor gets a chance to put the window on screen.
    if self.hide_at_start {
      self.hide_at_start = false;
      self.hide_window(ctx);
    }

    // A second launch rings the doorbell instead of calling `ShowWindow` on
    // our window (which would desynchronise winit's cached visibility).
    if crate::single::show_requested() {
      self.show_window(ctx);
    }

    // The core may have been restarted (crash, upgrade, systemd unit or logon
    // task): reconnect instead of going deaf.
    self.ensure_link();

    // Ask the service about itself now and then; the row it fills is advisory,
    // so a slow cadence is plenty.
    if self.link.as_ref().is_some_and(Link::alive) && Instant::now() >= self.next_core_query {
      self.next_core_query = Instant::now() + Duration::from_secs(30);
      self.send(Cmd::CoreInfo);
    }

    // A left click on the tray icon shows the window, a double click hides it.
    match tray_click() {
      Some(true) => self.show_window(ctx),
      Some(false) => self.hide_window(ctx),
      None => {}
    }

    self.handle_menu(ctx);
    self.drain_events();

    if self
      .toast
      .as_ref()
      .map(|t| t.at.elapsed() > Duration::from_millis(2500))
      .unwrap_or(false)
    {
      self.toast = None;
    }

    if ctx.input(|i| i.viewport().close_requested()) {
      if self.quitting {
        // Programmatic close from the tray "Quit" item: let it through.
      } else if self.tray.is_some() {
        // Closing the window only hides it to the tray.
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.hide_window(ctx);
      } else {
        // No tray icon: hiding would leave the app unreachable, so close quits.
        self.send(Cmd::Quit);
        self.quitting = true;
        self.quit_at = Some(Instant::now());
      }
    }
    if self.quitting {
      ctx.send_viewport_cmd(egui::ViewportCommand::Close);
      // A hidden window may ignore Close on some platforms; force it.
      if self
        .quit_at
        .map(|t| t.elapsed() > Duration::from_millis(1500))
        .unwrap_or(false)
      {
        std::process::exit(0);
      }
    }

    // Keep ticking while hidden so tray clicks are still processed.
    let rgb = color_for(self.state);
    if self.tray_rgb != Some(rgb) {
      self.tray_rgb = Some(rgb);
      if let Some(tray) = &self.tray {
        let _ = tray.set_icon(Some(make_icon(rgb)));
      }
    }
    let t = self.t();
    let tip = match self.state {
      State::Online => format!("THUnet · {} · {}", t.online, dash(&self.info.ip4)),
      State::Authenticating => format!("THUnet · {}", t.authenticating),
      State::Offline => format!("THUnet · {}", t.offline),
    };
    if let Some(tray) = &self.tray {
      let _ = tray.set_tooltip(Some(&tip));
    }
    self
      .connect_item
      .set_enabled(self.state != State::Online && self.env.allows_login());
    self
      .disconnect_item
      .set_enabled(self.state == State::Online);

    // When hidden, wake up often enough to pick up a second launch quickly.
    let tick = if self.shown { 500 } else { 200 };
    ctx.request_repaint_after(Duration::from_millis(tick));
  }

  fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    self.paint(ui);
    paint_toast(ui.ctx(), self.toast.as_ref());
  }
}

const DEFAULT_WIDTH: f32 = 420.0;
/// Fixed client height: tall enough for the denser settings page (including
/// its log-level row and the build stamp), tight enough that the status page
/// has no dead space.
const WINDOW_HEIGHT: f32 = 300.0;

/// Local time of the compile that produced this binary, to the second, with the
/// numeric UTC offset so "local" needs no further qualification. `build.rs`
/// stamps the epoch; the conversion happens here, in the timezone of whoever is
/// looking at the window.
fn build_time() -> String {
  use chrono::TimeZone;
  let epoch = env!("THUNET_BUILD_EPOCH").parse::<i64>().unwrap_or(0);
  match chrono::Local.timestamp_opt(epoch, 0).single() {
    Some(t) => t.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
    None => "unknown".to_string(),
  }
}

/// Left half of the settings footer: name plus version.
fn build_version() -> String {
  format!(
    "{} {}",
    crate::shortcut::APP_TITLE,
    env!("CARGO_PKG_VERSION")
  )
}

/// Right half of the settings footer: what the build time actually is.
fn build_time_label(t: &Texts) -> String {
  format!("{} {}", t.built, build_time())
}

/// Floating feedback strip so both "copied" and "saved" are visible
/// regardless of where the triggering widget sits.
fn paint_toast(ctx: &egui::Context, toast: Option<&Toast>) {
  let Some(tst) = toast else { return };
  if tst.at.elapsed() > Duration::from_millis(2500) {
    return;
  }
  let (fg, bg) = match tst.kind {
    ToastKind::Ok => (
      egui::Color32::from_rgb(210, 255, 220),
      egui::Color32::from_rgb(28, 92, 45),
    ),
    ToastKind::Err => (
      egui::Color32::from_rgb(255, 205, 205),
      egui::Color32::from_rgb(96, 32, 32),
    ),
    ToastKind::Info => (
      egui::Color32::from_rgb(210, 230, 255),
      egui::Color32::from_rgb(30, 62, 105),
    ),
  };
  // An area wraps its content at the width it had in the previous frame; the
  // default is narrow enough that even a two-word toast broke over two lines.
  // Give it the window width (minus a margin) so short messages stay on one
  // line and only genuinely long ones wrap or get clipped at the edge.
  let max_w = (ctx.content_rect().width() - 32.0).max(120.0);
  egui::Area::new(egui::Id::unique("thunet_toast"))
    .order(egui::Order::Foreground)
    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -16.0])
    .default_width(max_w)
    .show(ctx, |ui| {
      egui::Frame::new()
        .fill(bg)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
          // The area sizes itself from the previous frame, so the label has to
          // be told how wide it may become or a short message breaks in two.
          ui.set_max_width(max_w);
          ui.label(egui::RichText::new(&tst.msg).color(fg));
        });
    });
}

/// egui's bundled fonts have no CJK glyphs, so Chinese text renders as boxes.
/// Append the first available system CJK font as a fallback (latin text still
/// uses the default font because this is appended, not prepended).
fn install_fonts(ctx: &egui::Context) -> Option<&'static str> {
  const CANDIDATES: &[&str] = &[
    // Windows
    "C:/Windows/Fonts/msyh.ttc", // Microsoft YaHei
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/simsun.ttc",
    // macOS
    "/System/Library/Fonts/PingFang.ttc",
    // Linux
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
  ];

  for path in CANDIDATES {
    let Ok(bytes) = std::fs::read(path) else {
      continue;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
      "cjk".to_owned(),
      std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
      fonts
        .families
        .entry(family)
        .or_default()
        .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    return Some(path);
  }
  None
}

/// Keeps the window out of the compositor while the app is supposed to live in
/// the tray only.
///
/// eframe shows the root window right after painting its first frame (its
/// viewport info never tracks visibility), and only then applies the
/// `Visible(false)` command sent from `logic`. Skipping that show entirely is
/// not possible through egui, so on Windows the window is cloaked instead:
/// it can still be "shown", but the desktop compositor never draws it.
#[cfg(windows)]
mod cloak {
  use windows_sys::Win32::Foundation::HWND;
  use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
  use windows_sys::Win32::System::Threading::GetCurrentProcessId;
  use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  /// Our own top-level window (there is only one per process).
  fn hwnd() -> Option<HWND> {
    let title = wide("THUnet");
    // SAFETY: plain Win32 queries on a title we own.
    unsafe {
      let h = FindWindowW(std::ptr::null(), title.as_ptr());
      if h.is_null() {
        return None;
      }
      let mut pid = 0u32;
      GetWindowThreadProcessId(h, &mut pid);
      (pid == GetCurrentProcessId()).then_some(h)
    }
  }

  pub fn set(cloaked: bool) {
    let Some(h) = hwnd() else { return };
    let value: i32 = i32::from(cloaked);
    // SAFETY: `value` is a BOOL, which is what DWMWA_CLOAK expects.
    unsafe {
      DwmSetWindowAttribute(
        h,
        DWMWA_CLOAK as u32,
        (&value as *const i32).cast(),
        size_of::<i32>() as u32,
      );
    }
  }
}

#[cfg(not(windows))]
mod cloak {
  pub fn set(_cloaked: bool) {}
}

/// Windows keeps the foreground with the process that owns the last input
/// event, so a copy started by an installer (or by another program) opens
/// behind whatever is on screen and can look like it never started. Raise the
/// window once; when Windows still refuses, flash the taskbar button.
#[cfg(windows)]
mod foreground {
  use std::mem::size_of;

  use windows_sys::Win32::System::Threading::GetCurrentProcessId;
  use windows_sys::Win32::UI::WindowsAndMessaging::{
    FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FindWindowW, FlashWindowEx, GetWindowThreadProcessId,
    SetForegroundWindow,
  };

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  pub fn raise(title: &str) {
    let title = wide(title);
    // SAFETY: plain Win32 calls on a window whose pid is checked below.
    unsafe {
      let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
      if hwnd.is_null() {
        return;
      }
      let mut pid = 0u32;
      GetWindowThreadProcessId(hwnd, &mut pid);
      if pid != GetCurrentProcessId() {
        return;
      }
      if SetForegroundWindow(hwnd) != 0 {
        return;
      }
      let flash = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 3,
        dwTimeout: 0,
      };
      FlashWindowEx(&flash);
    }
  }
}

#[cfg(not(windows))]
mod foreground {
  pub fn raise(_title: &str) {}
}

/// Entry time of `run`, for the "startup: N ms to tray" log line.
static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Left clicks counted in the current burst: 0 = nothing pending, 1 = one click
/// waiting for the double-click deadline, 2 = a double click.
static CLICKS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// When the latest left click arrived, in ms since `STARTED`; 0 = no click yet.
static LAST_CLICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
  // 0 is the "no click yet" sentinel, so the clock starts at 1.
  STARTED
    .get()
    .map_or(0, |s| s.elapsed().as_millis() as u64 + 1)
}

/// A second left click inside this window counts as a double click. 300 ms is
/// snappier than the 500 ms most desktops use; the delay is a compromise, since
/// a single click can only be acted on once no second click can follow.
const DOUBLE_CLICK_MS: u64 = 300;

/// Does the click at `now` follow `previous` closely enough to be the second
/// half of a double click? `previous == 0` means there was no click.
fn is_double_click(previous: u64, now: u64, deadline: u64) -> bool {
  previous != 0 && now.saturating_sub(previous) <= deadline
}

/// What a pending click burst means: `None` waits for the deadline to pass,
/// `Some(true)` shows the window, `Some(false)` hides it.
fn click_outcome(clicks: u8, since_last: u64, deadline: u64) -> Option<bool> {
  match clicks {
    2 => Some(false),
    1 if since_last >= deadline => Some(true),
    _ => None,
  }
}

/// Consume the pending left click, if it can be resolved already.
fn tray_click() -> Option<bool> {
  use std::sync::atomic::Ordering;

  let since_last = now_ms().saturating_sub(LAST_CLICK.load(Ordering::Relaxed));
  let outcome = click_outcome(CLICKS.load(Ordering::Relaxed), since_last, DOUBLE_CLICK_MS);
  if outcome.is_some() {
    CLICKS.store(0, Ordering::Relaxed);
  }
  outcome
}

/// Tray callbacks arrive on another thread. Without a handler every event waits
/// for the next tick of the (deliberately slow) repaint loop, which makes a
/// click on the icon feel dead; the handler records the click and wakes egui.
/// It must be registered before the icon exists, because the crate ignores a
/// late registration once an event has gone through the default channel.
fn install_tray_events(ctx: &egui::Context) {
  use std::sync::atomic::Ordering;

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
    // `TrayIconEvent::DoubleClick` never arrives: tray-icon registers its window
    // class without `CS_DBLCLKS`, so Windows never posts `WM_LBUTTONDBLCLK`.
    // Count the clicks here instead.
    let now = now_ms();
    let previous = LAST_CLICK.swap(now, Ordering::Relaxed);
    let deadline = DOUBLE_CLICK_MS;
    let double = is_double_click(previous, now, deadline);
    CLICKS.store(if double { 2 } else { 1 }, Ordering::Relaxed);
    // A single click is only acted on once the deadline has passed: acting at
    // once would show the window on the first click of a double click and hide
    // it again on the second, which looks like a flash.
    ctx.request_repaint_after(Duration::from_millis(deadline));
    ctx.request_repaint();
  }));
}

/// OpenGL first (smallest footprint), then wgpu. Virtual machines and RDP
/// sessions often expose only OpenGL 1.1, where `egui_glow` refuses to start and
/// the process used to die with exit code 1 before any window appeared;
/// `THUNET_RENDERER=wgpu` skips straight to the fallback.
pub fn run(cfg: Config, minimized: bool) -> Result<(), String> {
  STARTED.get_or_init(Instant::now);
  let force_wgpu = std::env::var("THUNET_RENDERER")
    .map(|v| v.eq_ignore_ascii_case("wgpu"))
    .unwrap_or(false);
  let attempts: &[eframe::Renderer] = if force_wgpu {
    &[eframe::Renderer::Wgpu]
  } else {
    &[eframe::Renderer::Glow, eframe::Renderer::Wgpu]
  };
  let mut last = String::from("no renderer available");
  for renderer in attempts {
    match run_with(cfg.clone(), minimized, *renderer) {
      Ok(()) => return Ok(()),
      Err(e) => last = format!("{}: {e}", renderer_name(renderer)),
    }
  }
  Err(last)
}

fn renderer_name(renderer: &eframe::Renderer) -> &'static str {
  match renderer {
    eframe::Renderer::Glow => "glow",
    eframe::Renderer::Wgpu => "wgpu",
  }
}

/// Linux only: initialise GTK before `tray-icon` creates its menu.
///
/// `tray-icon` builds the appindicator menu with gtk-rs, and every gtk-rs
/// constructor asserts that `gtk::init()` already ran on the main thread.
/// eframe uses winit only and never touches GTK, so without this the app
/// panicked at startup with "GTK has not been initialized".
#[cfg(target_os = "linux")]
fn init_gtk() -> Result<(), String> {
  gtk::init().map_err(|e| format!("gtk: {e}"))
}

#[cfg(not(target_os = "linux"))]
fn init_gtk() -> Result<(), String> {
  Ok(())
}

fn run_with(cfg: Config, minimized: bool, renderer: eframe::Renderer) -> Result<(), String> {
  let mut options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default()
      .with_inner_size([DEFAULT_WIDTH, WINDOW_HEIGHT])
      .with_resizable(false)
      .with_visible(!minimized)
      .with_icon(std::sync::Arc::new(app_icon()))
      .with_title("THUnet"),
    // The window size is fixed by the layout, so a stale persisted size (or a
    // previous position off-screen) must not override it on the next launch.
    persist_window: false,
    renderer,
    ..Default::default()
  };

  // Wayland cannot hide a toplevel: winit's `set_visible` is a no-op there
  // ("Not possible on Wayland"), so *Hide window* and closing the window to the
  // tray would leave it on screen, and only minimise-to-taskbar works. XWayland
  // supports both, and GNOME draws its normal server-side title bar for X11
  // clients. Prefer it whenever a display is available; a pure Wayland session
  // still falls back to the adwaita client-side decorations.
  // `THUNET_UNIX_BACKEND=wayland` opts out.
  #[cfg(target_os = "linux")]
  {
    let have_x11 = std::env::var_os("DISPLAY").is_some();
    let force_wayland = std::env::var("THUNET_UNIX_BACKEND").is_ok_and(|v| v == "wayland");
    if have_x11 && !force_wayland {
      options.event_loop_builder = Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
      }));
    }
  }

  eframe::run_native(
    "THUnet",
    options,
    Box::new(move |cc| {
      // Must happen before `App::new`, which builds the tray menu.
      init_gtk().map_err(std::io::Error::other)?;
      let font = install_fonts(&cc.egui_ctx);
      install_tray_events(&cc.egui_ctx);
      cc.egui_ctx.add_plugin(LogCopyClick::default());
      Ok(Box::new(App::new(cfg, minimized, font)))
    }),
  )
  .map_err(|e| e.to_string())
}
