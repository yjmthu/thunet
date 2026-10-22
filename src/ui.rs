//! Tray icon + minimal egui window.
//!
//! The network work runs in the background core (`thunet core`, see
//! [`crate::core`]); this process only renders the events it publishes. Closing
//! the window therefore no longer drops the session, and the window never
//! blocks on HTTP.
//!
//! `logic()` handles tray/menu/close events and keeps ticking even while the
//! window is hidden (that is what eframe calls instead of `ui()` then).

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::Value;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
// Click events exist only in the Windows backend; the Linux one
// (`libappindicator`) never reports a click, so nothing there consumes these.
#[cfg(windows)]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::autostart::{Autostart, Mode};
use crate::config::Config;
use crate::i18n::{Lang, Texts};
use crate::ipc::Link;
use crate::logline::{Level, Line};
use crate::proto::{Cmd, CoreInfo, CredentialState, Ev, Failure, LinkKind, LoginEnv, ToastKind};
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

/// `config.json`'s `theme` setting ("auto" | "light" | "dark"), as egui wants
/// it. `auto` follows the system preference.
fn theme_preference(setting: &str) -> egui::ThemePreference {
  match setting {
    "light" => egui::ThemePreference::Light,
    "dark" => egui::ThemePreference::Dark,
    _ => egui::ThemePreference::System,
  }
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
  let half = (s - 1.0) / 2.0;
  let radius = s * 0.47;
  let edge = (s * 0.06).max(1.0);
  for y in 0..size {
    for x in 0..size {
      let d = ((x as f32 - half).powi(2) + (y as f32 - half).powi(2)).sqrt();
      let alpha = if d <= radius {
        1.0
      } else if d <= radius + edge {
        (radius + edge - d) / edge
      } else {
        0.0
      };
      if alpha <= 0.0 {
        continue;
      }
      let t = letter_cover(x as f32, y as f32, s);
      let i = (y * size + x) * 4;
      rgba[i] = mix(fill[0], 255, t);
      rgba[i + 1] = mix(fill[1], 255, t);
      rgba[i + 2] = mix(fill[2], 255, t);
      rgba[i + 3] = (alpha * 255.0) as u8;
    }
  }
  rgba
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
  (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
}

/// Coverage of the white "T" at one pixel, 0.0 .. 1.0, in icon coordinates.
///
/// The metrics are the ones `scripts/make_icon.py` draws the packaged
/// `assets/thunet.ico` with, feathering included, so the tray, the taskbar and
/// the Start Menu shortcut show the same letter instead of three variations of
/// it.
fn letter_cover(x: f32, y: f32, size: f32) -> f32 {
  let half = (size - 1.0) / 2.0;
  let bar_half_w = size * 0.17;
  let bar_top = size * 0.29;
  let bar_thick = (size * 0.115).max(1.2);
  let stem_half = (size * 0.062).max(0.9);
  let stem_bottom = size * 0.74;
  let feather = 0.75;

  let inside_bar = (x - half).abs() <= bar_half_w && y >= bar_top && y <= bar_top + bar_thick;
  let inside_stem = (x - half).abs() <= stem_half && y >= bar_top && y <= stem_bottom;
  if inside_bar || inside_stem {
    return 1.0;
  }
  // Distance to the nearest edge, so diagonal pixels blend instead of
  // stair-stepping.
  let dx_bar = (x - half).abs() - bar_half_w;
  let dy_bar = (bar_top - y).max(y - (bar_top + bar_thick));
  let dx_stem = (x - half).abs() - stem_half;
  let dy_stem = (bar_top - y).max(y - stem_bottom);
  let gap = dx_bar.max(dy_bar).min(dx_stem.max(dy_stem));
  if gap <= 0.0 {
    return 1.0;
  }
  (1.0 - gap / feather).max(0.0)
}

fn make_icon(rgb: [u8; 3]) -> Icon {
  const S: usize = 64;
  Icon::from_rgba(disc_rgba(S, rgb), S as u32, S as u32).expect("valid rgba")
}

/// Size of the header icons (*Settings*, *Back*) in points.
///
/// Set by hand rather than from the button's text height: the glyphs are
/// textures with their own proportions, and this size lines them up with the
/// labels next to them.
const ICON_SIDE: f32 = 15.0;

/// The same, beside a page heading, which is drawn larger.
const HEADING_ICON_SIDE: f32 = 18.0;

/// Blue the portable setup uses where the installed app is green.
///
/// The letter stays the brand "T": the two modes are the same application, and
/// a second glyph only made the small tray icon unreadable.
const PORTABLE_FILL: [u8; 3] = [58, 120, 200];

/// Window / taskbar icon: the packaged app icon (green disc with a white "T"),
/// blue while this process is the portable one.
fn app_icon() -> egui::IconData {
  const S: usize = 64;
  egui::IconData {
    rgba: disc_rgba(S, app_fill(State::Online)),
    width: S as u32,
    height: S as u32,
  }
}

/// Fill of this process's icons for one state.
///
/// The installed app owns green / amber / grey (online, authenticating,
/// offline). The portable setup moves that whole scale into blue, brightness
/// carrying the state, because its window and tray sit next to the installed
/// ones and are otherwise impossible to tell apart - the two modes keep
/// separate accounts, credentials and endpoints.
fn app_fill(state: State) -> [u8; 3] {
  if !crate::ipc::portable() {
    return color_for(state);
  }
  match state {
    State::Online => PORTABLE_FILL,
    State::Authenticating => [108, 158, 216],
    State::Offline => [72, 88, 116],
  }
}

/// What this process calls itself in the title bar, the tray tooltip and
/// Alt-Tab. One string for all of them: the two modes keep separate state,
/// credentials and endpoints, and a user running both has to be able to tell
/// which one is asking.
fn app_name() -> &'static str {
  if crate::ipc::portable() {
    "THUnet Portable"
  } else {
    "THUnet"
  }
}

/// The gear in front of *Settings*, drawn in code: asset-free like the app
/// icon, and independent of the fonts, since egui bundles only the glyphs in
/// `special_emojis` and the system CJK fallback has no gear either.
fn gear_image() -> egui::ColorImage {
  const S: usize = 64;
  let c = (S as f32 - 1.0) / 2.0;
  let mut rgba = vec![0u8; S * S * 4];
  for y in 0..S {
    for x in 0..S {
      let dx = x as f32 - c;
      let dy = y as f32 - c;
      // 0 at the centre, 1 at the edge of the icon.
      let rho = (dx * dx + dy * dy).sqrt() / c;
      // Teeth: the rim is further out inside a tooth than in the gap after it,
      // which gives eight rectangular teeth with radial sides.
      let phase = (dy.atan2(dx) / std::f32::consts::TAU * 8.0).rem_euclid(1.0);
      let outer = if phase < 0.5 { 0.95 } else { 0.72 };
      // Two soft edges make the teeth survive being scaled down to 15 px.
      let rim = 1.0 - ramp(rho, outer - 0.05, outer + 0.03);
      let hub = ramp(rho, 0.26, 0.34);
      let alpha = (rim * hub).clamp(0.0, 1.0);
      let i = (y * S + x) * 4;
      rgba[i] = 255;
      rgba[i + 1] = 255;
      rgba[i + 2] = 255;
      rgba[i + 3] = (alpha * 255.0).round() as u8;
    }
  }
  egui::ColorImage::from_rgba_unmultiplied([S, S], &rgba)
}

/// 0 at or below `lo`, 1 at or above `hi`, linear in between.
fn ramp(v: f32, lo: f32, hi: f32) -> f32 {
  ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Left arrow for the *Back* button: a shaft with two head strokes.
///
/// White and soft-edged like the gear, so the button can tint it to its own
/// text colour.
fn back_arrow_image() -> egui::ColorImage {
  const S: usize = 64;
  // In units of the icon's own size, so the shape does not depend on `S`.
  let strokes = [
    ((0.24, 0.50), (0.80, 0.50)), // shaft
    ((0.48, 0.26), (0.24, 0.50)), // upper head
    ((0.48, 0.74), (0.24, 0.50)), // lower head
  ];
  let half = 0.055; // half the stroke width
  let s = S as f32;
  let mut rgba = vec![0u8; S * S * 4];
  for y in 0..S {
    for x in 0..S {
      // Sample the pixel centre, so the arrow stays symmetric.
      let p = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
      let alpha = strokes
        .iter()
        .map(|&(from, to)| segment_alpha(p, from, to, half))
        .fold(0.0, f32::max);
      let i = (y * S + x) * 4;
      rgba[i] = 255;
      rgba[i + 1] = 255;
      rgba[i + 2] = 255;
      rgba[i + 3] = (alpha * 255.0).round() as u8;
    }
  }
  egui::ColorImage::from_rgba_unmultiplied([S, S], &rgba)
}

/// Coverage of a round-capped stroke from `a` to `b` with half-width `half`.
fn segment_alpha(p: (f32, f32), a: (f32, f32), b: (f32, f32), half: f32) -> f32 {
  let (px, py) = p;
  let (dx, dy) = (b.0 - a.0, b.1 - a.1);
  let len2 = dx * dx + dy * dy;
  let t = if len2 > 0.0 {
    (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
  } else {
    0.0
  };
  let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
  let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
  // A ~1 px soft edge at 64 px keeps the arrow smooth at 15 px.
  1.0 - ramp(d, half - 0.012, half + 0.012)
}

/// Upload an icon once and hand the same handle out afterwards.
///
/// `load_texture` allocates a new GPU texture on every call, and the window is
/// repainted twice a second even when nothing happens.
fn icon_texture(
  slot: &mut Option<egui::TextureHandle>,
  ctx: &egui::Context,
  name: &str,
  image: fn() -> egui::ColorImage,
) -> egui::TextureHandle {
  slot
    .get_or_insert_with(|| ctx.load_texture(name, image(), egui::TextureOptions::LINEAR))
    .clone()
}

/// An icon-only header button, tinted like a text button.
fn icon_button(texture: egui::TextureId) -> egui::Button<'static> {
  egui::Button::image(egui::Image::new(egui::load::SizedTexture::new(
    texture,
    [ICON_SIDE, ICON_SIDE],
  )))
  .image_tint_follows_text_color(true)
}

/// An icon beside a page heading, painted in the text colour and drawn larger
/// than the header buttons.
fn heading_icon(
  slot: &mut Option<egui::TextureHandle>,
  ctx: &egui::Context,
  name: &str,
  image: fn() -> egui::ColorImage,
  color: egui::Color32,
) -> egui::Image<'static> {
  let texture = icon_texture(slot, ctx, name, image);
  egui::Image::new(egui::load::SizedTexture::new(
    texture.id(),
    [HEADING_ICON_SIDE, HEADING_ICON_SIDE],
  ))
  .tint(color)
}
// (the session loop now lives in `core.rs`)

/// Which pane the window is showing: the everyday status view, or the
/// rarely changed settings (startup behaviour, credentials, language).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
  Status,
  Settings,
}

/// A click on the status page's Start/Stop button whose outcome is still
/// unknown. The toast fires once the service has actually appeared or gone
/// away, not when the button was pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ServiceRequest {
  Start,
  Stop,
}

/// How long a Start/Stop may take before the click is reported as failed.
const SERVICE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long an account switch may stay unanswered before *Apply* is released.
/// The core logs out and logs in twice, so a few round trips are normal; this
/// only exists so a lost answer cannot disable the button forever.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait before probing the local endpoint again.
const LINK_RETRY: Duration = Duration::from_secs(2);

