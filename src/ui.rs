//! Tray icon + minimal egui window.
//!
//! The network work runs in one worker thread owning a `Session`; the UI only
//! exchanges messages with it, so the window never blocks on HTTP.
//!
//! `logic()` handles tray/menu/close events and keeps ticking even while the
//! window is hidden (that is what eframe calls instead of `ui()` then).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use auto_launch::{AutoLaunch, AutoLaunchBuilder, WindowsEnableMode};
use eframe::egui;
use serde_json::Value;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::Config;
use crate::i18n::{Lang, Texts};
use crate::logline::{Level, Line};
use crate::session::{Session, State};
use crate::srun::NetEnv;

pub enum Cmd {
  Connect,
  Disconnect,
  Refresh,
  SetLang(Lang),
  SetCredentials { username: String, password: String },
  SetAutoConnect(bool),
  Quit,
}

pub enum Ev {
  State(State),
  Info(Value),
  Log(Line),
  /// `Some` = a failure to surface; `None` = clear the banner.
  Error(Option<String>),
  /// Transient bottom-strip feedback for an action's outcome.
  Notify {
    msg: String,
    kind: ToastKind,
  },
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

fn level_color(level: Level, visuals: &egui::Visuals) -> egui::Color32 {
  match level {
    Level::Debug => egui::Color32::from_gray(135),
    Level::Info => visuals.text_color(),
    Level::Warn => egui::Color32::from_rgb(216, 160, 48),
    Level::Error => egui::Color32::from_rgb(228, 112, 112),
    Level::Fatal => egui::Color32::from_rgb(255, 138, 138),
  }
}

fn make_icon(rgb: [u8; 3]) -> Icon {
  const S: u32 = 32;
  let mut rgba = vec![0u8; (S * S * 4) as usize];
  let c = (S as f32 - 1.0) / 2.0;
  for y in 0..S {
    for x in 0..S {
      let dx = x as f32 - c;
      let dy = y as f32 - c;
      let d = (dx * dx + dy * dy).sqrt();
      let a = if d <= 12.0 {
        255
      } else if d <= 14.5 {
        (((14.5 - d) / 2.5) * 255.0) as u8
      } else {
        0
      };
      let i = ((y * S + x) * 4) as usize;
      rgba[i] = rgb[0];
      rgba[i + 1] = rgb[1];
      rgba[i + 2] = rgb[2];
      rgba[i + 3] = a;
    }
  }
  Icon::from_rgba(rgba, S, S).expect("valid rgba")
}

/// Window / taskbar icon: the packaged app icon (green disc with a white "T"),
/// drawn in code so the binary stays asset-free. The tray keeps its
/// state-coloured dot from `make_icon`.
fn app_icon() -> egui::IconData {
  const S: usize = 64;
  let mut rgba = vec![0u8; S * S * 4];
  let c = (S as f32 - 1.0) / 2.0;
  let radius = S as f32 * 0.47;
  for y in 0..S {
    for x in 0..S {
      let dx = x as f32 - c;
      let dy = y as f32 - c;
      let d = (dx * dx + dy * dy).sqrt();
      let alpha = if d <= radius {
        1.0
      } else if d <= radius + 1.5 {
        (radius + 1.5 - d) / 1.5
      } else {
        0.0
      };
      if alpha <= 0.0 {
        continue;
      }
      // Letter "T": a horizontal bar plus a centred stem.
      let bar = dx.abs() <= 11.0 && dy >= -13.0 && dy <= -5.5;
      let stem = dx.abs() <= 4.0 && dy >= -13.0 && dy <= 15.0;
      let i = (y * S + x) * 4;
      let (r, g, b) = if bar || stem {
        (255, 255, 255)
      } else {
        (46, 160, 67)
      };
      rgba[i] = r;
      rgba[i + 1] = g;
      rgba[i + 2] = b;
      rgba[i + 3] = (alpha * 255.0) as u8;
    }
  }
  egui::IconData {
    rgba,
    width: S as u32,
    height: S as u32,
  }
}

fn spawn_worker(cfg: Config) -> (Sender<Cmd>, Receiver<Ev>) {
  let (cmd_tx, cmd_rx) = channel::<Cmd>();
  let (ev_tx, ev_rx) = channel::<Ev>();
  std::thread::spawn(move || worker(cfg, cmd_rx, ev_tx));
  (cmd_tx, ev_rx)
}

fn worker(cfg: Config, rx: Receiver<Cmd>, tx: Sender<Ev>) {
  let keepalive = Duration::from_secs(cfg.keepalive_interval_secs.max(30));
  let mut lang = Lang::from_setting(&cfg.lang);
  let mut session = Session::new(cfg);
  let has_creds = !session.cfg().username.is_empty() && !session.cfg().password.is_empty();
  let mut want_online = session.cfg().auto_connect && has_creds;
  if session.cfg().auto_connect && !has_creds {
    tx.send(Ev::Log(Line::warn(lang.texts().need_credentials)))
      .ok();
  }
  let mut last_ka = Instant::now();
  let mut last_status = Instant::now() - Duration::from_secs(60);
  let mut last_state = State::Offline;
  // Backoff state so a rejected login does not hammer the portal every tick.
  let retry_delay = Duration::from_secs(session.cfg().reconnect_delay_secs.max(5));
  let mut next_attempt = Instant::now();
  let mut failures = 0u32;
  let mut last_failure = String::new();
  // Set when the user pressed "Connect", so a skipped attempt can be reported.
  let mut user_connect = false;
  // Link watch: `link_up()` costs a route lookup, so it can run every second.
  // A rising edge (network just became usable) retries immediately instead of
  // waiting out a backoff, which makes login start ~1 s after Wi-Fi connects.
  let mut next_link_check = Instant::now();
  let mut link_ready = false;

  for w in session.take_warnings() {
    tx.send(Ev::Log(Line::info(w))).ok();
  }

  loop {
    let t = lang.texts();
    while let Ok(c) = rx.try_recv() {
      match c {
        Cmd::Connect => {
          // Check the current state first so an existing session is not re-authenticated.
          if session.is_online_now() {
            if let Ok(v) = session.refresh() {
              tx.send(Ev::Info(v)).ok();
            }
            tx.send(Ev::State(session.state)).ok();
            tx.send(Ev::Error(None)).ok();
            tx.send(Ev::Log(Line::info(t.already_online))).ok();
            tx.send(Ev::Notify {
              msg: t.already_online.into(),
              kind: ToastKind::Info,
            })
            .ok();
          } else {
            want_online = true;
            user_connect = true;
            next_attempt = Instant::now();
            failures = 0;
            last_failure.clear();
            last_status = Instant::now() - Duration::from_secs(60);
          }
        }
        Cmd::Disconnect => {
          // Check the current state first so no useless logout request is sent.
          if !session.is_online_now() {
            if let Ok(v) = session.refresh() {
              tx.send(Ev::Info(v)).ok();
            }
            tx.send(Ev::State(session.state)).ok();
            tx.send(Ev::Log(Line::info(t.not_connected))).ok();
            tx.send(Ev::Notify {
              msg: t.not_connected.into(),
              kind: ToastKind::Info,
            })
            .ok();
          } else {
            match session.logout() {
              Ok(m) => {
                tx.send(Ev::Log(Line::info(format!("{}: {m}", t.disconnected))))
                  .ok();
                tx.send(Ev::Notify {
                  msg: t.disconnected.into(),
                  kind: ToastKind::Ok,
                })
                .ok();
              }
              Err(e) => {
                tx.send(Ev::Error(Some(format!("{}: {e}", t.disconnect_failed))))
                  .ok();
                tx.send(Ev::Notify {
                  msg: t.disconnect_failed.into(),
                  kind: ToastKind::Err,
                })
                .ok();
              }
            };
            want_online = false;
          }
        }
        Cmd::Refresh => match session.refresh() {
          Ok(v) => {
            tx.send(Ev::Info(v)).ok();
            tx.send(Ev::State(session.state)).ok();
            tx.send(Ev::Error(None)).ok();
            tx.send(Ev::Log(Line::info(t.status_refreshed))).ok();
            tx.send(Ev::Notify {
              msg: t.status_refreshed.into(),
              kind: ToastKind::Ok,
            })
            .ok();
          }
          Err(e) => {
            tx.send(Ev::Error(Some(format!("{}: {e}", t.refresh_failed))))
              .ok();
            tx.send(Ev::Notify {
              msg: t.refresh_failed.into(),
              kind: ToastKind::Err,
            })
            .ok();
          }
        },
        Cmd::SetLang(l) => {
          lang = l;
          session.set_lang(l);
        }
        Cmd::SetCredentials { username, password } => {
          let ready = !username.is_empty() && !password.is_empty();
          session.set_credentials(username, password);
          if ready && session.cfg().auto_connect && session.state != State::Online {
            want_online = true;
            last_status = Instant::now() - Duration::from_secs(60);
          }
        }
        Cmd::SetAutoConnect(on) => {
          want_online = on;
          if on && session.state != State::Online {
            next_attempt = Instant::now();
            last_status = Instant::now() - Duration::from_secs(60);
          }
        }
        Cmd::Quit => {
          // Quitting the app must not end the network session.
          return;
        }
      }
    }

    // Fast link watch: retry the moment a usable route appears.
    if want_online && session.state != State::Online && Instant::now() >= next_link_check {
      next_link_check = Instant::now() + Duration::from_secs(1);
      let up = crate::srun::link_up();
      if up && !link_ready {
        next_attempt = Instant::now();
        failures = 0;
        last_failure.clear();
      }
      link_ready = up;
    }

    if want_online && session.state != State::Online && link_ready && Instant::now() >= next_attempt
    {
      // Never spray login attempts at a network where logging in cannot work:
      // enterprise Wi-Fi (802.1X), no cable/Wi-Fi, a network that already
      // reaches the internet, or some other operator's captive portal.
      let mut skip: Option<(String, Duration, ToastKind)> = None;
      if let Some(ssid) = crate::wifi::no_portal_wifi() {
        let text = t.secure_wifi.replacen("{}", &ssid, 1);
        skip = Some((text, Duration::from_secs(60), ToastKind::Info));
      }

      let env = match skip {
        Some(_) => None,
        None => Some(session.net_env()),
      };
      if let Some(env) = env {
        match env {
          NetEnv::Campus { ac_id } => {
            session.set_ac_id(ac_id);
          }
          NetEnv::Unreachable => {
            skip = Some((
              t.network_unavailable.to_string(),
              Duration::from_secs(30),
              ToastKind::Info,
            ))
          }
          NetEnv::OtherPortal => {
            skip = Some((
              t.not_campus_network.to_string(),
              Duration::from_secs(60),
              ToastKind::Info,
            ))
          }
          NetEnv::DirectInternet => {
            // The internet works without interception. Either the campus
            // session is already authenticated, or this is a network that has
            // no portal at all (another operator, or WPA-Enterprise Wi-Fi such
            // as Tsinghua-Secure which the OS authenticates itself) - in which
            // case logging in is wrong.
            let online = session
              .refresh()
              .map(|_| session.state)
              .unwrap_or(State::Offline);
            if online != State::Online {
              skip = Some((
                t.no_login_needed.to_string(),
                Duration::from_secs(60),
                ToastKind::Info,
              ));
            }
          }
        }
      }

      if let Some((msg, delay, kind)) = skip {
        next_attempt = Instant::now() + delay;
        failures = 0;
        // Log (and toast) only when the reason changes, not on every retry.
        if last_failure != msg {
          last_failure = msg.clone();
          tx.send(Ev::Log(Line::warn(&msg))).ok();
          if user_connect {
            tx.send(Ev::Notify { msg, kind }).ok();
            user_connect = false;
          }
        }
        tx.send(Ev::State(session.state)).ok();
        continue;
      }

      match session.ensure_online() {
        Ok(m) => {
          failures = 0;
          last_failure.clear();
          user_connect = false;
          tx.send(Ev::Error(None)).ok();
          tx.send(Ev::Log(Line::info(m))).ok();
          tx.send(Ev::Notify {
            msg: t.connect_ok.into(),
            kind: ToastKind::Ok,
          })
          .ok();
        }
        Err(e) => {
          failures = failures.saturating_add(1);
          // 5s, 10s, 20s, 40s, ... capped at two minutes.
          let backoff = retry_delay.as_secs_f64() * 2f64.powi((failures - 1).min(5) as i32);
          next_attempt = Instant::now() + Duration::from_secs_f64(backoff.min(120.0));
          tx.send(Ev::Error(Some(e.clone()))).ok();
          tx.send(Ev::Log(Line::error(format!(
            "{e} (retry in {}s)",
            backoff.min(120.0) as u64
          ))))
          .ok();
          // Only buzz once per distinct reason, otherwise retries spam toasts.
          if e != last_failure {
            last_failure = e;
            tx.send(Ev::Notify {
              msg: t.connect_failed.into(),
              kind: ToastKind::Err,
            })
            .ok();
          }
        }
      };
    }

    if session.state == State::Online && last_status.elapsed() >= Duration::from_secs(15) {
      if let Ok(v) = session.client().status(&session.cfg().ac_id) {
        let online = v.get("online").and_then(|b| b.as_bool()).unwrap_or(false);
        if !online {
          // Session ended outside our control (timeout / kicked / unplugged).
          session.state = State::Offline;
          tx.send(Ev::Log(Line::warn(t.detected_offline))).ok();
        }
        tx.send(Ev::Info(v)).ok();
      }
      last_status = Instant::now();
    }

    if session.state == State::Online && last_ka.elapsed() >= keepalive {
      if session.keepalive().is_err() {
        let _ = session.ensure_online();
      }
      last_ka = Instant::now();
    }

    if session.state != last_state {
      tx.send(Ev::State(session.state)).ok();
      tx.send(Ev::Log(Line::debug(format!("state: {:?}", session.state))))
        .ok();
      last_state = session.state;
    }

    for w in session.take_warnings() {
      tx.send(Ev::Log(Line::info(w))).ok();
    }

    std::thread::sleep(Duration::from_millis(400));
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
  Ok,
  Err,
  Info,
}

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
  tx: Sender<Cmd>,
  rx: Receiver<Ev>,
  state: State,
  info: Info,
  logs: Vec<Line>,
  last_error: Option<String>,
  cfg: Config,
  lang: Lang,
  autostart: Option<AutoLaunch>,
  autostart_on: bool,
  user_edit: String,
  pass_edit: String,
  remember: bool,
  page: Page,
  toast: Option<Toast>,
  tray: TrayIcon,
  connect_item: MenuItem,
  disconnect_item: MenuItem,
  autostart_item: CheckMenuItem,
  desktop_item: MenuItem,
  started: bool,
  quitting: bool,
  quit_at: Option<Instant>,
}

/// Build the auto-launch entry; `--minimized` is only passed when the user
/// asked the app to start hidden.
fn autostart_entry(minimized: bool) -> Option<AutoLaunch> {
  let exe = std::env::current_exe().ok()?;
  let args: &[&str] = if minimized { &["--minimized"] } else { &[] };
  AutoLaunchBuilder::new()
    .set_app_name("thunet")
    .set_app_path(&exe.to_string_lossy())
    .set_args(args)
    .set_windows_enable_mode(WindowsEnableMode::CurrentUser)
    .build()
    .ok()
}

impl App {
  pub fn new(cfg: Config, minimized: bool, font: Option<&'static str>) -> Self {
    let (tx, rx) = spawn_worker(cfg.clone());
    let lang = Lang::from_setting(&cfg.lang);
    let t = lang.texts();

    let connect_item = MenuItem::with_id("connect", t.connect, true, None);
    let disconnect_item = MenuItem::with_id("disconnect", t.disconnect, true, None);
    let show_item = MenuItem::with_id("show", t.show_window, true, None);
    let autostart_item = CheckMenuItem::with_id("autostart", t.autostart, true, false, None);
    let desktop_item = MenuItem::with_id("desktop", t.desktop_shortcut, true, None);
    let quit_item = MenuItem::with_id("quit", t.quit, true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&connect_item, &disconnect_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append_items(&[&show_item, &autostart_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&desktop_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let tray = TrayIconBuilder::new()
      .with_menu(Box::new(menu.clone()))
      .with_tooltip(if minimized { "THUnet" } else { "THUnet" })
      .with_icon(make_icon([140, 140, 140]))
      .build()
      .expect("tray icon");

    let autostart = autostart_entry(cfg.start_minimized);
    let autostart_on = autostart
      .as_ref()
      .and_then(|a| a.is_enabled().ok())
      .unwrap_or(false);
    autostart_item.set_checked(autostart_on);

    // Surface where settings live; also proves the file exists on first run.
    let mut logs = vec![Line::info(format!(
      "{}: {}",
      t.config_path,
      crate::config::path().display()
    ))];
    // The portal host is a CLI/config-only setting, so log the effective value.
    logs.push(Line::debug(format!("portal: {}", cfg.host)));
    match font {
      Some(path) => logs.push(Line::debug(format!("CJK font: {path}"))),
      None => logs.push(Line::warn("no CJK font found; text may render as boxes")),
    }

    Self {
      tx,
      rx,
      state: State::Offline,
      info: Info::default(),
      logs,
      last_error: None,
      user_edit: cfg.username.clone(),
      pass_edit: cfg.password.clone(),
      remember: cfg.remember_password,
      page: Page::Status,
      toast: None,
      cfg,
      lang,
      autostart,
      autostart_on,
      tray,
      connect_item,
      disconnect_item,
      autostart_item,
      desktop_item,
      started: false,
      quitting: false,
      quit_at: None,
    }
  }

  fn t(&self) -> &'static Texts {
    self.lang.texts()
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
    self.desktop_item.set_text(t.desktop_shortcut);
    self.tx.send(Cmd::SetLang(self.lang)).ok();
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
        self.toast_ok(self.t().saved.to_string());
        self
          .tx
          .send(Cmd::SetCredentials {
            username: self.cfg.username.clone(),
            password: self.cfg.password.clone(),
          })
          .ok();
      }
      Err(e) => {
        self.toast_err(e);
      }
    }
  }