/// The same, while a core that cannot talk to this build is answering: only a
/// restart changes that answer, so a faster loop would just rebuild a
/// connection that is refused immediately. Short enough that a manual restart
/// is picked up without waiting out a full period.
const STALE_RETRY: Duration = Duration::from_secs(5);

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
  /// What the core holds in the credential slot. The window never receives
  /// the password, so this — not `cfg.password` — is what the *Remember
  /// password* row and the *Unsaved* markers are based on once a core answers.
  core_cred: Option<CredentialState>,
  /// Result of comparing the typed password with the core's copy: `Some(true)`
  /// means the text in the field is already the stored value, so it is not an
  /// edit. Cleared whenever the field changes.
  pass_matches: Option<bool>,
  /// Debounce for [`Cmd::CheckCredentials`], and the exact text that was sent,
  /// so one value is not compared on every frame.
  pass_check_at: Option<Instant>,
  pass_checked: String,
  next_core_query: Instant,
  core_version_warned: bool,
  /// Which interface carries the default route. Read locally and re-read
  /// often: a cable plugged in has to show up long before the 30 s service
  /// poll, and a laptop can have a cable and Wi-Fi up at once - which link the
  /// session rides on is the first thing to check when it comes and goes.
  uplink: LinkKind,
  next_uplink: Instant,
  state: State,
  info: Info,
  logs: Vec<Line>,
  /// The failure the red banner shows. Stored as a kind rather than as words,
  /// so the banner is re-worded by a language switch instead of keeping the
  /// language it arrived in.
  last_error: Option<Failure>,
  /// Height the status banner occupies in this frame (0 when none is shown).
  /// The window grows by exactly this much, so the log pane keeps its size
  /// instead of being covered or squeezed.
  banner_height: f32,
  /// Extra height last sent to the viewport; keeps the resize command from
  /// being emitted on every frame.
  window_extra: f32,
  /// The missing-service warning is already in the log; keeps the two-second
  /// reconnect cadence from repeating it.
  service_warned: bool,
  /// Protocol the core answered with when it refused this build. `Some` means
  /// a core is running but cannot serve this window until it is restarted.
  core_stale: Option<u8>,
  /// A core answered on the current link. An open socket alone does not count:
  /// one that speaks another protocol is refused a moment after connecting,
  /// and the banner must not flicker for that.
  core_answered: bool,
  cfg: Config,
  lang: Lang,
  /// Whether this link has been told the window's language. `App::new` opens a
  /// link before the object exists, so the first connection never passed
  /// through the "new link" path - and the core, which words the messages it
  /// generates itself, kept its own setting (the machine one) instead.
  lang_sent: bool,
  /// Lowest level rendered in the log pane; older lines are kept and simply
  /// hidden, so raising the threshold again brings them back.
  min_level: Level,
  /// The theme setting last pushed to the context (`config.json` `theme`).
  applied_theme: Option<String>,
  autostart: Option<Autostart>,
  autostart_mode: Mode,
  /// This process runs a portable core (`THUNET_STATE_DIR`), so the machine
  /// service is not its business: the settings page hides the autostart modes
  /// (all three register something with systemd / the SCM) and nothing here
  /// may write a registration for a machine that chose portable.
  portable: bool,
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
  /// The gear in front of *Settings*, built on first paint: the egui context
  /// does not exist yet when `App::new` runs.
  gear: Option<egui::TextureHandle>,
  /// The arrow in the settings page's *Back* button, likewise.
  back_arrow: Option<egui::TextureHandle>,
  connect_item: MenuItem,
  disconnect_item: MenuItem,
  show_item: MenuItem,
  hide_item: MenuItem,
  quit_item: MenuItem,
  started: bool,
  /// Keyboard focus as of the previous frame, so the window coming to the
  /// front can trigger exactly one refresh.
  focused: bool,
  /// `--minimized`/`start_minimized`: the first painted frame must not be
  /// left on screen (eframe shows the root window after it unconditionally).
  hide_at_start: bool,
  /// The window has been placed once in this process. eframe is told not to
  /// persist a window position, so the first `logic` pass picks the centre of
  /// the monitor; afterwards the user's own drag is authoritative.
  centered: bool,
  /// The root window is DWM-cloaked until the first real "show" request, so
  /// eframe's forced first show never reaches the compositor.
  cloaked: bool,
  /// Mirrors the window state we asked the platform for. eframe does not track
  /// visibility, so the app keeps its own idea of whether the window is up.
  shown: bool,
  quitting: bool,
  quit_at: Option<Instant>,
  /// Start/Stop clicked on the status page, waiting for its outcome.
  service_request: Option<(ServiceRequest, Instant)>,
  /// An account switch that has no stored credential to fall back on: the user
  /// has to agree to it first, because a wrong password would take the machine
  /// off the network with nothing to sign back in with.
  confirm_switch: Option<Config>,
  /// A switch is in flight; *Apply* stays disabled until the core answers.
  switch_pending: Option<Instant>,
  /// Credentials entered while the machine service was down. They stay out of
  /// `cfg` until the service accepts them, so a failed start leaves the
  /// *Unsaved* markers visible.
  pending_credentials: Option<Config>,
  /// A mode change running on a helper thread. On Windows its core half can
  /// wait for a UAC prompt, which must not block a frame.
  mode_rx: Option<Receiver<(Mode, Result<(), String>)>>,
  /// A Start runs on a helper thread (`systemctl` / `schtasks` can take a
  /// moment); its outcome arrives here rather than blocking a frame. The tag
  /// says which click produced it, since the pending request may already have
  /// been completed by the link appearing or dropping.
  start_tx: Sender<(ServiceRequest, Result<(), String>)>,
  start_rx: Receiver<(ServiceRequest, Result<(), String>)>,
  /// Executable a restart would launch (the file on disk, with the `(deleted)`
  /// suffix removed), plus its stamp when this process started. The stamp is
  /// what tells an upgrade apart from a stale copy at a fixed path.
  exe_path: Option<std::path::PathBuf>,
  exe_stamp: Option<(u64, std::time::SystemTime)>,
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

    // Nothing starts a core at launch: connect if one already answers, and
    // keep retrying from `logic()` (see `ensure_link`) while the status page
    // offers *Start*.
    let link = crate::ipc::open();
    let lang = Lang::from_setting(&cfg.lang);
    let min_level = Level::from_setting(&cfg.log_level);
    let t = lang.texts();

    let connect_item = MenuItem::with_id("connect", t.connect, true, None);
    let disconnect_item = MenuItem::with_id("disconnect", t.disconnect, true, None);
    // Portable is visible in the title and the tray tooltip: the two modes
    // keep separate state, credentials and endpoints, so the user has to be
    // able to tell which one this window is. The application id is left alone
    // - it is what the desktop groups windows by and what the autostart entry
    // points at, and a second id would mean a second icon and a second set of
    // window rules.
    let portable = crate::ipc::portable();
    let app_title = app_name();

    let show_item = MenuItem::with_id("show", t.show_window, true, None);
    let hide_item = MenuItem::with_id("hide", t.hide_window, true, None);
    let quit_item = MenuItem::with_id("quit", t.quit, true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&connect_item, &disconnect_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    // Autostart is deliberately absent: the three modes (off / service /
    // service + tray) live in the settings page, and registering the service
    // needs a UAC prompt that should not be one stray click away.
    let _ = menu.append_items(&[&show_item, &hide_item]);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let tray = TrayIconBuilder::new()
      .with_menu(Box::new(menu.clone()))
      // The menu lives on the right button; a left click brings the window back.
      .with_menu_on_left_click(false)
      .with_tooltip(app_title)
      .with_icon(make_icon(app_fill(State::Offline)))
      .build()
      .ok();
    // Without a tray icon there is no way back into a hidden window, so an
    // invisible start has to be cancelled and the window left on screen.
    let start_hidden = minimized && tray.is_some();
    if minimized && tray.is_none() {
      cloak::set(false);
    }

    let autostart = autostart_entry(cfg.start_minimized);
    let (start_tx, start_rx) = mpsc::channel();
    let exe_path = crate::exe_path();
    let exe_stamp = exe_path.as_deref().and_then(exe_stamp);

    // Surface where settings live; also proves the file exists on first run.
    let mut logs = vec![Line::info(format!(
      "config: {}",
      crate::config::path().display()
    ))];
    // Portable: the core is a thread of this window, so it exists exactly as
    // long as the window does. There is nothing to start, and starting it here
    // is not the "a window must not resurrect a service" rule being broken -
    // that rule is about the machine service, which portable mode never uses.
    if portable {
      match crate::ipc::host_core() {
        Ok(()) => logs.push(Line::info("portable: the core runs in this window")),
        Err(e) => logs.push(Line::warn(format!("portable: {e}"))),
      }
    }
    // The package (or `thunet service install`) provides the system unit; this
    // is a no-op on both platforms and kept for portable builds.
    match autostart.as_ref().map(Autostart::prepare_service) {
      Some(Ok(true)) => logs.push(Line::debug("autostart: service entry installed")),
      Some(Err(e)) => logs.push(Line::warn(format!("autostart: {e}"))),
      _ => {}
    }
    // The tray entry has to name the executable that is installed now: an
    // upgrade may have moved it, and a task pointing at the old file would
    // simply fail at the next logon. Rewriting is a no-op when nothing is
    // registered. A portable run leaves the entry alone: it belongs to a
    // machine-mode configuration this process is not part of.
    if !portable && let Some(Err(e)) = autostart.as_ref().map(Autostart::refresh_tray) {
      logs.push(Line::warn(format!("autostart: {e}")));
    }
    // Read the mode after that: the settings page has to show what is actually
    // registered, not what was requested.
    let autostart_mode = autostart.as_ref().map(Autostart::mode).unwrap_or(Mode::Off);

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
      core_cred: None,
      pass_matches: None,
      pass_check_at: None,
      pass_checked: String::new(),
      next_core_query: Instant::now(),
      core_version_warned: false,
      uplink: crate::srun::link_kind(),
      next_uplink: Instant::now(),
      state: State::Offline,
      info: Info::default(),
      logs,
      last_error: None,
      banner_height: 0.0,
      window_extra: 0.0,
      service_warned: false,
      core_stale: None,
      core_answered: false,
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
      // `App::new` opened a link before this object existed, so the first
      // `ensure_link` pass has to publish the language.
      lang_sent: false,
      min_level,
      applied_theme: None,
      autostart,
      autostart_mode,
      portable,
      tray,
      tray_rgb: Some(color_for(State::Offline)),
      gear: None,
      back_arrow: None,
      connect_item,
      disconnect_item,
      show_item,
      hide_item,
      quit_item,
      started: false,
      focused: false,
      hide_at_start: start_hidden,
      centered: false,
      cloaked: start_hidden,
      shown: !start_hidden,
      quitting: false,
      quit_at: None,
      service_request: None,
      confirm_switch: None,
      switch_pending: None,
      pending_credentials: None,
      mode_rx: None,
      start_tx,
      start_rx,
      exe_path,
      exe_stamp,
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

  /// Drop the core link and every fact it reported.
  ///
  /// A core in its own process reports this by closing the pipe; a hosted core
  /// cannot (both halves of the socket live in this process), so
  /// [`Self::ensure_link`] triggers the same cleanup from the hosting side.
  fn forget_core(&mut self, was_machine: bool, stale: Option<u8>) {
    self.push_log(Line::debug("frontend: the core link went away"));
    self.lang_sent = false;
    self.core = None;
    self.core_answered = false;
    self.last_error = None;
    self.state = State::Offline;
    self.info = Info::default();
    if let Some(v) = stale
      && self.core_stale != Some(v)
    {
      self.core_stale = Some(v);
      self.push_log(Line::error(format!(
        "the background service speaks protocol {v} but this build speaks {}; restart the service",
        crate::proto::VERSION
      )));
    }
    if was_machine {
      // A machine core owned the credential; its answers are stale now, and
      // the next one will report them again after reconnecting.
      self.core_cred = None;
      self.pass_matches = None;
      self.pass_checked.clear();
    }
    // A pending *Stop* is only done once the core is really gone.
    if let Some((ServiceRequest::Stop, _)) = self.service_request {
      self.service_request = None;
      // "Not running" is the expected outcome of this click, not a warning:
      // keep the reconnect probe quiet until a core answers again. The flag
      // resets on a successful connect, so a real outage still reports.
      self.service_warned = true;
      let msg = self.t().service_stopped.to_string();
      self.toast_ok(msg);
    }
  }

  /// Reconnect to the background core, without starting one.
  ///
  /// Starting a core is an explicit action — autostart at boot, the status
  /// page's *Start* button, or the CLI. A window that respawned one on its own
  /// would make `systemctl stop` / `sc stop` look broken.
  fn ensure_link(&mut self) {
    // A core this window hosts ends *inside* this process, and the connections
    // it accepted cannot report EOF from there: both halves of the socket stay
    // open until the other side lets go, and the core's read half only lets go
    // after the frontend does. "Stop was requested and nothing is hosted any
    // more" is the same fact seen from the hosting side, so use it instead of
    // waiting for a hang-up that cannot happen.
    if self.portable
      && self.link.is_some()
      && self
        .service_request
        .is_some_and(|(r, _)| r == ServiceRequest::Stop)
      && !crate::ipc::hosted()
    {
      let was_machine = self.link.as_ref().is_some_and(Link::is_machine);
      let stale = self.link.as_ref().and_then(Link::speaks);
      self.link = None;
      self.forget_core(was_machine, stale);
      return;
    }
    if self.link.as_ref().is_some_and(Link::alive) {
      if !self.lang_sent {
        self.send(Cmd::SetLang(self.lang));
        self.lang_sent = true;
      }
      // Nothing to reconnect, but a click that was re-armed because the core
      // had not answered yet is settled here once it has. Returning early
      // skipped that, so a successful Start still tripped the 120 s watchdog.
      self.settle_click();
      return;
    }
    // Deliver whatever the core managed to say before it went away — the last
    // log lines are what explain an exit — and only then drop the dead link.
    self.drain_events();
    // A link that went away means the service did too (stopped, crashed,
    // replaced by an upgrade). Drop the facts it reported, otherwise the status
    // page keeps claiming "running" for a process that is gone, with session
    // rows nothing updates any more.
    let was_machine = self.link.as_ref().is_some_and(Link::is_machine);
    // A core that answered with another protocol is not gone: it keeps
    // serving, so reconnecting cannot help and the page should say what is
    // actually wrong.
    let stale = self.link.as_ref().and_then(Link::speaks);
    if self.link.take().is_some() {
      self.forget_core(was_machine, stale);
    }
    // Nothing about a refused protocol changes by reconnecting: only a
    // restart does, and that resets this delay. A slower loop keeps the log
    // quiet and stops rebuilding a connection that is refused right away.
    let wait = if self.core_stale.is_some() {
      STALE_RETRY
    } else {
      LINK_RETRY
    };
    if Instant::now() < self.next_link_try {
      return;
    }
    self.next_link_try = Instant::now() + wait;
    match crate::ipc::open_result() {
      Ok(link) => {
        self.link = Some(link);
        self.core_answered = false;
        self.last_error = None;
        self.send(Cmd::SetLang(self.lang));
        self.lang_sent = true;
        // Ask who we are talking to on the next pass.
        self.next_core_query = Instant::now();
      }
      Err(reason) => {
        // Nothing listens any more, so an earlier "old core" verdict no longer
        // describes the machine; the ordinary "not running" band takes over.
        self.core_stale = None;
        // Not a login failure, so this belongs in the log rather than in the
        // red banner; the service row says "not running" by itself.
        // Portable: the core was just started in this window, so the first
        // few failed connects only mean the thread has not bound its socket
        // yet. Saying nothing is right - a core that really died stops being
        // `hosted()` and reports as usual.
        if !self.service_warned && !(self.portable && crate::ipc::hosted()) {
          self.service_warned = true;
          self.push_log(Line::warn(format!(
            "the background service is not running: {reason}"
          )));
        }
      }
    }
    self.settle_click();
  }

  /// Complete a click that was waiting for a core to answer.
  ///
  /// Runs whether or not a reconnect just happened: the core may have answered
  /// on the link that was already up, and a pending *Start* re-armed by the
  /// helper's late reply would otherwise sit there until the watchdog fired.
  fn settle_click(&mut self) {
    // Credentials entered while the service was down travel with its start:
    // once a machine core answers, hand them over and turn the pending Apply
    // into the same toast a normal one would show.
    // A core that answered is what lets a pending credential travel; an open
    // link may already be refused for speaking another protocol.
    let machine_ready = self.core_answered && self.link.as_ref().is_some_and(Link::is_machine);
    if machine_ready && let Some(next) = self.pending_credentials.take() {
      // The service accepted the start: this is the point where the edits
      // become the new baseline and the per-field markers clear.
      self.cfg = next;
      self.service_request = None;
      let typed = !self.cfg.password.is_empty();
      let (msg, kind) = self.credentials_toast(typed);
      self.set_toast(msg.to_string(), kind);
      self.send(Cmd::SetCredentials {
        username: self.cfg.username.clone(),
        password: self.cfg.password.clone(),
        remember: self.cfg.remember_password,
        // These were typed in this window, so there is nothing held to seal.
        keep_password: false,
      });
      // The machine core holds it from now on. Keeping a copy here would make
      // the window claim a stored password it cannot verify against the core.
      self.cfg.password.clear();
      self.pass_edit.clear();
      self.pass_matches = None;
      self.pass_checked.clear();
    }
    // A pending *Start* is only done once the core actually answers, and only
    // for the core this click asked for: the machine endpoint is bound by the
    // service identity alone, so a portable core that merely happens to be up
    // must not complete the click. A portable process hosts its own core, so
    // there that *is* the answer. (macOS has no machine service at all.)
    let start_ready = self.core_answered
      && self.link.as_ref().is_some_and(|link| {
        #[cfg(any(windows, target_os = "linux"))]
        {
          link.is_machine() || crate::ipc::portable()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
          let _ = link;
          true
        }
      });
    if self
      .service_request
      .is_some_and(|(r, _)| r == ServiceRequest::Start)
      && start_ready
    {
      self.service_request = None;
      let msg = self.t().service_started.to_string();
      self.toast_ok(msg);
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
    let next = self.edited_config();

    if self.link.as_ref().is_some_and(Link::alive) {
      match self.credential_action(&next) {
        CredentialAction::Store => {}
        CredentialAction::Switch => self.switch_pending = Some(Instant::now()),
        // Without a credential the core could fall back to, a wrong password
        // leaves the machine offline and unreachable; that decision is the
        // user's to make.
        CredentialAction::Confirm => {
          self.confirm_switch = Some(next);
          return;
        }
      }
      self.save_credentials(next, true);
      return;
    }

    // No core answers. When the start below brings up the machine service (see
    // `ipc::core_is_service`), the credential has to travel with it and is
    // delivered once that core answers: the machine core would never read the
    // per-user store. An explicit `--state-dir` asks for portable mode, where
    // the user store is exactly right.
    if crate::ipc::core_is_service() {
      if let Err(e) = next.save_prefs() {
        self.toast_err(e);
        return;
      }
      let first = self.pending_credentials.is_none();
      // The newest fields travel with the in-flight start, but `cfg` stays on
      // the old values until the core actually accepts them; that is what the
      // per-field *Unsaved* markers compare against.
      self.pending_credentials = Some(next);
      if !first {
        return;
      }
      self.service_request = Some((ServiceRequest::Start, Instant::now()));
      self.set_toast(self.t().service_starting.to_string(), ToastKind::Info);
      let tx = self.start_tx.clone();
      // An old core would keep serving the replaced binary; restarting is what
      // makes Apply work after an upgrade none of the two halves finished.
      let restart = self.core_stale.is_some();
      std::thread::spawn(move || {
        let _ = tx.send((ServiceRequest::Start, crate::ipc::start_core(restart)));
      });
      return;
    }

    // Portable install with no core yet: keep the credential in the user store.
    // The next `thunet core` reads it from there.
    self.save_credentials(next, false);
  }

  /// The config a click on *Apply* would store, without touching `cfg` yet.
  fn edited_config(&self) -> Config {
    let mut next = self.cfg.clone();
    next.username = self.user_edit.trim().to_string();
    next.password = self.pass_edit.clone();
    next.remember_password = self.remember;
    next
  }

  /// Would this *Apply* replace the live session, and does that need a question?
  ///
  /// Mirrors the core's own rule: a complete pair that differs from the current
  /// one, while online. The core then logs the session out and tries the new
  /// pair; whether it can fall back to the old one decides if the user is asked
  /// first.
  fn credential_action(&self, next: &Config) -> CredentialAction {
    credential_action(
      self.state == State::Online,
      next.username != self.cfg.username,
      !next.password.is_empty(),
      self.pass_matches == Some(true),
      self.switch_has_fallback(),
    )
  }

  /// Does the core hold a pair it could sign back in with?
  ///
  /// That is the in-memory credential — not the *Remember password* box: the
  /// core may hold a password this session without a sealed copy on disk, and a
  /// sealed copy it cannot open is worth nothing.
  fn switch_has_fallback(&self) -> bool {
    self
      .core_cred
      .as_ref()
      .is_some_and(|c| c.has_password && !c.username.is_empty())
  }

  /// Save the credential and hand it to a live core.
  ///
  /// `owned_by_core` selects the JSON-only path: a core writes the sealed blob
  /// into its own state directory, so the window must not keep a second copy.
  fn save_credentials(&mut self, next: Config, owned_by_core: bool) {
    let saved = if owned_by_core {
      next.save_prefs()
    } else {
      next.save()
    };
    match saved {
      Ok(()) => {
        // Only now do the edits become the new baseline, so a failed save
        // leaves the *Unsaved* markers in place.
        let typed = !next.password.is_empty();
        self.cfg = next;
        // Say what actually happened: unticking the checkbox deletes the stored
        // credential instead of writing one.
        let (msg, kind) = self.credentials_toast(typed);
        self.set_toast(msg.to_string(), kind);
        self.send(Cmd::SetCredentials {
          username: self.cfg.username.clone(),
          password: self.cfg.password.clone(),
          remember: self.cfg.remember_password,
          // The field is empty and the core is the holder: ask it to seal the
          // password it already has instead of failing for lack of plaintext.
          keep_password: owned_by_core
            && self.cfg.remember_password
            && self.cfg.password.is_empty()
            && self.core_cred.as_ref().is_some_and(|c| c.has_password),
        });
        if owned_by_core {
          // The core is the only holder now; the field goes back to empty and
          // the hint explains that the saved value is kept as long as it stays
          // empty. `Ev::Credentials` will confirm it and fill in the state.
          self.cfg.password.clear();
          self.pass_edit.clear();
          self.pass_matches = None;
          self.pass_checked.clear();
        }
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
    self.pending_credentials = None;
    self.toast = None;
    // An account that was refused is only reported until the user takes the
    // edit back; the stored pair is the one that works, so ask the portal again
    // and let that answer clear the banner.
    self.send(Cmd::Refresh { quiet: true });
  }

  /// Ask before an account switch that has nothing to fall back on.
  ///
  /// The switch itself always costs the session; what makes this case worth a
  /// question is that a refused password would leave the machine offline with
  /// no credential to sign back in with.
  fn paint_switch_confirm(&mut self, ctx: &egui::Context) {
    let Some(next) = self.confirm_switch.clone() else {
      return;
    };
    let t = self.t();
    let modal = egui::Modal::new(egui::Id::unique("thunet_switch_confirm")).show(ctx, |ui| {
      ui.set_max_width(340.0);
      ui.label(egui::RichText::new(t.switch_warning_title).strong());
      ui.add_space(6.0);
      ui.label(t.switch_warning_body);
      ui.add_space(10.0);
      ui.horizontal(|ui| {
        if ui.button(t.switch_continue).clicked() {
          Some(true)
        } else if ui.button(t.cancel).clicked() {
          Some(false)
        } else {
          None
        }
      })
      .inner
    });
    // Escape or a click on the backdrop is a cancel.
    let choice = if modal.should_close() {
      Some(false)
    } else {
      modal.inner
    };
    match choice {
      Some(true) => {
        self.confirm_switch = None;
        self.switch_pending = Some(Instant::now());
        self.save_credentials(next, true);
      }
      Some(false) => self.confirm_switch = None,
      None => {}
    }
  }

  fn credentials_toast(&self, typed: bool) -> (&'static str, ToastKind) {
    let t = self.t();
    let held = typed || self.core_cred.as_ref().is_some_and(|c| c.has_password);
    if !self.remember {
      (t.password_cleared, ToastKind::Info)
    } else if !held {
      (t.password_missing, ToastKind::Info)
    } else {
      (t.password_saved, ToastKind::Ok)
    }
  }

  /// Fold what the core says it holds into the form's baseline.
  ///
  /// A machine core is the only holder of the password, so the window takes
  /// the account and the "saved" flag from it instead of from its own
  /// `config.json` (which never contains the password). Fields the user is
  /// currently editing are left alone: overwriting them would silently discard
  /// an edit whenever a periodic `CoreInfo` answer lands.
  fn apply_core_credential(&mut self, state: &CredentialState) {
    let editing_user = self.user_edit.trim() != self.cfg.username;
    let editing_remember = self.remember != self.cfg.remember_password;
    self.cfg.username = state.username.clone();
    self.cfg.remember_password = state.remembered;
    // Never keep a copy: `has_password` says whether the core has one.
    self.cfg.password.clear();
    if !editing_user {
      self.user_edit = state.username.clone();
    }
    if !editing_remember {
      self.remember = state.remembered;
    }
    self.core_cred = Some(state.clone());
  }

  /// Ask the core whether the typed password is the stored one.
  ///
  /// Re-typing a password that is already saved should not leave an *Unsaved*
  /// marker, and the window cannot compare on its own — only the core can open
  /// the sealed blob. The request is debounced and only sent on a change.
  fn check_typed_password(&mut self) {
    if self.pass_edit != self.pass_checked {
      self.pass_matches = None;
      self.pass_check_at = None;
    }
    if self.pass_edit.is_empty() || self.pass_edit == self.pass_checked {
      return;
    }
    if !self.link.as_ref().is_some_and(Link::alive)
      || !self.core_cred.as_ref().is_some_and(|c| c.has_password)
    {
      return;
    }
    let at = *self
      .pass_check_at
      .get_or_insert_with(|| Instant::now() + Duration::from_millis(350));
    if Instant::now() < at {
      return;
    }
    self.pass_check_at = None;
    self.pass_checked = self.pass_edit.clone();
    self.send(Cmd::CheckCredentials {
      username: self.user_edit.trim().to_string(),
      password: self.pass_edit.clone(),
    });
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
  ///
  /// The work happens on a helper thread: on Windows the core half may show a
  /// UAC prompt, so the answer can take as long as the user needs to click.
  fn apply_autostart_mode(&mut self, mode: Mode) {
    // The row is not drawn in portable mode; this keeps a stray call from
    // registering the machine service anyway.
    if self.portable {
      return;
    }
    let Some(autostart) = self.autostart.clone() else {
      self.toast_err(self.t().autostart_failed.to_string());
      return;
    };
    if self.mode_rx.is_some() {
      // One at a time: a second prompt for the same setting helps nobody.
      return;
    }
    // Show the requested state right away; the toast confirms or reverts it.
    self.autostart_mode = mode;
    self.push_log(Line::debug(format!("autostart: requesting {mode:?}")));
    let (tx, rx) = mpsc::channel::<(Mode, Result<(), String>)>();
    std::thread::spawn(move || {
      let result = autostart.set_mode(mode);
      let _ = tx.send((mode, result));
    });
    self.mode_rx = Some(rx);
  }

  /// Deliver the outcome of the last mode change, if it has arrived.
  fn pump_mode_request(&mut self) {
    let Some(rx) = self.mode_rx.as_ref() else {
      return;
    };
    let Ok((mode, result)) = rx.try_recv() else {
      return;
    };
    self.mode_rx = None;
    let t = self.t();
    self.push_log(Line::debug(format!(
      "autostart: {mode:?} returned {result:?}"
    )));
    match result {
      Ok(()) => {
        self.autostart_mode = mode;
        let label = match mode {
          Mode::Off => t.autostart_off,
          Mode::Core => t.autostart_core,
          Mode::All => t.autostart_all,
        };
        self.toast_ok(format!("{}: {label}", t.autostart));
      }
      Err(e) if e == crate::elevate::CANCELLED => {
        // Nothing changed: go back to what the OS actually says.
        self.reload_autostart_mode();
        self.toast_err(t.admin_cancelled.to_string());
      }
      Err(e) if e == crate::elevate::NO_AUTHORIZATION => {
        // Same "nothing changed" outcome, but `pkexec` cannot tell a dismissed
        // dialog from a session that could not show one, so the wording is
        // neutral and names the terminal fallback.
        self.reload_autostart_mode();
        self.push_log(Line::warn(format!("autostart: {e}")));
        self.toast_err(t.admin_unavailable.to_string());
      }
      Err(e) => {
        self.reload_autostart_mode();
        let msg = format!("{}: {e}", t.autostart_failed);
        self.push_log(Line::error(&msg));
        self.toast_err(msg);
      }
    }
  }

  /// Re-read the autostart state from the OS (after a cancelled or failed
  /// change, the radio buttons must not keep the optimistic value).
  fn reload_autostart_mode(&mut self) {
    let mode = self
      .autostart
      .as_ref()
      .map(Autostart::mode)
      .unwrap_or(Mode::Off);
    self.autostart_mode = mode;
  }

  /// "Start minimised" and "start with the system" are independent settings,
  /// so changing one rewrites the auto-launch command line.
  fn apply_start_minimized(&mut self, on: bool) {
    self.cfg.start_minimized = on;
    if let Err(e) = self.cfg.save() {
      self.toast_err(e);
      return;
    }
    self.autostart = autostart_entry(on);
    let mut failure = None;
    if let Some(a) = &self.autostart
      && let Err(e) = a.refresh_tray()
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
    // Undo the Wayland fallback in `hide_window`, which left it minimised.
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
  }

  fn hide_window(&mut self, ctx: &egui::Context) {
    self.shown = false;
    // winit cannot hide a Wayland toplevel ("Not possible on Wayland"), and
    // `run_with` only forces X11 when `DISPLAY` exists. Without this fallback a
    // pure-Wayland session would ignore *Hide window* and the close button and
    // leave the window on screen; minimising is the closest thing the protocol
    // offers.
    if wayland_without_x11() {
      ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
      return;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
  }

  /// Tray *Quit* (and closing the window when there is no tray icon).
  ///
  /// This ends the frontend only, whatever the autostart mode says: the core
  /// owns the session and keeps running. Stopping it is a separate, explicit
  /// action — the status page's *Stop* button or `thunet service stop`.
  /// The service row's *Start* / *Stop* / *Restart* button.
  fn service_button(&mut self, ui: &mut egui::Ui, stale: bool, running: bool, label: &'static str) {
    // One request at a time: the outcome arrives when the link is up (start) or
    // down (stop), which takes a moment. A service that is already running
    // ignores a start, so an incompatible core needs a restart.
    if !ui
      .add_enabled(self.service_request.is_none(), egui::Button::new(label))
      .clicked()
    {
      return;
    }
    let verb = if stale {
      "Restart"
    } else if running {
      "Stop"
    } else {
      "Start"
    };
    self.push_log(Line::debug(format!(
      "frontend: {verb} clicked (service_request={:?})",
      self.service_request
    )));
    if stale {
      // Treated like a start: the toast fires once a usable core answers.
      self.service_request = Some((ServiceRequest::Start, Instant::now()));
      let tx = self.start_tx.clone();
      std::thread::spawn(move || {
        let _ = tx.send((ServiceRequest::Start, crate::ipc::start_core(true)));
      });
    } else if running {
      self.service_request = Some((ServiceRequest::Stop, Instant::now()));
      let machine = self.link.as_ref().map_or(true, Link::is_machine);
      if machine {
        // The machine core belongs to SCM / systemd, so stopping it is an
        // administrator operation. The link drops when the service exits.
        let tx = self.start_tx.clone();
        std::thread::spawn(move || {
          let r =
            crate::autostart::Autostart::service(crate::autostart::ServiceAction::Stop, false)
              .map(|_| ());
          let _ = tx.send((ServiceRequest::Stop, r));
        });
      } else {
        // A portable core is ours: one control command, no UAC.
        self.send(Cmd::Shutdown);
      }
    } else {
      // Registering the start can block, so it happens off the UI thread.
      self.service_request = Some((ServiceRequest::Start, Instant::now()));
      let tx = self.start_tx.clone();
      std::thread::spawn(move || {
        let _ = tx.send((ServiceRequest::Start, crate::ipc::start_core(false)));
      });
    }
  }

  fn quit(&mut self) {
    self.quitting = true;
    self.quit_at = Some(Instant::now());
  }

  /// False where the portal verbs (Connect, Disconnect and the account form)
  /// have nothing to act on. On an 802.1X network the OS owns the session and
  /// the portal entry is only a mirror of it, so a logout there would tear down
  /// something this app never opened - and the account is not used at all.
  fn portal_verbs(&self) -> bool {
    !matches!(self.env, LoginEnv::EnterpriseWifi(_))
  }

  /// Words and colour for the state line, and for the tray icon with them.
  ///
  /// One helper because the two drifted apart: the window showed "online" for
  /// a link that carries no portal session while the tray icon kept its grey.
  fn state_display(&self, t: &Texts) -> (&'static str, [u8; 3]) {
    match self.state {
      State::Online => (t.online, app_fill(State::Online)),
      State::Authenticating => (t.authenticating, app_fill(State::Authenticating)),
      // The portal has no session for this address, but the link does not go
      // through it at all: "Offline" would read as "there is no internet",
      // which is the opposite of what the probe just measured.
      State::Offline if self.env == LoginEnv::NoPortal => {
        (t.online_without_portal, app_fill(State::Online))
      }
      State::Offline => (t.offline, app_fill(State::Offline)),
    }
  }

  /// Replace this process with a fresh launch of the executable on disk.
  ///
  /// Used when the service reports a newer build: this process still runs the
  /// replaced image. The instance guard is released before the spawn so the
  /// successor can become the primary instance; if the spawn fails, this
  /// window keeps running (and takes the guard back) instead of leaving no
  /// window at all.
  fn restart(&mut self) {
    let Some(exe) = self.exe_path.clone() else {
      return;
    };
    let mut cmd = std::process::Command::new(exe);
    if !self.shown {
      // The user hid the window, not the app: come back in the tray.
      cmd.arg("--minimized");
    }
    #[cfg(windows)]
    {
      use std::os::windows::process::CommandExt as _;
      const CREATE_NO_WINDOW: u32 = 0x0800_0000;
      cmd.creation_flags(CREATE_NO_WINDOW);
    }
    crate::single::release();
    match cmd.spawn() {
      Ok(_) => {
        // Skip destructors: the successor owns the guard now, and
        // `Guard::drop` would delete the lock file it just created.
        std::process::exit(0)
      }
      Err(e) => {
        if let Some(guard) = crate::single::acquire() {
          std::mem::forget(guard); // stay the primary instance
        }
        self.push_log(Line::error(format!(
          "restart failed: {e}; staying on the old build"
        )));
      }
    }
  }

  fn handle_menu(&mut self, ctx: &egui::Context) {
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
      match ev.id().0.as_str() {
        "connect" => self.send(Cmd::Connect),
        "disconnect" => self.send(Cmd::Disconnect),
        "quit" => self.quit(),
        "show" => self.show_window(ctx),
        "hide" => self.hide_window(ctx),
        _ => {}
      }
    }
  }

  fn drain_events(&mut self) {
    while let Some(ev) = self.next_event() {
      if !self.core_answered {
        // A real answer, not just an open socket: a core that speaks another
        // protocol closes the link right after connecting, and counting that
        // as "running" is what made the banner blink.
        self.core_answered = true;
        self.core_stale = None;
        self.service_warned = false;
        let kind = if self.link.as_ref().is_some_and(Link::is_machine) {
          "service"
        } else {
          "portable"
        };
        self.push_log(Line::info(format!("connected to the {kind} core")));
      }
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
          // A newer build on the service side means the package was upgraded
          // and this process still runs the replaced file; relaunch it. The
          // checks in `should_restart` keep a manual stop/start (same build)
          // and a stale copy (unchanged file) from restarting forever.
          let file_changed = self.exe_path.as_deref().and_then(exe_stamp) != self.exe_stamp;
          if !self.quitting && should_restart(info.build_epoch, own_build_epoch(), file_changed) {
            self.push_log(Line::info(
              "service runs a newer build; restarting this window to match",
            ));
            self.restart();
          }
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
          let cred = info.credential.clone();
          self.apply_core_credential(&cred);
          self.core = Some(info);
        }
        Ev::Credentials(state) => {
          // The answer to any Apply, including the outcome of a switch.
          self.switch_pending = None;
          // A comparison answer and/or a fresh baseline. `matches` is only
          // present on a `CheckCredentials` reply.
          if let Some(m) = state.matches {
            self.pass_matches = Some(m);
          }
          self.apply_core_credential(&state);
        }
        // Answers to the CLI's one-off questions; the window has no use for them.
        Ev::Status(_) | Ev::AcId(_) => {}
        // An installer is about to replace the executable: end here instead of
        // hiding, so the image is gone before the file is (see `quit`).
        Ev::Quit => self.quit(),
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
    let (label, rgb) = self.state_display(t);

    egui::Frame::new()
      .fill(ui.visuals().panel_fill)
      // The bottom margin is smaller than the other three on purpose: the only
      // thing down there is the build stamp, and the footer cannot go closer to
      // the window edge than these margins allow.
      .inner_margin(egui::Margin {
        left: 14,
        right: 14,
        top: 14,
        bottom: 8,
      })
      .show(ui, |ui| {
        // Bands take their own space at the top of the page instead of lying
        // over the rows below: the text stays centred, the page below keeps its
        // layout, and `sync_window_height` grows the window by exactly this
        // height so the log pane is not squeezed.
        self.banner_height = 0.0;
        // Both halves of "is there something to log in with": the core owns the
        // credential, and its last answer is fresher than the 30 s `CoreInfo`.
        let credential = self
          .core_cred
          .as_ref()
          .or_else(|| self.core.as_ref().map(|c| &c.credential));
        let auto_connect = self.core.as_ref().is_some_and(|c| c.auto_connect);
        let credentials_ready =
          credential.is_some_and(|c| c.has_password && !c.username.is_empty());
        // Portable mode hosts the core, so "not running" or "an old core" are
        // not things the user can act on: at most the link has not been made
        // yet, and the status row says where the core lives. Reporting it as a
        // fault would flash a red band on every start.
        let answered = self.core_answered || self.portable;
        let banners = page_banners(
          self.page,
          self.state,
          answered,
          self.core_stale,
          self.env.clone(),
          self.last_error.as_ref(),
          auto_connect,
          credentials_ready,
          t,
        );
        if !banners.is_empty() {
          let mut height = 0.0;
          for (i, (tone, text)) in banners.iter().enumerate() {
            if i > 0 {
              ui.add_space(BANNER_GAP);
              height += BANNER_GAP;
            }
            height += banner(ui, text.as_str(), *tone);
          }
          ui.add_space(BANNER_GAP);
          self.banner_height = height + BANNER_GAP;
        }

        ui.horizontal(|ui| {
          if self.page == Page::Status {
            ui.label(
              egui::RichText::new("●")
                .color(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                .size(18.0),
            );
            ui.label(egui::RichText::new(label).size(18.0).strong());
          } else {
            // The same gear as the button that opened this page, so the two
            // read as the same place.
            let color = ui.visuals().text_color();
            let icon = heading_icon(&mut self.gear, ui.ctx(), "thunet_gear", gear_image, color);
            ui.add(icon);
            ui.label(egui::RichText::new(t.settings).size(18.0).strong());
          }
          ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Icon only: the label moved to the tooltip, which keeps the header
            // to one line in both languages.
            let (texture, label) = if self.page == Page::Status {
              let gear = icon_texture(&mut self.gear, ui.ctx(), "thunet_gear", gear_image);
              (gear.id(), t.settings)
            } else {
              let arrow = icon_texture(
                &mut self.back_arrow,
                ui.ctx(),
                "thunet_back",
                back_arrow_image,
              );
              (arrow.id(), t.back)
            };
            if ui.add(icon_button(texture)).on_hover_text(label).clicked() {
              self.page = match self.page {
                Page::Status => Page::Settings,
                Page::Settings => Page::Status,
              };
            }
          });
        });

        ui.add_space(8.0);

        if self.page == Page::Status {
          // Only the portal names an account, and it does so only when this
          // address has a session: the `not_online_error` answer carries
          // `client_ip` and nothing else. Falling back to the configured name
          // would claim a session the portal never confirmed - that name has
          // its place on the settings page, where it is edited.
          let account = if self.info.name.is_empty() {
            "—".to_string()
          } else {
            self.info.name.clone()
          };
          egui::Grid::new("info")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
              ui.label(t.account);
              ui.label(egui::RichText::new(account).monospace());
              ui.end_row();
              ui.label(t.link);
              ui.label(egui::RichText::new(self.uplink.text(t)).monospace());
              ui.end_row();
              ui.label(t.ipv4);
              if address_cell(ui, &self.info.ip4, t) {
                ui.ctx().copy_text(self.info.ip4.clone());
                self.toast_ok(t.copied.to_string());
              }
              ui.end_row();
              ui.label(t.ipv6);
              if address_cell(ui, &self.info.ip6, t) {
                ui.ctx().copy_text(self.info.ip6.clone());
                self.toast_ok(t.copied.to_string());
              }
              ui.end_row();
              ui.label(t.session_traffic);
              ui.label(format!(
                "↓ {}   ↑ {}",
                human(self.info.bytes_in),
                human(self.info.bytes_out)
              ));
              ui.end_row();
              // The core is a separate process in service mode, so the row says
              // whether one is answering and which version it runs, and the
              // button beside it starts or stops it: like `thunet service stop`
              // it never closes this window. In portable mode the core is a
              // thread of this window - there is nothing to start, stop or
              // restart - so the row only states where it lives.
              let service = self
                .core
                .as_ref()
                .map(|c| (c.version.clone(), c.uptime_secs));
              // A core that answered counts as running even before its first
              // `CoreInfo` lands, and so does one that answered with another
              // protocol: it is alive, it just cannot serve this build.
              let stale = self.core_stale.is_some();
              let running = service.is_some() || self.core_answered || stale;
              if self.portable {
                ui.label(t.core_row);
                ui.horizontal(|ui| {
                  let mut text = egui::RichText::new(t.core_in_window);
                  if running {
                    text = text.color(state_color(State::Online));
                  }
                  ui.label(text);
                  if let Some((version, _)) = &service {
                    ui.label(format!("v{version}"));
                  }
                });
                ui.end_row();
              } else {
                ui.label(t.service);
                // Same shape as the address rows above: the button is laid out
                // first in a right-to-left row, so it keeps the column's right
                // edge, and the status text takes the rest from the left. The
                // children stay inside the cell because the row is given
                // exactly the width the cell offers.
                let action = match (stale, running) {
                  (true, _) => t.service_restart,
                  (false, true) => t.service_stop,
                  (false, false) => t.service_start,
                };
                ui.horizontal(|ui| {
                  let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                  ui.allocate_ui_with_layout(
                    size,
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                      self.service_button(ui, stale, running, action);
                      ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        service_status(ui, service.as_ref(), running, t);
                      });
                    },
                  );
                });
                ui.end_row();
              }
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
              .add_enabled(
                online && self.portal_verbs(),
                egui::Button::new(t.disconnect),
              )
              .on_disabled_hover_text(
                self
                  .env
                  .blocked_reason(t)
                  .unwrap_or_else(|| t.disconnect.into()),
              )
              .clicked()
            {
              self.send(Cmd::Disconnect);
            }
            if ui.button(t.refresh).clicked() {
              self.send(Cmd::Refresh { quiet: false });
            }
          });
        } else {
          // Denser spacing: the settings page has to fit the fixed window.
          ui.spacing_mut().item_spacing.y = 4.0;

          // Language, log level and theme lead the page: they are the settings
          // people look for first.
          let mut choice = match self.cfg.lang.as_str() {
            "zh" => 1,
            "en" => 2,
            _ => 0,
          };
          let before_lang = choice;
          let before_level = self.min_level;
          let mut theme = match self.cfg.theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
          };
          let before_theme = theme;
          // All three combos share one row, and all three are given the same
          // width: the labels differ from language to language, but a common
          // combo width is what keeps the row inside the fixed window width and
          // the three controls the same size.
          ui.horizontal(|ui| {
            ui.label(t.language);
            egui::ComboBox::from_id_salt("lang")
              .width(COMBO_WIDTH)
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
            ui.label(t.log_level);
            egui::ComboBox::from_id_salt("log-level")
              .width(COMBO_WIDTH)
              .selected_text(self.min_level.tag())
              .show_ui(ui, |ui| {
                for level in Level::CHOICES {
                  ui.selectable_value(&mut self.min_level, level, level.tag());
                }
              });
            ui.label(t.theme);
            egui::ComboBox::from_id_salt("theme")
              .width(COMBO_WIDTH)
              .selected_text(match theme {
                1 => t.theme_light,
                2 => t.theme_dark,
                _ => t.theme_auto,
              })
              .show_ui(ui, |ui| {
                ui.selectable_value(&mut theme, 0, t.theme_auto);
                ui.selectable_value(&mut theme, 1, t.theme_light);
                ui.selectable_value(&mut theme, 2, t.theme_dark);
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
          if theme != before_theme {
            self.cfg.theme = ["auto", "light", "dark"][theme].to_string();
            match self.cfg.save() {
              Ok(()) => {
                let label = [t.theme_auto, t.theme_light, t.theme_dark][theme];
                self.set_toast(format!("{}: {label}", t.theme), ToastKind::Info);
              }
              Err(e) => self.toast_err(e),
            }
          }

          // A rule under the row that only changes how this window renders, so
          // the settings that decide what runs at logon start clearly below it.
          ui.separator();

          ui.add_space(2.0);
          // Three modes, not two checkboxes: "tray without core" is not worth
          // offering, because a tray icon with no session behind it can only
          // report "not running". With the short labels from `i18n.rs` all
          // three fit on the label's row, which is what keeps the page inside
          // the fixed window height.
          // Every mode here registers something with the machine's service
          // manager, so a portable process must not offer them at all: it has
          // no service, and choosing one is exactly how a portable run would
          // leave a registration behind on a machine that never asked for one.
          if self.portable {
            ui.label(t.portable_autostart);
          } else {
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
          }

          // Both are startup behaviour, so they share one row: the settings page
          // has to fit the fixed window height. *Start minimised* is only
          // meaningful when the tray is part of the autostart mode.
          let mut start_min = self.cfg.start_minimized;
          let tray_starts = !self.portable && self.autostart_mode != Mode::Core;
          let mut auto_conn = self.cfg.auto_connect;
          ui.horizontal(|ui| {
            if ui
              .add_enabled(
                tray_starts,
                egui::Checkbox::new(&mut start_min, t.start_minimized),
              )
              .changed()
            {
              self.apply_start_minimized(start_min);
            }
            if ui.checkbox(&mut auto_conn, t.auto_connect).changed() {
              self.cfg.auto_connect = auto_conn;
              self.send(Cmd::SetAutoConnect(auto_conn));
              match self.cfg.save() {
                Ok(()) => self.toast_toggle(t.auto_connect, auto_conn),
                Err(e) => self.toast_err(e),
              }
            }
          });
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
          // The rule under the shortcut button is unconditional: it is the line
          // that separates "settings that only touch this window" from the
          // account block, and before it appeared only when the account form was
          // live.
          ui.separator();
          // On an 802.1X network the portal account is never used, so the form is
          // inert: an edit could not be put to work, and a pair that cannot be
          // tried would be stored unverified.
          let credentials_live = self.portal_verbs();
          if !credentials_live && let Some(why) = self.env.blocked_reason(t) {
            ui.label(
              egui::RichText::new(why)
                .small()
                .color(ui.visuals().weak_text_color()),
            );
          }
          // Comment on each field separately: the marker belongs next to the
          // input that actually differs from what is stored.
          let core_password = self.core_cred.as_ref().is_some_and(|c| c.has_password);
          // Only trust the comparison for the text that was actually sent: as
          // soon as the field changes, the previous answer is irrelevant.
          let match_hint = (self.pass_checked == self.pass_edit)
            .then_some(self.pass_matches)
            .flatten();
          let stored = CredentialEdits::new(
            &self.user_edit,
            &self.pass_edit,
            self.remember,
            &self.cfg,
            self.core_cred.as_ref(),
            match_hint,
          );
          ui.horizontal(|ui| {
            let label_w = field_label_width(ui, t);
            field_label(ui, t.account, label_w);
            ui.add_enabled(
              credentials_live,
              egui::TextEdit::singleline(&mut self.user_edit).desired_width(FIELD_WIDTH),
            );
            if stored.username {
              unsaved_marker(ui, t);
            }
          });
          ui.horizontal(|ui| {
            let label_w = field_label_width(ui, t);
            field_label(ui, t.password, label_w);
            let keep_hint = core_password && self.pass_edit.is_empty();
            let mut input = egui::TextEdit::singleline(&mut self.pass_edit)
              .password(true)
              .desired_width(FIELD_WIDTH);
            if keep_hint {
              // The core holds a password the window cannot show; an empty
              // field means "keep it" rather than "no password".
              input = input.hint_text(t.password_keep);
            }
            ui.add_enabled(credentials_live, input);
            if stored.password {
              unsaved_marker(ui, t);
            }
          });
          ui.horizontal(|ui| {
            if ui
              .add_enabled(
                credentials_live,
                egui::Checkbox::new(&mut self.remember, t.remember_password),
              )
              .on_hover_text(t.remember_password_hint)
              .changed()
            {
              self.toast = None;
            }
            // This row is the only one that does not save as you type, so the
            // button shows when there is anything left to apply. A switch in
            // flight owns the session: a second click would start another one.
            // The password the core holds can be remembered without retyping
            // it, but only while the form still names the account it belongs
            // to - the core refuses to pair a held password with another user.
            let held_here = self.core_cred.as_ref().is_some_and(|cred| {
              cred.has_password && cred.username.trim() == self.user_edit.trim()
            });
            let may_apply = can_apply(
              self.remember,
              &self.user_edit,
              &self.pass_edit,
              held_here,
              stored.any(),
            );
            let mut apply = egui::Button::new(t.apply);
            if stored.any() {
              apply = apply.fill(ui.visuals().selection.bg_fill);
            }
            let enabled = credentials_live && self.switch_pending.is_none() && may_apply;
            let response = ui
              .add_enabled(enabled, apply)
              .on_disabled_hover_text(if stored.any() {
                t.password_required
              } else {
                t.nothing_to_apply
              });
            if response.clicked() {
              self.apply_credentials();
            }
            // Cancelling an edit only makes sense while a password is actually
            // remembered: without one there is nothing to restore.
            if ui
              .add_enabled(
                credentials_live && self.remember,
                egui::Button::new(t.revert),
              )
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

        // The settings page ends with a build stamp pinned to the bottom edge.
        // Both distances are spelled out instead of falling out of the leftover
        // space: `RULE_GAP` is how far the rule sits above the stamp, and
        // `BOTTOM_MARGIN` keeps the stamp off the edge so it lines up with the
        // page's own side margin rather than touching it.
        if self.page == Page::Settings {
          /// Height the separator reserves; the line is drawn inside it.
          const RULE_H: f32 = 2.0;
          /// Gap between the rule and the stamp under it.
          const RULE_GAP: f32 = 5.0;
          // The stamp row is laid out at exactly the text height instead of the
          // interactive height `ui.horizontal` would use: that one is ~10 px
          // taller than the text and centres it, which pushed the stamp back up
          // by half the difference.
          let text_h = ui.text_style_height(&egui::TextStyle::Small);
          let strip = RULE_H + RULE_GAP + text_h;
          let rest = ui.available_height() - strip;
          if rest > 0.0 {
            ui.add_space(rest);
          }
          ui.vertical(|ui| {
            // No automatic gaps inside the strip: the rule and the stamp are the
            // only two things in it, and they are placed by the arithmetic above.
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(egui::Separator::default().spacing(RULE_H));
            ui.add_space(RULE_GAP);
            // Version on the left, build time on the right: they answer
            // different questions, so the two ends of the strip read better
            // than one run-on line.
            ui.allocate_ui_with_layout(
              egui::vec2(ui.available_width(), text_h),
              egui::Layout::left_to_right(egui::Align::Center),
              |ui| {
                ui.label(egui::RichText::new(build_version()).small().weak());
                // Source link: the octocat itself is clickable instead of being a
                // `Hyperlink`, whose label is painted in the link colour (the
                // solid glyph then looks like a blue blob) and which is also
                // selectable text (a click leaves a blue selection over it).
                // The glyph is one of egui's bundled icons, so no image asset is
                // needed; hovering shows the URL and the pointing-hand cursor.
                let mark_color = ui.visuals().weak_text_color();
                let mark = ui
                  .add(
                    egui::Label::new(
                      egui::RichText::new(egui::special_emojis::GITHUB)
                        .small()
                        .color(mark_color),
                    )
                    .selectable(false)
                    .sense(egui::Sense::click()),
                  )
                  .on_hover_text(env!("CARGO_PKG_REPOSITORY"));
                if mark.hovered() {
                  ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if mark.clicked() {
                  ui.ctx().open_url(egui::OpenUrl {
                    url: env!("CARGO_PKG_REPOSITORY").to_owned(),
                    new_tab: true,
                  });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                  ui.label(egui::RichText::new(build_time_label(t)).small().weak());
                });
              },
            );
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

/// One copyable address row: the address against the left edge of the value
/// column, the *Copy* button against its right edge.
///
/// The button is laid out **first**, in a right-to-left row, so the layout
/// itself puts it on the column's right edge; the address then takes whatever
/// is left and elides there. Placing it by arithmetic instead (`available -
/// <button width>`) left it a few pixels short of the edge and drifted with the
/// grid's column width, and `add_sized` centred the address because that helper
/// lays its child out with `centered_and_justified`.
///
/// Returns the address label and the button, so a test can pin the alignment.
fn address_row(ui: &mut egui::Ui, value: &str, t: &Texts) -> (egui::Response, egui::Response) {
  ui.horizontal(|ui| {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(
      size,
      egui::Layout::right_to_left(egui::Align::Center),
      |ui| {
        let button = ui.add_enabled(!value.is_empty(), egui::Button::new(t.copy));
        // Laid out left-to-right in the space that is left: the enclosing
        // right-to-left layout would otherwise align the text against the
        // button instead of against the column's left edge.
        let label = ui
          .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            // `truncate` elides at the width that is left and, by default
            // (`show_tooltip_when_elided`), shows the whole address on hover;
            // an explicit `on_hover_text` would stack a second tooltip on top
            // of that one for the address that is cut off (the IPv6 one).
            ui.add(
              egui::Label::new(egui::RichText::new(dash(value)).monospace())
                .truncate()
                .sense(egui::Sense::hover()),
            )
          })
          .inner;
        (label, button)
      },
    )
    .inner
  })
  .inner
}

/// Returns true when the *Copy* button was clicked.
fn address_cell(ui: &mut egui::Ui, value: &str, t: &Texts) -> bool {
  address_row(ui, value, t).1.clicked()
}

/// The service row's status: "running v0.1.4 · 1m", or why nothing answers.
fn service_status(ui: &mut egui::Ui, service: Option<&(String, u64)>, running: bool, t: &Texts) {
  match service {
    Some((version, uptime)) => {
      ui.label(egui::RichText::new(t.service_running).color(state_color(State::Online)));
      let label = format!("v{version} · {}", human_uptime(*uptime));
      let text = egui::RichText::new(label).monospace();
      ui.label(if version == env!("CARGO_PKG_VERSION") {
        text.weak()
      } else {
        text.color(state_color(State::Authenticating))
      });
    }
    None if running => {
      ui.label(egui::RichText::new(t.service_running).color(state_color(State::Online)));
    }
    None => {
      ui.label(egui::RichText::new(t.service_gone).color(state_color(State::Offline)));
    }
  }
}

/// What *Apply* does with the edited pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialAction {
  /// No live session is replaced: store the pair and let the core try it.
  Store,
  /// Replace the session; the core signs the previous pair back in if the new
  /// one is refused.
  Switch,
  /// Replace the session, but ask first: a refusal would leave nothing to sign
  /// back in with.
  Confirm,
}

/// Decide that from the four facts that matter.
fn credential_action(
  online: bool,
  username_changed: bool,
  password_typed: bool,
  password_matches: bool,
  has_fallback: bool,
) -> CredentialAction {
  // An empty password field means "keep the stored one", so nothing about the
  // pair changed; a typed value the core already confirmed as the stored one is
  // not a change either. Offline there is no session to replace.
  if !online || !password_typed || (!username_changed && password_matches) {
    CredentialAction::Store
  } else if has_fallback {
    CredentialAction::Switch
  } else {
    CredentialAction::Confirm
  }
}

/// May *Apply* do anything with this form?
///
/// *Remember password* together with an empty field has no meaning: the core
/// keeps what is stored (the field says "leave blank to keep it"), so the only
/// thing an Apply could do is the one thing that is not allowed — change the
/// account without retyping the password. An empty field is fine when `held`
/// says the core holds a password for exactly this account: then the request
/// only asks to seal that one. Unticking the box is the explicit clear and
/// needs neither field; with nothing changed there is nothing to do.
fn can_apply(remember: bool, username: &str, password: &str, held: bool, changed: bool) -> bool {
  changed && (!remember || (!username.trim().is_empty() && (!password.is_empty() || held)))
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
  fn new(
    user_edit: &str,
    pass_edit: &str,
    remember: bool,
    cfg: &Config,
    core: Option<&CredentialState>,
    pass_matches: Option<bool>,
  ) -> Self {
    let password = match core {
      // A machine core owns the password, so the window has no plaintext to
      // compare against. An empty field is "leave the saved one alone"; a
      // typed value is an edit unless the core said it equals the stored pair.
      Some(state) if state.has_password => !pass_edit.is_empty() && pass_matches != Some(true),
      Some(_) => !pass_edit.is_empty(),
      None => pass_edit != cfg.password,
    };
    Self {
      // Applying trims the account, so a stray space is not a change.
      username: user_edit.trim() != cfg.username,
      password,
      remember: remember != cfg.remember_password,
    }
  }

  fn any(self) -> bool {
    self.username || self.password || self.remember
  }
}

/// Width of the three presentation combos (*Language*, *Log*, *Skin*).
///
/// One width for all three: they share a row, and the row has to fit the fixed
/// window width in every language.
const COMBO_WIDTH: f32 = 78.0;

/// Width of the *Account* / *Password* input boxes.
///
/// Wide enough for the longest placeholder — "Saved — leave blank to keep it" —
/// at the default font, and for the account a portal user actually types, while
/// still leaving room on the same row for the *Unsaved* marker.
const FIELD_WIDTH: f32 = 250.0;

/// Width of the label column in front of those inputs.
///
/// Measured from the two labels in the current language instead of fixed:
/// "Password" is the widest English label, while "账号" and "密码" are two
/// characters each, and a column sized for English leaves a wide gap in
/// Chinese. Both rows take the same number, which is what keeps the two inputs
/// on the same x in every language.
fn field_label_width(ui: &egui::Ui, t: &Texts) -> f32 {
  let font = egui::TextStyle::Body.resolve(ui.style());
  [t.account, t.password]
    .iter()
    .map(|text| {
      ui.fonts_mut(|f| {
        f.layout_no_wrap((*text).to_owned(), font.clone(), egui::Color32::WHITE)
          .size()
          .x
      })
    })
    .fold(0.0_f32, f32::max)
    .ceil()
}

/// That label, drawn flush left in the shared column.
///
/// A plain `ui.label` would make each row's column as wide as its own text, so
/// the two inputs would start at different x; `add_sized` reserves the width
/// but centres the text inside it.
fn field_label(ui: &mut egui::Ui, text: &str, width: f32) {
  let (rect, _) = ui.allocate_exact_size(
    egui::vec2(width, ui.spacing().interact_size.y),
    egui::Sense::hover(),
  );
  ui.put(rect, egui::Label::new(text));
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
  /// Where the current primary drag started, if one is in progress.
  drag_origin: Option<egui::Pos2>,
  /// A drag selected label text. A plain click only leaves a caret, which
  /// egui also reports as `has_selection` — redirecting a right click then
  /// would show "copied" without a selection.
  selected: bool,
}

impl Default for LogCopyClick {
  fn default() -> Self {
    Self {
      copy_pending: false,
      swallow_release: false,
      log_rect: egui::Rect::NOTHING,
      drag_origin: None,
      selected: false,
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
    for event in &input.events {
      match event {
        egui::Event::PointerButton {
          pos,
          button: egui::PointerButton::Primary,
          pressed: true,
          ..
        } => {
          // A press starts a fresh gesture; only moving far enough while it is
          // down selects anything. This keeps a bare click (which egui counts
          // as a selection because it leaves a caret) from hijacking the next
          // right click.
          self.selected = false;
          self.drag_origin = Some(*pos);
        }
        egui::Event::PointerMoved(pos) => {
          if self
            .drag_origin
            .is_some_and(|start| start.distance(*pos) > 3.0)
          {
            self.selected = true;
          }
        }
        egui::Event::PointerButton {
          button: egui::PointerButton::Primary,
          pressed: false,
          ..
        } => self.drag_origin = None,
        _ => {}
      }
    }
    if !self.selected || !has_label_selection(ctx) {
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

  /// The address rows must hug both edges of the value column: the address
  /// against the left one, the *Copy* button against the right one. An earlier
  /// version centred the address (`add_sized` lays its child out with
  /// `centered_and_justified`), and another left the button short of the edge
  /// by subtracting its width from `available_width()`.
  #[test]
  fn address_rows_hug_both_edges_of_the_value_column() {
    use egui::{Rect, pos2, vec2};

    let t = crate::i18n::Lang::En.texts();
    let ctx = egui::Context::default();
    let mut rows = Vec::new();
    let mut column = Rect::NOTHING;
    for _ in 0..3 {
      let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 300.0))),
        ..Default::default()
      };
      let mut out = ctx.run_ui(input, |ui| {
        egui::Frame::new()
          .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 14,
            bottom: 8,
          })
          .show(ui, |ui| {
            // egui may lay the ui out twice per frame (a sizing pass first);
            // only the last pass is the one that is painted.
            rows.clear();
            egui::Grid::new("info")
              .num_columns(2)
              .spacing([12.0, 6.0])
              .show(ui, |ui| {
                ui.label(t.account);
                ui.label(egui::RichText::new("alice").monospace());
                ui.end_row();
                ui.label(t.ipv4);
                column = ui.max_rect();
                let left = ui.cursor().min.x;
                rows.push((left, address_row(ui, "192.0.2.10", t)));
                ui.end_row();
                ui.label(t.ipv6);
                let left = ui.cursor().min.x;
                rows.push((
                  left,
                  address_row(ui, "2001:db8:5:8001:f49d:103e:ffff:ffff", t),
                ));
                ui.end_row();
              });
          });
      });
      out.textures_delta.clear();
    }

    let [(start4, (label4, button4)), (start6, (label6, button6))] = &rows[..] else {
      panic!("two rows expected, got {}", rows.len());
    };
    let (start4, start6) = (*start4, *start6);
    // Both buttons end on the same edge, which is the column's.
    assert!(
      (button4.rect.right() - button6.rect.right()).abs() < 0.5,
      "{:?} vs {:?}",
      button4.rect,
      button6.rect
    );
    assert!(
      (button4.rect.right() - column.right()).abs() < 0.5,
      "the button must reach the column's right edge: {:?} in {:?}",
      button4.rect,
      column
    );
    // Both addresses start at the left edge of the column, and the long one
    // elides instead of running into the button.
    for (start, label) in [(start4, label4.rect), (start6, label6.rect)] {
      assert!(
        (label.left() - start).abs() < 0.5,
        "{label:?} must start at the column's left edge {start}"
      );
    }
    assert!(
      label6.rect.right() <= button6.rect.left() + 0.5,
      "the elided address must stop before the button: {:?} vs {:?}",
      label6.rect,
      button6.rect
    );
  }

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
  fn right_click_after_a_bare_click_keeps_the_menu() {
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
            "a single line".to_string(),
            egui::Color32::WHITE,
          ));
        });
        LogCopyClick::set_log_rect(ui.ctx(), pane.response.rect);
      });
      out.textures_delta.clear();
    };
    let button = |pressed, button, pos: Pos2| Event::PointerButton {
      pos,
      button,
      pressed,
      modifiers: Default::default(),
    };

    let _ = run(&ctx, vec![]);
    let at = pos2(9.0, 10.0);
    let _ = run(&ctx, vec![Event::PointerMoved(at)]);
    // A click without dragging leaves a caret; egui counts that as a label
    // selection, but there is nothing to copy.
    let _ = run(&ctx, vec![button(true, PointerButton::Primary, at)]);
    let _ = run(&ctx, vec![button(false, PointerButton::Primary, at)]);
    let _ = run(&ctx, vec![button(true, PointerButton::Secondary, at)]);
    assert!(
      !LogCopyClick::take_copy(&ctx),
      "a right click without a selection must reach the context menu"
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
    let same = CredentialEdits::new("alice", "pw", true, &cfg, None, None);
    assert_eq!(same, CredentialEdits::default(), "unchanged");
    assert!(!same.any());
    // A stray space is trimmed away when applying, so it is not a change.
    assert!(!CredentialEdits::new("  alice  ", "pw", true, &cfg, None, None).username);

    let user = CredentialEdits::new("bob", "pw", true, &cfg, None, None);
    assert!(user.username && !user.password && !user.remember);
    let pass = CredentialEdits::new("alice", "pw2", true, &cfg, None, None);
    assert!(!pass.username && pass.password && !pass.remember);
    let remember = CredentialEdits::new("alice", "pw", false, &cfg, None, None);
    assert!(!remember.username && !remember.password && remember.remember);
    assert!(remember.any());
  }

  #[test]
  fn a_core_owned_password_is_only_unsaved_when_it_was_typed() {
    // The window never receives the core's password, so the form baseline is
    // `has_password = true` plus an empty field.
    let cfg = Config {
      username: "alice".into(),
      remember_password: true,
      ..Config::default()
    };
    let core = CredentialState {
      username: "alice".into(),
      has_password: true,
      remembered: true,
      matches: None,
    };
    let empty = CredentialEdits::new("alice", "", true, &cfg, Some(&core), None);
    assert_eq!(
      empty,
      CredentialEdits::default(),
      "an empty field is not an edit"
    );

    // Typing the value that is already stored is not an edit once the core
    // confirmed it; typing anything else is.
    let same = CredentialEdits::new("alice", "pw", true, &cfg, Some(&core), Some(true));
    assert!(!same.password, "the core said it matches");
    let other = CredentialEdits::new("alice", "pw", true, &cfg, Some(&core), Some(false));
    assert!(other.password, "the core said it differs");

    // With nothing stored, any password is a change.
    let none = CredentialState::default();
    let typed = CredentialEdits::new("alice", "pw", false, &cfg, Some(&none), None);
    assert!(typed.password);
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

  /// A long error must not turn the band — and the window grown to fit it —
  /// into a wall of text: it stops after `BANNER_ROWS` lines with an ellipsis.
  #[test]
  fn a_long_banner_is_capped() {
    let ctx = egui::Context::default();
    let height_of = |text: &str| -> f32 {
      let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
          egui::Pos2::ZERO,
          egui::vec2(420.0, 300.0),
        )),
        ..Default::default()
      };
      let mut height = 0.0;
      let mut out = ctx.run_ui(input, |ui| {
        height = banner(ui, text, BannerTone::Error);
      });
      out.textures_delta.clear();
      height
    };

    // One row plus the 8 px frame margin above and below.
    let one_line = height_of("Wrong username or password");
    let row = one_line - 16.0;
    assert!(row > 0.0, "a band has at least one row: {one_line}");
    let capped = one_line + (BANNER_ROWS - 1) as f32 * row;

    let run_on = "login rejected (ac_id=1): {portal answer} ".repeat(20);
    assert!((height_of(&run_on) - capped).abs() <= 1.0, "one long line");

    // Embedded newlines count as rows too, so a multi-line answer is capped
    // the same way.
    let stacked = "row\n".repeat(BANNER_ROWS * 3);
    assert!((height_of(&stacked) - capped).abs() <= 1.0, "many rows");
  }

  /// The footer is the only place the build date shows, so pin its shape:
  /// `THUnet <version>` on the left, `Debug|Release · Built at YYYY-MM-DD HH:MM:SS +HH:MM`
  /// on the right.
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
    let prefix = format!("{} · Built at ", build_profile());
    let rest = label.strip_prefix(&prefix).expect(&label);
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

  /// A network where logging in cannot work is a warning band, not a red one;
  /// failures keep the red band and lead it.
  #[test]
  fn a_non_campus_network_shows_a_warning_band() {
    let t = crate::i18n::Lang::En.texts();
    let tones = |env: LoginEnv, state: State, err: Option<Failure>| {
      page_banners(
        Page::Status,
        state,
        true,
        None,
        env,
        err.as_ref(),
        false,
        true,
        t,
      )
      .into_iter()
      .map(|(tone, _)| tone)
      .collect::<Vec<_>>()
    };

    assert_eq!(tones(LoginEnv::Campus, State::Offline, None), vec![]);
    assert_eq!(
      tones(LoginEnv::OtherPortal, State::Offline, None),
      vec![BannerTone::Warn]
    );
    // Online, the verdict is only worth a band where it says "nothing to do":
    // another operator's portal is not shown while a session is up.
    assert_eq!(tones(LoginEnv::OtherPortal, State::Online, None), vec![]);
    // An 802.1X network is amber until its session is up and green afterwards.
    let secure = || LoginEnv::EnterpriseWifi("Tsinghua-Secure".into());
    assert_eq!(
      tones(secure(), State::Offline, None),
      vec![BannerTone::Warn]
    );
    let online = page_banners(
      Page::Status,
      State::Online,
      true,
      None,
      secure(),
      None,
      false,
      true,
      t,
    );
    assert_eq!(
      online,
      vec![(
        BannerTone::Ok,
        t.secure_wifi_online.replacen("{}", "Tsinghua-Secure", 1)
      )]
    );
    // With no core answering the stored verdict is not trustworthy either.
    assert_eq!(
      page_banners(
        Page::Status,
        State::Offline,
        false,
        None,
        LoginEnv::NoNetwork,
        None,
        false,
        true,
        t
      ),
      vec![(BannerTone::Error, t.service_down.to_string())]
    );
    // A core that answers with another protocol is running, so the band says
    // that instead of "not running" and keeps saying it while the retry loop
    // is throttled.
    assert_eq!(
      page_banners(
        Page::Status,
        State::Offline,
        false,
        Some(1),
        LoginEnv::Campus,
        None,
        false,
        true,
        t
      ),
      vec![(BannerTone::Error, t.service_stale.replacen("{}", "1", 1))]
    );
    // A rejection is red and comes before the warning.
    assert_eq!(
      tones(
        LoginEnv::NoNetwork,
        State::Offline,
        Some(Failure::Credentials)
      ),
      vec![BannerTone::Error, BannerTone::Warn]
    );
    // A network failure is worded from the active texts, so the band follows
    // a language switch instead of keeping whatever language the OS printed.
    for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Zh] {
      let t = lang.texts();
      assert_eq!(
        page_banners(
          Page::Status,
          State::Offline,
          true,
          None,
          LoginEnv::Campus,
          Some(&Failure::Network(crate::srun::NetCause::Resolve)),
          false,
          true,
          t
        ),
        vec![(BannerTone::Error, t.net_resolve.to_string())]
      );
    }
    // Settings shows only the credential fix, which is what that page is for.
    assert_eq!(
      page_banners(
        Page::Settings,
        State::Offline,
        true,
        None,
        LoginEnv::NoNetwork,
        Some(&Failure::Credentials),
        false,
        true,
        t
      ),
      vec![(BannerTone::Error, t.invalid_credentials.to_string())]
    );
    // Auto connect with nothing to connect with is an amber band, next to the
    // network verdict; while online it would be noise.
    let auto = |state: State, ready: bool| {
      page_banners(
        Page::Status,
        state,
        true,
        None,
        LoginEnv::Campus,
        None,
        true,
        ready,
        t,
      )
    };
    assert_eq!(
      auto(State::Offline, false),
      vec![(
        BannerTone::Warn,
        t.auto_connect_without_credentials.to_string()
      )]
    );
    assert!(auto(State::Offline, true).is_empty(), "credentials saved");
    // A leftover session looks healthy, so the band has to say the machine
    // cannot sign in again rather than stay silent.
    assert_eq!(
      auto(State::Online, false),
      vec![(
        BannerTone::Warn,
        t.auto_connect_without_credentials_online.to_string()
      )]
    );
    assert!(auto(State::Online, true).is_empty(), "credentials saved");
  }

  /// Apply has to ask before an online switch that has nothing to fall back on,
  /// and has to stay silent when it does: a saved password means the core signs
  /// the previous account back in if the new one is refused.
  #[test]
  fn only_a_switch_without_a_fallback_asks_first() {
    // Online, a new pair typed, a saved password the core could fall back to.
    assert_eq!(
      credential_action(true, false, true, false, true),
      CredentialAction::Switch,
      "a silent switch when the old pair is still there"
    );
    // Same, but the core holds nothing to sign back in with.
    assert_eq!(
      credential_action(true, false, true, false, false),
      CredentialAction::Confirm,
      "no fallback: ask first"
    );
    // A new username with the old password kept is a switch too.
    assert_eq!(
      credential_action(true, true, true, false, false),
      CredentialAction::Confirm
    );
    // Offline there is no session to replace, so nothing to warn about.
    assert_eq!(
      credential_action(false, false, true, false, false),
      CredentialAction::Store
    );
    // An empty password field means "keep the stored one".
    assert_eq!(
      credential_action(true, true, false, false, false),
      CredentialAction::Store
    );
    // Retyping the password the core already confirmed as the stored one.
    assert_eq!(
      credential_action(true, false, true, true, false),
      CredentialAction::Store
    );
    // ... but the same password under a different account is a real change.
    assert_eq!(
      credential_action(true, true, true, true, false),
      CredentialAction::Confirm
    );
  }

  /// *Apply* stays disabled unless it has something it is allowed to do: with
  /// *Remember password* ticked, a changed account must come with a typed
  /// password, while unticking the box is the explicit clear.
  #[test]
  fn apply_needs_a_password_before_it_changes_the_account() {
    assert!(
      !can_apply(true, "alice", "", false, true),
      "rename without password"
    );
    assert!(
      !can_apply(true, "", "pw", false, true),
      "password without account"
    );
    assert!(
      !can_apply(true, "   ", "pw", false, true),
      "a blank account"
    );
    assert!(can_apply(true, "alice", "pw", false, true));
    // Unticking the box clears the stored credential and needs neither field.
    assert!(can_apply(false, "", "", false, true));
    assert!(can_apply(false, "alice", "", false, true));
    // Nothing changed: nothing to apply either way.
    assert!(!can_apply(true, "alice", "pw", false, false));
    assert!(!can_apply(false, "", "", false, false));
  }

  /// Ticking *Remember password* after an Apply that did not save it has no
  /// plaintext to send — the core holds the password — so the request is to
  /// seal that one, which is only allowed for the same account.
  #[test]
  fn a_held_password_can_be_remembered_without_retyping_it() {
    assert!(
      can_apply(true, "alice", "", true, true),
      "held, same account"
    );
    assert!(
      !can_apply(true, "bob", "", false, true),
      "a held password only counts for the account it belongs to"
    );
    assert!(
      !can_apply(true, "alice", "", false, true),
      "nothing held for this form"
    );
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

  #[test]
  fn only_a_newer_core_build_restarts_the_window() {
    // Same build (manual stop/start, plain restart): leave the window alone.
    assert!(!should_restart(100, 100, true));
    // A newer core whose file was replaced by an upgrade: restart once.
    assert!(should_restart(200, 100, true));
    // The file did not change, so a restart would run the same old binary.
    assert!(!should_restart(200, 100, false));
    // An older core, and one that predates the stamp.
    assert!(!should_restart(100, 200, true));
    assert!(!should_restart(0, 200, true));
    assert!(!should_restart(200, 0, true));
  }
}