  fn toast_ok(&mut self, msg: String) {
    self.toast = Some(Toast {
      msg,
      at: Instant::now(),
      kind: ToastKind::Ok,
    });
  }

  fn toast_err(&mut self, msg: String) {
    self.toast = Some(Toast {
      msg,
      at: Instant::now(),
      kind: ToastKind::Err,
    });
  }

  /// "Start minimised" and "start with the system" are independent settings,
  /// so changing one rewrites the auto-launch command line.
  fn apply_start_minimized(&mut self, on: bool) {
    self.cfg.start_minimized = on;
    if let Err(e) = self.cfg.save() {
      self.toast_err(e);
      return;
    }
    let re_enable = self.autostart_on && self.autostart.is_some();
    self.autostart = autostart_entry(on);
    let mut failure = None;
    if re_enable {
      if let Some(a) = &self.autostart {
        if let Err(e) = a.enable() {
          failure = Some(format!("{}: {e}", self.t().autostart_failed));
        }
      }
    }
    match failure {
      Some(msg) => self.toast_err(msg),
      None => self.toast_ok(self.t().saved.to_string()),
    }
  }

  fn show_window(&self, ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
  }

  fn handle_menu(&mut self, ctx: &egui::Context) {
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
      match ev.id().0.as_str() {
        "connect" => {
          self.tx.send(Cmd::Connect).ok();
        }
        "disconnect" => {
          self.tx.send(Cmd::Disconnect).ok();
        }
        "autostart" => {
          self.autostart_on = !self.autostart_on;
          if let Some(a) = &self.autostart {
            let _ = if self.autostart_on {
              a.enable()
            } else {
              a.disable()
            };
          }
          self.autostart_item.set_checked(self.autostart_on);
        }
        "desktop" => match crate::shortcut::create_desktop_shortcut() {
          Ok(()) => self.toast_ok(self.t().done.to_string()),
          Err(e) => {
            self.push_log(Line::error(&e));
            self.toast_err(e);
          }
        },
        "quit" => {
          self.tx.send(Cmd::Quit).ok();
          self.quitting = true;
          self.quit_at = Some(Instant::now());
        }
        "show" => self.show_window(ctx),
        _ => {}
      }
    }
  }

  fn drain_events(&mut self) {
    while let Ok(ev) = self.rx.try_recv() {
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
      }
    }
  }

  fn paint(&mut self, ui: &mut egui::Ui) {
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
            });

          ui.add_space(10.0);
          ui.horizontal(|ui| {
            let online = self.state == State::Online;
            if ui
              .add_enabled(!online, egui::Button::new(t.connect))
              .clicked()
            {
              self.tx.send(Cmd::Connect).ok();
            }
            if ui
              .add_enabled(online, egui::Button::new(t.disconnect))
              .clicked()
            {
              self.tx.send(Cmd::Disconnect).ok();
            }
            if ui.button(t.refresh).clicked() {
              self.tx.send(Cmd::Refresh).ok();
            }
          });
        } else {
          // Denser spacing: the settings page has to fit the fixed window.
          ui.spacing_mut().item_spacing.y = 4.0;
          let mut auto = self.autostart_on;
          if ui.checkbox(&mut auto, t.autostart).changed() {
            self.autostart_on = auto;
            if let Some(a) = &self.autostart {
              if let Err(e) = if auto { a.enable() } else { a.disable() } {
                self
                  .logs
                  .push(Line::error(format!("{}: {e}", t.autostart_failed)));
              }
            }
          }

          let mut start_min = self.cfg.start_minimized;
          if ui.checkbox(&mut start_min, t.start_minimized).changed() {
            self.apply_start_minimized(start_min);
          }

          let mut auto_conn = self.cfg.auto_connect;
          if ui.checkbox(&mut auto_conn, t.auto_connect).changed() {
            self.cfg.auto_connect = auto_conn;
            self.tx.send(Cmd::SetAutoConnect(auto_conn)).ok();
            let _ = self.cfg.save();
          }

          ui.add_space(2.0);
          ui.separator();
          ui.horizontal(|ui| {
            ui.label(t.account);
            ui.add(egui::TextEdit::singleline(&mut self.user_edit).desired_width(190.0));
          });
          ui.horizontal(|ui| {
            ui.label(t.password);
            ui.add(
              egui::TextEdit::singleline(&mut self.pass_edit)
                .password(true)
                .desired_width(190.0),
            );
          });
          ui.horizontal(|ui| {
            if ui
              .checkbox(&mut self.remember, t.remember_password)
              .on_hover_text(t.remember_password_hint)
              .changed()
            {
              self.toast = None;
            }
            if ui.button(t.save).clicked() {
              self.apply_credentials();
            }
          });

          ui.add_space(2.0);
          let mut choice = match self.cfg.lang.as_str() {
            "zh" => 1,
            "en" => 2,
            _ => 0,
          };
          let before = choice;
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
          });
          if choice != before {
            let setting = match choice {
              1 => "zh",
              2 => "en",
              _ => "auto",
            };
            self.apply_lang_setting(setting);
          }
        }

        if self.page == Page::Status && !self.logs.is_empty() {
          ui.add_space(8.0);
          ui.separator();
          let everything = self
            .logs
            .iter()
            .map(Line::render)
            .collect::<Vec<_>>()
            .join("\n");
          // The context menu cannot call back into `self`, so record the
          // requested text and copy it once the scroll area has finished.
          let mut copy_request: Option<String> = None;
          // Fill whatever is left: with a fixed window height the log pane is
          // the only elastic element, so no empty band is left underneath it.
          let pane_w = ui.available_width();
          let pane_h = ui.available_height().max(60.0);
          ui.allocate_ui_with_layout(
            egui::vec2(pane_w, pane_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
              egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                  for line in &self.logs {
                    let color = {
                      let visuals = ui.visuals();
                      level_color(line.level, visuals)
                    };
                    let resp = ui.add(log_entry(ui, line.render(), color));
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
          if let Some(text) = copy_request {
            ui.ctx().copy_text(text);
            self.toast_ok(self.t().copied.to_string());
          }
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
    if !self.started {
      self.started = true;
      // The worker reads `auto_connect` itself on startup.
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
      } else {
        // Closing the window only hides it to the tray.
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
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
    let _ = self.tray.set_icon(Some(make_icon(rgb)));
    let t = self.t();
    let tip = match self.state {
      State::Online => format!("THUnet · {} · {}", t.online, dash(&self.info.ip4)),
      State::Authenticating => format!("THUnet · {}", t.authenticating),
      State::Offline => format!("THUnet · {}", t.offline),
    };
    let _ = self.tray.set_tooltip(Some(&tip));
    self.connect_item.set_enabled(self.state != State::Online);
    self
      .disconnect_item
      .set_enabled(self.state == State::Online);

    ctx.request_repaint_after(Duration::from_millis(500));
  }

  fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    self.paint(ui);
    paint_toast(ui.ctx(), self.toast.as_ref());
  }
}

const DEFAULT_WIDTH: f32 = 420.0;
/// Fixed client height: tall enough for the denser settings page, tight enough
/// that the status page has no dead space.
const WINDOW_HEIGHT: f32 = 300.0;

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
  egui::Area::new(egui::Id::unique("thunet_toast"))
    .order(egui::Order::Foreground)
    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -16.0])
    .show(ctx, |ui| {
      egui::Frame::new()
        .fill(bg)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
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

pub fn run(cfg: Config, minimized: bool) -> Result<(), String> {
  let options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default()
      .with_inner_size([DEFAULT_WIDTH, WINDOW_HEIGHT])
      .with_resizable(false)
      .with_visible(!minimized)
      .with_icon(std::sync::Arc::new(app_icon()))
      .with_title("THUnet"),
    // The window size is fixed by the layout, so a stale persisted size (or a
    // previous position off-screen) must not override it on the next launch.
    persist_window: false,
    ..Default::default()
  };
  eframe::run_native(
    "THUnet",
    options,
    Box::new(move |cc| {
      let font = install_fonts(&cc.egui_ctx);
      Ok(Box::new(App::new(cfg, minimized, font)))
    }),
  )
  .map_err(|e| e.to_string())
}