/// Space between stacked bands, and between the last band and the page.
const BANNER_GAP: f32 = 8.0;

/// How many lines a band may take before it ends in an ellipsis.
///
/// Every message the window writes itself fits in this many lines at the
/// default width; the cap exists for text it did not write, such as a portal
/// answer pasted into a login error. Without it the band — and the window
/// grown to fit it — could cover the whole page.
const BANNER_ROWS: usize = 3;

/// Which palette a top band uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BannerTone {
  /// Something failed and the user has to act: no core answering, or a login
  /// the portal refused.
  Error,
  /// Nothing failed; a login simply cannot work on this network, and the
  /// verdict resolves itself once the probe sees a campus portal again.
  Warn,
  /// Nothing to do at all: the network authenticates below the portal and the
  /// session is already up.
  Ok,
}

/// The bands shown at the top of the page, in display order.
///
/// Failures that need the user lead; the network verdict follows, because "off
/// campus" is information, not a fault, and it is derived from the last probe
/// rather than from an attempt that failed.
fn page_banners(
  page: Page,
  state: State,
  core_answered: bool,
  stale: Option<u8>,
  env: LoginEnv,
  last_error: Option<&Failure>,
  auto_connect: bool,
  credentials_ready: bool,
  t: &Texts,
) -> Vec<(BannerTone, String)> {
  let mut banners = Vec::new();
  // A service that is not serving this build only matters on the page with the
  // button that fixes it. An old core says so instead of claiming to be gone.
  if page == Page::Status && !core_answered {
    let text = match stale {
      Some(v) => t.service_stale.replacen("{}", &v.to_string(), 1),
      None => t.service_down.to_string(),
    };
    banners.push((BannerTone::Error, text));
  }
  // A rejected password is shown on both pages: Settings is where it is
  // fixed, so hiding it there would leave the fix without feedback. `Text`
  // arrives worded by the core, which is why only these variants carry a
  // wording here; `Network` is worded from `t`.
  if let Some(err) = last_error {
    let text = match err {
      Failure::Credentials => t.invalid_credentials.to_string(),
      Failure::AcUnreachable => t.ac_unreachable.to_string(),
      Failure::Network(cause) => t.net_cause(*cause).to_string(),
      Failure::Text(e) => e.clone(),
    };
    // An `ac_id` that belongs to another link is not a fault of this machine,
    // and only something outside this window can change it.
    let tone = match err {
      Failure::AcUnreachable => BannerTone::Warn,
      _ => BannerTone::Error,
    };
    banners.push((tone, text));
  }
  // The verdict explains the state of *Connect*: amber while a login is
  // impossible here (no network, another operator's portal, or an 802.1X
  // network that has not signed in yet), green on that same network once the
  // session is up. The core re-probes as soon as the path to the internet
  // changes, so this is not a leftover from the network the machine left.
  if page == Page::Status && core_answered {
    match env {
      LoginEnv::EnterpriseWifi(ref ssid) if state == State::Online => {
        banners.push((BannerTone::Ok, t.secure_wifi_online.replacen("{}", ssid, 1)))
      }
      _ if state != State::Online => {
        if let Some(why) = env.blocked_reason(t) {
          banners.push((BannerTone::Warn, why));
        }
      }
      _ => {}
    }
  }
  // Auto connect with nothing to connect with. Offline the machine sits there
  // until someone fills the form in; online it looks healthy but cannot sign in
  // again — a leftover portal session, which survives a core restart because
  // keepalive needs no credential, hides exactly that. Information, not a
  // failure, so it uses the amber band either way.
  if page == Page::Status && core_answered && auto_connect && !credentials_ready {
    let text = if state == State::Online {
      t.auto_connect_without_credentials_online
    } else {
      t.auto_connect_without_credentials
    };
    banners.push((BannerTone::Warn, text.to_string()));
  }
  banners
}

/// Draw one full-width band across the top of the page.
///
/// The text wraps at the band's width and is cut after [`BANNER_ROWS`] lines
/// with an ellipsis. egui shows the untruncated text on hover by itself (a
/// `Label` whose galley came out elided does that), so no tooltip is set here -
/// a manual one would stack a second bubble on top of it.
///
/// Returns the height it consumed, so the window can grow by the same amount
/// and the page below keeps the size it has without a warning.
fn banner(ui: &mut egui::Ui, text: impl Into<String>, tone: BannerTone) -> f32 {
  let text = text.into();
  let (fill, ink) = match tone {
    BannerTone::Error => (
      egui::Color32::from_rgb(90, 30, 30),
      egui::Color32::from_rgb(255, 190, 190),
    ),
    // The amber of a warn log line, dimmed for a band this wide.
    BannerTone::Warn => (
      egui::Color32::from_rgb(74, 58, 20),
      egui::Color32::from_rgb(240, 206, 128),
    ),
    // A dimmed green: the news is good, so it should not shout.
    BannerTone::Ok => (
      egui::Color32::from_rgb(28, 56, 34),
      egui::Color32::from_rgb(168, 226, 178),
    ),
  };
  let frame = egui::Frame::new()
    .fill(fill)
    .inner_margin(egui::Margin::same(8))
    .corner_radius(6)
    .show(ui, |ui| {
      // Full width (instead of shrink-wrapping the text) and centred, so the
      // strip reads as one band across the top of the page rather than a small
      // box.
      let width = ui.available_width();
      ui.set_width(width);
      ui.vertical_centered(|ui| {
        // The text is laid out here instead of being handed to a `Label` as a
        // string: a Label re-wraps whatever job it gets and would drop the row
        // limit, so only a galley built here keeps the cap. epaint appends
        // `…` to the elided last row by itself.
        let mut job = egui::text::LayoutJob::simple(
          text,
          egui::TextStyle::Body.resolve(ui.style()),
          ink,
          width,
        );
        job.wrap.max_rows = BANNER_ROWS;
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        ui.add(egui::Label::new(egui::WidgetText::Galley(galley)));
      });
    });
  frame.response.rect.height()
}

impl App {
  /// Grow or shrink the client area by the banner height.
  ///
  /// Only the banner's own height is measured; measuring the whole page would
  /// feed the available height back into the layout (the reason the window is
  /// otherwise a fixed size). The width never changes, so the banner wraps the
  /// same way and the target stays stable instead of oscillating.
  fn sync_window_height(&mut self, ctx: &egui::Context) {
    if (self.banner_height - self.window_extra).abs() < 0.5 {
      return;
    }
    self.window_extra = self.banner_height;
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::new(
      DEFAULT_WIDTH,
      WINDOW_HEIGHT + self.banner_height,
    )));
  }
}

impl eframe::App for App {
  fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    // The stored theme is pushed here rather than in `App::new`, which runs
    // before there is a context to apply it to. `applied_theme` remembers what
    // was sent, so this is a no-op on the frames in between.
    if self.applied_theme.as_deref() != Some(self.cfg.theme.as_str()) {
      ctx.set_theme(theme_preference(&self.cfg.theme));
      self.applied_theme = Some(self.cfg.theme.clone());
    }

    // GTK delivers the appindicator menu callbacks through its own event loop,
    // but eframe owns the main loop here. Pump GTK every pass, otherwise menu
    // clicks are queued forever and never reach `handle_menu`.
    #[cfg(target_os = "linux")]
    while gtk::events_pending() {
      gtk::main_iteration_do(false);
    }

    // A mode change may have been waiting for a UAC prompt; its result is due
    // regardless of what else happens this frame.
    self.pump_mode_request();

    // Place the window in the middle of its monitor. eframe persists no window
    // geometry (`persist_window: false`, the layout fixes the size), so without
    // this the OS just picks a spot — Windows put the first window in the
    // top-left corner of the work area. Only once per process: the user's own
    // drag has to win over a repeated command.
    if !self.centered {
      let placement = ctx.input(|i| {
        let viewport = i.viewport();
        (
          viewport.monitor_size,
          viewport
            .outer_rect
            .or(viewport.inner_rect)
            .map(|rect| rect.size()),
        )
      });
      if let (Some(monitor), Some(frame)) = placement {
        self.centered = true;
        let corner = ((monitor - frame) * 0.5).max(egui::Vec2::ZERO);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(corner.to_pos2()));
      }
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

    // An installer (or `thunet quit`) asks for a real exit, not the tray's
    // hide-on-close: it needs the image gone before it replaces the file.
    if crate::single::quit_requested() {
      self.quit();
    }

    // The core may have been restarted (crash, upgrade, systemd unit or logon
    // task): reconnect instead of going deaf.
    self.ensure_link();

    // The window coming to the front is the moment the page has to be current;
    // waiting for the core's 10–15 s poll made a freshly opened window show
    // numbers that were seconds old. The refresh is quiet: the user did not
    // ask for it, so it neither logs nor toasts.
    let focused = ctx.input(|i| i.focused);
    if focused && !self.focused {
      self.send(Cmd::Refresh { quiet: true });
      self.next_core_query = Instant::now();
    }
    self.focused = focused;

    // Outcome of a Start/Stop that ran on the helper thread. The tag matters:
    // a successful Stop has usually already been completed by the link
    // dropping, and treating its late `Ok` as a Start would arm a bogus
    // pending request that disables the button forever.
    while let Ok((request, result)) = self.start_rx.try_recv() {
      self.push_log(Line::debug(format!(
        "service: {request:?} helper returned {result:?}"
      )));
      match (request, result) {
        (ServiceRequest::Stop, Ok(())) => {
          // The link drop in `ensure_link` confirms and toasts it.
        }
        (ServiceRequest::Stop, Err(e)) => {
          if self
            .service_request
            .is_some_and(|(r, _)| r == ServiceRequest::Stop)
          {
            self.service_request = None;
            self.push_log(Line::error(format!("service: {e}")));
            // The stop never ran, so this is about the prompt, not about the
            // service refusing to stop.
            let msg = if e == crate::elevate::NO_AUTHORIZATION {
              self.t().admin_unavailable.to_string()
            } else {
              self.t().service_stop_failed.to_string()
            };
            self.toast_err(msg);
          } else {
            // The core is already gone, so the stop did take effect.
            self.push_log(Line::warn(format!("service: {e} (the core is gone)")));
          }
        }
        (ServiceRequest::Start, Ok(())) => {
          // Do not sit out the reconnect throttle: the new core is usually
          // listening before the next frame.
          self.next_link_try = Instant::now();
          // `ensure_link` ran above, so a core that already answered has
          // cleared the click and toasted "started". Re-arming the watchdog
          // here would report a failure later for a start that succeeded.
          if !self.core_answered {
            self.service_request = Some((ServiceRequest::Start, Instant::now()));
          }
        }
        (ServiceRequest::Start, Err(e)) => {
          let cancelled = e == crate::elevate::CANCELLED;
          let unauthorized = e == crate::elevate::NO_AUTHORIZATION;
          let recovered = self.core_answered
            && self.link.as_ref().is_some_and(|link| {
              #[cfg(any(windows, target_os = "linux"))]
              {
                link.is_machine() || crate::ipc::portable()
              }
              #[cfg(not(any(windows, target_os = "linux")))]
              {
                let _ = link;
                true
              }
            });
          self.service_request = None;
          self.pending_credentials = None;
          self.push_log(Line::error(format!("service: {e}")));
          if !recovered {
            let msg = if unauthorized {
              self.t().admin_unavailable.to_string()
            } else if cancelled {
              self.t().admin_cancelled.to_string()
            } else {
              self.t().service_start_failed.to_string()
            };
            self.toast_err(msg);
          }
        }
      }
    }

    // A Start/Stop whose outcome never arrived (no socket, or a core that did
    // not exit) is reported instead of leaving the click unanswered.
    if let Some((request, at)) = self.service_request
      && at.elapsed() > SERVICE_REQUEST_TIMEOUT
    {
      self.service_request = None;
      self.pending_credentials = None;
      let log = match request {
        ServiceRequest::Start => format!(
          "service: no core answered after {} s",
          SERVICE_REQUEST_TIMEOUT.as_secs()
        ),
        ServiceRequest::Stop => format!(
          "service: the core is still running after {} s",
          SERVICE_REQUEST_TIMEOUT.as_secs()
        ),
      };
      self.push_log(Line::warn(log));
      let msg = match request {
        ServiceRequest::Start => self.t().service_start_failed,
        ServiceRequest::Stop => self.t().service_stop_failed,
      }
      .to_string();
      self.toast_err(msg);
    }

    // Verify a typed password against the core's copy once typing pauses, so
    // re-entering the saved value does not look like an unsaved edit.
    self.check_typed_password();

    // A switch whose answer never arrived (core restarted mid-way, link lost)
    // must not leave *Apply* disabled for the rest of the session.
    if self
      .switch_pending
      .is_some_and(|at| at.elapsed() > SWITCH_TIMEOUT)
    {
      self.switch_pending = None;
      self.push_log(Line::warn("credentials: no answer from the core"));
    }

    // Which link carries the session is a local fact, so it is read here
    // instead of being asked of the core: the row has to follow a cable being
    // plugged in within a couple of seconds, and the service poll is 30 s.
    if Instant::now() >= self.next_uplink {
      self.next_uplink = Instant::now() + Duration::from_secs(2);
      self.uplink = crate::srun::link_kind();
    }

    // Ask the service about itself now and then; the row it fills is advisory,
    // so a slow cadence is plenty.
    if self.link.as_ref().is_some_and(Link::alive) && Instant::now() >= self.next_core_query {
      self.next_core_query = Instant::now() + Duration::from_secs(30);
      self.send(Cmd::CoreInfo);
    }

    // A left click on the tray icon shows the window, a double click hides it.
    // Windows only: the Linux backend delivers no click event at all, so there
    // the tray menu is the only way in (and out).
    #[cfg(windows)]
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
        self.quit();
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
        // `process::exit` skips `Guard::drop`; remove the Unix lock file first
        // so the next launch does not have to reap it by pid.
        crate::single::release();
        std::process::exit(0);
      }
    }

    // Keep ticking while hidden so tray clicks are still processed. The icon
    // (and, on Windows, the tooltip) come from the same words as the window's
    // state line.
    let t = self.t();
    // `label` feeds the tooltip below only, and that is Windows-only.
    #[cfg_attr(not(windows), allow(unused_variables))]
    let (label, rgb) = self.state_display(t);
    if self.tray_rgb != Some(rgb) {
      self.tray_rgb = Some(rgb);
      if let Some(tray) = &self.tray {
        let _ = tray.set_icon(Some(make_icon(rgb)));
      }
    }
    // The tooltip is Windows-only: `set_tooltip` is a no-op in the Linux
    // backend, so there it would just build a string nobody reads.
    #[cfg(windows)]
    {
      let tip = match self.state {
        State::Online => format!("{} · {label} · {}", app_name(), dash(&self.info.ip4)),
        _ => format!("{} · {label}", app_name()),
      };
      if let Some(tray) = &self.tray {
        let _ = tray.set_tooltip(Some(&tip));
      }
    }
    self
      .connect_item
      .set_enabled(self.state != State::Online && self.env.allows_login());
    self
      .disconnect_item
      .set_enabled(self.state == State::Online && self.portal_verbs());

    // When hidden, wake up often enough to pick up a second launch quickly.
    let tick = if self.shown { 500 } else { 200 };
    ctx.request_repaint_after(Duration::from_millis(tick));
  }

  fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    self.paint(ui);
    self.sync_window_height(ui.ctx());
    paint_toast(ui.ctx(), self.toast.as_ref());
    self.paint_switch_confirm(ui.ctx());
  }
}

const DEFAULT_WIDTH: f32 = 420.0;
/// One row of the status grid, measured: the label height plus the 6 px the
/// grid leaves between rows.
const INFO_ROW_HEIGHT: f32 = 27.0;
/// Fixed client height: tall enough for the denser settings page (including
/// its log-level row and the build stamp), tight enough that the status page
/// has no dead space. The status grid gained the `Link` row after this was
/// tuned, so its height is added on top - otherwise the log pane would shrink
/// by one line every time a row is added.
const WINDOW_HEIGHT: f32 = 300.0 + INFO_ROW_HEIGHT;

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

/// Build stamp of this process (see `build.rs`), seconds since the epoch.
fn own_build_epoch() -> u64 {
  env!("THUNET_BUILD_EPOCH").parse().unwrap_or(0)
}

/// (size, mtime) of the executable a restart would exec.
fn exe_stamp(path: &std::path::Path) -> Option<(u64, std::time::SystemTime)> {
  let meta = std::fs::metadata(path).ok()?;
  Some((meta.len(), meta.modified().ok()?))
}

/// Should this window replace itself with the file on disk?
///
/// All three conditions matter:
/// * the service must report a *newer* build: an equal one is a manual
///   stop/start or a plain restart and must leave the window alone, and a
///   merely different one could bounce against an older service forever;
/// * the stamp must be known (`0` is a core built before this field existed);
/// * the file this process would exec must have changed since it started,
///   otherwise a stale copy at a fixed path (a portable install next to a
///   newer system service) would restart itself forever.
fn should_restart(core_epoch: u64, own_epoch: u64, file_changed: bool) -> bool {
  file_changed && core_epoch > 0 && own_epoch > 0 && core_epoch > own_epoch
}

/// Right half of the settings footer: what the build time actually is.
fn build_time_label(t: &Texts) -> String {
  format!("{} · {} {}", build_profile(), t.built, build_time())
}

/// `Debug` or `Release`, from the profile Cargo used for this build.
fn build_profile() -> &'static str {
  match env!("THUNET_PROFILE") {
    "debug" => "Debug",
    "release" => "Release",
    other => other,
  }
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

/// Is this a Wayland session with no XWayland behind it?
///
/// `run_with` asks for X11 whenever `DISPLAY` is set, and hides the window by
/// hiding the toplevel; winit cannot do that on Wayland, so the sessions that
/// end up here need the minimising fallback instead.
fn wayland_without_x11() -> bool {
  #[cfg(target_os = "linux")]
  {
    let forced = std::env::var("THUNET_UNIX_BACKEND").is_ok_and(|v| v == "wayland");
    (forced || std::env::var_os("DISPLAY").is_none())
      && std::env::var_os("WAYLAND_DISPLAY").is_some()
  }
  #[cfg(not(target_os = "linux"))]
  {
    false
  }
}

/// Left clicks counted in the current burst: 0 = nothing pending, 1 = one click
/// waiting for the double-click deadline, 2 = a double click.
#[cfg(windows)]
static CLICKS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// When the latest left click arrived, in ms since `STARTED`; 0 = no click yet.
#[cfg(windows)]
static LAST_CLICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(windows)]
fn now_ms() -> u64 {
  // 0 is the "no click yet" sentinel, so the clock starts at 1.
  STARTED
    .get()
    .map_or(0, |s| s.elapsed().as_millis() as u64 + 1)
}

/// A second left click inside this window counts as a double click. 300 ms is
/// snappier than the 500 ms most desktops use; the delay is a compromise, since
/// a single click can only be acted on once no second click can follow.
#[cfg(windows)]
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

/// Consume the pending left click, if it can be resolved already.
#[cfg(windows)]
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
///
/// Windows only: the Linux backend never emits a `TrayIconEvent`, so there is
/// nothing to handle.
#[cfg(windows)]
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
  #[cfg(windows)]
  if crate::ipc::portable() {
    set_portable_app_id();
  }
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

/// Give the portable window its own taskbar identity.
///
/// Windows builds the taskbar button from the process's AppUserModelID: with
/// the implicit one (the executable path) both modes resolve to the installed
/// shortcut, so the portable window joins the installed one in a single button
/// and the button keeps the shortcut's icon whatever the window icon is. A
/// separate id makes it a separate button, and a button with no matching
/// shortcut falls back to the window's own icon - the blue one.
#[cfg(windows)]
fn set_portable_app_id() {
  use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
  let id: Vec<u16> = "THUnet.Portable\0".encode_utf16().collect();
  // SAFETY: `id` is NUL-terminated and outlives the call. A failure at worst
  // leaves the portable window grouped with the installed one, as before.
  let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
}

fn run_with(cfg: Config, minimized: bool, renderer: eframe::Renderer) -> Result<(), String> {
  // Mutated only by the X11 block below; other platforms build it as is.
  #[allow(unused_mut)]
  let mut options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default()
      .with_inner_size([DEFAULT_WIDTH, WINDOW_HEIGHT])
      .with_resizable(false)
      .with_visible(!minimized)
      .with_icon(std::sync::Arc::new(app_icon()))
      .with_title(app_name()),
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
      #[cfg(windows)]
      install_tray_events(&cc.egui_ctx);
      cc.egui_ctx.add_plugin(LogCopyClick::default());
      Ok(Box::new(App::new(cfg, minimized, font)))
    }),
  )
  .map_err(|e| e.to_string())
}
