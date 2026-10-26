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

use crate::autostart::{Autostart, Mode};
use crate::config::Config;
use crate::core::{POLL_OFFLINE, POLL_ONLINE};
use crate::i18n::{Lang, Texts};
use crate::icons::{
  DeviceKind, app_fill, app_icon, back_arrow_image, close_button, color_for, device_kind,
  eye_closed_image, eye_open_image, gear_image, heading_icon, icon_button, icon_texture,
  paint_copy_icon, paint_device_icon,
};
use crate::ipc::Link;
use crate::logline::{Level, Line};
use crate::proto::{Cmd, CoreInfo, CredentialState, Ev, Failure, LinkKind, LoginEnv, ToastKind};
use crate::session::State;
use crate::tray::{Action, Tray};

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
      LoginEnv::NoPortal | LoginEnv::OtherNetwork => Some(t.no_login_needed.to_string()),
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
  /// The account's online devices, this machine first. Empty when the portal
  /// reports no detail (offline, or an answer without the field).
  devices: Vec<Device>,
  /// How many devices the account has online, from the detail when there is
  /// one and from the portal's own total otherwise.
  devices_total: usize,
}

/// One device the account has online, as `/cgi-bin/rad_user_info` lists it.
#[derive(Default, Clone)]
struct Device {
  /// Portal session id. Only keeps the order stable inside a group.
  id: String,
  ip4: String,
  ip6: String,
  os: String,
  class: String,
  /// This is the machine the window runs on, judged by the session's own
  /// addresses.
  local: bool,
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

/// What a private value looks like when privacy is on.
///
/// Addresses keep the length of their family, because the row label already says
/// which family it is; an account gets a fixed run, so its length is not on
/// screen.
const MASK: &str = "•••••";
const MASK_IP4: &str = "•••••••••";
const MASK_IP6: &str = "••••••••••••••••";

fn mask_for(value: &str) -> &'static str {
  if value.contains(':') {
    MASK_IP6
  } else if value.contains('.') {
    MASK_IP4
  } else {
    MASK
  }
}

/// The account and the addresses are private; the row label, the session
/// traffic and the device types are not.
fn private_value(value: &str, hidden: bool) -> &str {
  if hidden && !value.is_empty() && value != "—" {
    mask_for(value)
  } else {
    value
  }
}

/// A character an account name can be made of. Masking stops at the edges of
/// this class, so the account `bob` does not turn the `bob` in `bobby` into
/// dots.
fn account_char(c: char) -> bool {
  c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@')
}

/// The operating system's account name.
///
/// It arrives in the log through paths (`config: /home/alice/...`, a state
/// directory) and through messages about the process itself, so privacy mode
/// hides it along with the portal account. Read once: a process cannot change
/// the account it runs as.
fn os_account() -> &'static str {
  static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
  NAME
    .get_or_init(|| {
      ["USER", "LOGNAME", "USERNAME"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok())
        .unwrap_or_default()
    })
    .as_str()
}

/// Replace every whole-token occurrence of `needle` in `text` with [`MASK`].
fn mask_token(text: &str, needle: &str) -> String {
  if needle.is_empty() || !text.contains(needle) {
    return text.to_string();
  }
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(at) = rest.find(needle) {
    let (head, tail) = rest.split_at(at);
    let after = &tail[needle.len()..];
    out.push_str(head);
    let before_ok = head.chars().next_back().is_none_or(|c| !account_char(c));
    let after_ok = after.chars().next().is_none_or(|c| !account_char(c));
    out.push_str(if before_ok && after_ok { MASK } else { needle });
    rest = after;
  }
  out.push_str(rest);
  out
}

/// Mask the account inside a rendered log line.
///
/// The line itself is kept: the portal echoes the account in some answers
/// (`logged out: {...}`) and the logout warning names both the reported and
/// the saved one, so hiding the affected lines would cost the context that
/// makes them useful. Only the names turn into dots.
fn masked_log(text: String, accounts: &[&str], hidden: bool) -> String {
  if !hidden {
    return text;
  }
  accounts
    .iter()
    .map(|name| name.trim())
    .filter(|name| !name.is_empty())
    .fold(text, |acc, name| mask_token(&acc, name))
}

fn info_from(v: &Value) -> Info {
  let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
  let ip4 = {
    let ip = norm_ip(s("online_ip"));
    if ip.is_empty() { norm_ip(s("ip")) } else { ip }
  };
  let ip6 = norm_ip(s("online_ip6"));
  let devices = devices_from(v.get("online_device_detail"), &ip4, &ip6);
  // The portal reports the total as a *string*, and it can disagree with the
  // detail: count what is actually listed, and only fall back to the portal's
  // number when the detail is missing altogether (an older core, or offline).
  let devices_total = if devices.is_empty() {
    v.get("online_device_total")
      .and_then(|n| {
        n.as_str()
          .and_then(|text| text.trim().parse().ok())
          .or_else(|| n.as_u64().map(|n| n as usize))
      })
      .unwrap_or(0)
  } else {
    devices.len()
  };
  Info {
    ip4,
    ip6,
    bytes_in: v.get("bytes_in").and_then(Value::as_u64).unwrap_or(0),
    bytes_out: v.get("bytes_out").and_then(Value::as_u64).unwrap_or(0),
    name: s("user_name").to_string(),
    devices,
    devices_total,
  }
}

/// The per-device list from `rad_user_info`.
///
/// The core hands the portal's JSON-in-a-string on as an object, keyed by
/// session id. A device with no address at all is dropped (it cannot be shown),
/// and the order is stable so the hover list does not jump around between
/// polls: this machine, then devices with an IPv4 address, then IPv6-only ones.
fn devices_from(detail: Option<&Value>, local4: &str, local6: &str) -> Vec<Device> {
  let mut devices = Vec::new();
  if let Some(map) = detail.and_then(Value::as_object) {
    for (id, entry) in map {
      let s = |k: &str| {
        entry
          .get(k)
          .and_then(Value::as_str)
          .unwrap_or("")
          .trim()
          .to_string()
      };
      let ip4 = norm_ip(&s("ip"));
      let ip6 = norm_ip(&s("ip6"));
      if ip4.is_empty() && ip6.is_empty() {
        continue;
      }
      let local = (!local4.is_empty() && ip4 == local4) || (!local6.is_empty() && ip6 == local6);
      devices.push(Device {
        id: id.clone(),
        ip4,
        ip6,
        os: s("os_name"),
        class: s("class_name"),
        local,
      });
    }
  }
  devices.sort_by(|a, b| {
    b.local
      .cmp(&a.local)
      .then_with(|| a.ip4.is_empty().cmp(&b.ip4.is_empty()))
      .then_with(|| a.id.cmp(&b.id))
  });
  devices
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

/// The state palette of [`color_for`], as an egui colour.
fn state_color(state: State) -> egui::Color32 {
  let c = color_for(state);
  egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// How far one status-poll cycle has run, 0.0 .. 1.0.
fn heartbeat_fraction(elapsed: Duration, period: Duration) -> f32 {
  if period.is_zero() {
    return 0.0;
  }
  (elapsed.as_secs_f32() / period.as_secs_f32()).clamp(0.0, 1.0)
}

/// The turtle that walks the status track.
///
/// It stands on a fixed-length line and its position is how far the current
/// poll period has run, so the line itself never changes length. It waits at
/// the right end, and the line turns amber, once the core has missed a whole
/// cycle.
fn status_track(
  ui: &mut egui::Ui,
  state: State,
  at: Option<Instant>,
  turtle: Option<&egui::TextureHandle>,
  t: &Texts,
) {
  const TURTLE: f32 = 18.0;
  const TRACK: f32 = 2.0;
  /// The line sits where an underline would: the row's bottom edge is the line
  /// box's descent, so the track goes just above it, then up again by its own
  /// thickness so the gap reads the same from both sides. The turtle stands on
  /// the line, which is what makes the pair read as one block.
  const UNDERLINE: f32 = 1.5;
  /// Room the header keeps on the right for its buttons: the settings or back
  /// button, and on the status page the privacy eye beside it.
  const BUTTON: f32 = 72.0;

  let period = if state == State::Online {
    POLL_ONLINE
  } else {
    POLL_OFFLINE
  };
  let width = (ui.available_width() - BUTTON).max(40.0);
  let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, TURTLE), egui::Sense::hover());
  let painter = ui.painter();
  let mid = rect.bottom() - UNDERLINE - TRACK;

  let elapsed = at.map_or(Duration::ZERO, |instant| instant.elapsed());
  let late = at.is_some_and(|_| elapsed > period.mul_f32(1.8));
  let color = if late {
    level_color(Level::Warn, ui.visuals())
  } else {
    ui.visuals().weak_text_color().gamma_multiply(0.35)
  };
  painter.rect_filled(
    egui::Rect::from_min_size(
      egui::pos2(rect.left(), mid - TRACK / 2.0),
      egui::vec2(rect.width(), TRACK),
    ),
    egui::CornerRadius::ZERO,
    color,
  );

  if let Some(turtle) = turtle {
    let x = rect.left() + turtle_x(heartbeat_fraction(elapsed, period), rect.width(), TURTLE);
    painter.image(
      turtle.id(),
      egui::Rect::from_min_size(egui::pos2(x, mid - TURTLE), egui::vec2(TURTLE, TURTLE)),
      // Mirrored: the sprite is drawn facing left, and the turtle has to walk
      // towards the right end of the track.
      egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0)),
      egui::Color32::WHITE,
    );
  }
  let _ = resp.on_hover_text(t.heartbeat_hint);
}

/// Where the turtle's left edge sits on a track of `width`, so it never walks
/// off either end.
fn turtle_x(progress: f32, width: f32, sprite: f32) -> f32 {
  progress.clamp(0.0, 1.0) * (width - sprite).max(0.0)
}

/// The turtle sprite, decoded once and uploaded on first use.
fn load_turtle(ctx: &egui::Context) -> Option<egui::TextureHandle> {
  let image = image::load_from_memory(include_bytes!("../assets/turtle.png"))
    .ok()?
    .to_rgba8();
  let size = [image.width() as usize, image.height() as usize];
  Some(ctx.load_texture(
    "thunet-turtle",
    egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw()),
    egui::TextureOptions::LINEAR,
  ))
}

/// `config.json`'s `theme` setting ("auto" | "light" | "dark"), as egui wants
/// it. `auto` resolves to the desktop's own preference where it can be read
/// (see [`crate::theme`]), and otherwise to egui's `System`, which is what
/// follows the theme on Windows and macOS.
fn theme_preference(setting: &str) -> egui::ThemePreference {
  match setting {
    "light" => egui::ThemePreference::Light,
    "dark" => egui::ThemePreference::Dark,
    _ => crate::theme::system().map_or(egui::ThemePreference::System, Into::into),
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
    // `gray(135)` sat within five levels of the dark theme's body text
    // (`gray(140)`), so debug looked like info. The theme's own weak colour is
    // dimmer than the body text in both themes, which is the point of the
    // level.
    Level::Debug => visuals.weak_text_color(),
    Level::Info => visuals.text_color(),
    Level::Warn => egui::Color32::from_rgb(216, 160, 48),
    Level::Error => egui::Color32::from_rgb(228, 112, 112),
    Level::Fatal => egui::Color32::from_rgb(255, 138, 138),
  }
}

/// What this process calls itself in the title bar, the tray tooltip and
/// Alt-Tab. One string for all of them: the two modes keep separate state,
/// credentials and endpoints, and a user running both has to be able to tell
/// which one is asking.
fn app_name() -> &'static str {
  #[cfg(target_os = "android")]
  {
    // The app is always portable-like on Android; the marker would only
    // confuse the one mode the platform has.
    "THUnet"
  }
  #[cfg(not(target_os = "android"))]
  {
    if crate::ipc::portable() {
      "THUnet Portable"
    } else {
      "THUnet"
    }
  }
}

/// System-bar and IME insets written by `ThunetActivity` in physical pixels:
/// `top bottom ime`. Zeros until the first callback.
#[cfg(target_os = "android")]
fn android_insets() -> (f32, f32) {
  let Ok(text) = std::fs::read_to_string(crate::config::dir().join("insets.txt")) else {
    return (0.0, 0.0);
  };
  let mut parts = text
    .split_whitespace()
    .filter_map(|v| v.parse::<f32>().ok());
  let top = parts.next().unwrap_or(0.0);
  let bottom = parts.next().unwrap_or(0.0);
  let ime = parts.next().unwrap_or(0.0);
  (top, bottom.max(ime))
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
  /// The addresses this machine holds on the current link. The portal row wins
  /// while the portal answers; these fill in on mobile data, another network,
  /// or a campus link without a session, where the portal has nothing to say.
  local_ip4: String,
  local_ip6: String,
  state: State,
  info: Info,
  /// When the last status answer arrived. The turtle beside the state label
  /// walks one poll period from here.
  last_heartbeat: Option<Instant>,
  /// The turtle sprite, uploaded on first use and kept alive by the handle.
  turtle: Option<egui::TextureHandle>,
  /// The device badge was clicked and its list is showing.
  devices_open: bool,
  logs: Vec<Line>,
  /// Android's background level, as written by the Activity into
  /// `files/background.txt`. Read on the slow tick; the settings row shows it.
  #[cfg(target_os = "android")]
  background: crate::background::Mode,
  /// The last request for the resident level lost the notification permission,
  /// so the effective level is lower and the row says why.
  #[cfg(target_os = "android")]
  background_denied: bool,
  /// The user confirmed the OEM toggles that let the wake-up survive a swipe.
  /// They cannot be read, so this is their answer, not a guess.
  #[cfg(target_os = "android")]
  background_allowed: bool,
  /// The system's battery exemption is in place. This one is readable, and it
  /// is what decides whether the app has a background network at all.
  #[cfg(target_os = "android")]
  background_power: bool,
  /// The Android SSID-detection setting; without the location permission it
  /// stays off and the network probes decide.
  #[cfg(target_os = "android")]
  secure_detection: bool,
  /// Which credential field the autofill service last heard about.
  #[cfg(target_os = "android")]
  autofill_focus: Option<AutofillKind>,
  /// The value last reported to the service, so a keystroke is sent once.
  #[cfg(target_os = "android")]
  autofill_value: String,
  /// Where the focused field was last reported, in physical pixels. The window
  /// moves when the keyboard opens, and the service places its dropdown from
  /// this rectangle.
  #[cfg(target_os = "android")]
  autofill_rect: Option<[i32; 4]>,
  /// Long-press detection for the credential fields' clipboard paste.
  #[cfg(target_os = "android")]
  paste: PasteHold,
  /// Long-press detection for the log menu, and the line it marks.
  #[cfg(target_os = "android")]
  log_hold: LogHold,
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
  /// The theme last pushed to the context: `config.json` `theme`, already
  /// resolved, so `auto` is pushed again whenever the desktop switches
  /// light/dark while the window is open.
  applied_theme: Option<egui::ThemePreference>,
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
  /// Inert on Android. `available()` is false when the platform refused to
  /// create an icon (no notification area, e.g. a validation sandbox): the app
  /// then stays window-only, so closing quits instead of hiding and a hidden
  /// start is not attempted.
  tray: Tray,
  /// Colour of the icon currently installed in the tray; `set_icon` rewrites a
  /// temp PNG and re-announces the indicator, so it must only run on a change.
  tray_rgb: Option<[u8; 3]>,
  /// The gear in front of *Settings*, built on first paint: the egui context
  /// does not exist yet when `App::new` runs.
  gear: Option<egui::TextureHandle>,
  /// The arrow in the settings page's *Back* button, likewise.
  back_arrow: Option<egui::TextureHandle>,
  /// The privacy eye in the status header, one texture per state.
  eye_open: Option<egui::TextureHandle>,
  eye_closed: Option<egui::TextureHandle>,
  started: bool,
  /// Input focus in the previous frame. The false-to-true edge is what a return
  /// from the background looks like on Android, and logging it is what makes
  /// "the session survived being hidden" observable.
  was_focused: bool,
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
    // The core owns the credential: it read the sealed blob at startup, and the
    // window must not hold a second copy. Portable mode is exactly where
    // `Config::load()` hands the window the plaintext (the core and the window
    // share a state directory), and seeding `pass_edit` with it made the window
    // treat a saved password as an unsaved edit — the *Unsaved* marker never
    // cleared after a restart on Android.
    let mut cfg = cfg;
    cfg.password.clear();
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

    // Portable is visible in the title and the tray tooltip: the two modes
    // keep separate state, credentials and endpoints, so the user has to be
    // able to tell which one this window is. The application id is left alone
    // - it is what the desktop groups windows by and what the autostart entry
    // points at, and a second id would mean a second icon and a second set of
    // window rules.
    let portable = crate::ipc::portable();
    let app_title = app_name();

    // The autostart modes live in the settings page; the tray menu deliberately
    // has no entry for them, because registering the service can raise a UAC
    // prompt that should not be one stray click away.
    let tray = Tray::new(app_title, t, app_fill(State::Offline));
    // Without a tray icon there is no way back into a hidden window, so an
    // invisible start has to be cancelled and the window left on screen.
    let start_hidden = minimized && tray.available();
    if minimized && !tray.available() {
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
    #[cfg(not(target_os = "android"))]
    if !tray.available() {
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
      local_ip4: String::new(),
      local_ip6: String::new(),
      state: State::Offline,
      info: Info::default(),
      last_heartbeat: None,
      turtle: None,
      devices_open: false,
      logs,
      #[cfg(target_os = "android")]
      background: crate::background::Mode::read(),
      #[cfg(target_os = "android")]
      background_denied: crate::background::denied(),
      #[cfg(target_os = "android")]
      background_allowed: crate::background::allowed(),
      #[cfg(target_os = "android")]
      background_power: crate::background::power_ok(),
      #[cfg(target_os = "android")]
      secure_detection: crate::secure_wifi::enabled(),
      #[cfg(target_os = "android")]
      autofill_focus: None,
      #[cfg(target_os = "android")]
      autofill_value: String::new(),
      #[cfg(target_os = "android")]
      autofill_rect: None,
      #[cfg(target_os = "android")]
      paste: PasteHold::default(),
      #[cfg(target_os = "android")]
      log_hold: LogHold::default(),
      last_error: None,
      banner_height: 0.0,
      window_extra: 0.0,
      service_warned: false,
      core_stale: None,
      core_answered: false,
      user_edit: cfg.username.chars().take(ACCOUNT_MAX_CHARS).collect(),
      pass_edit: String::new(),
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
      eye_open: None,
      eye_closed: None,
      started: false,
      was_focused: false,
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
    self.last_heartbeat = None;
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

  /// Every account name this window knows: the one it configured, the one the
  /// core holds, and the one the portal reports. Any of them can be the name a
  /// log line quotes, so all of them are masked.
  fn accounts(&self) -> Vec<&str> {
    let held = self.core_cred.as_ref().map_or("", |c| c.username.as_str());
    let mut names: Vec<&str> = Vec::new();
    for name in [
      self.cfg.username.as_str(),
      held,
      self.info.name.as_str(),
      os_account(),
    ] {
      let name = name.trim();
      if !name.is_empty() && !names.contains(&name) {
        names.push(name);
      }
    }
    names
  }

  fn apply_lang_setting(&mut self, setting: &str) {
    self.cfg.lang = setting.to_string();
    self.lang = Lang::from_setting(setting);
    let t = self.lang.texts();
    self.tray.set_texts(t);
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
        // The form is submitted: let a password manager offer to save the pair.
        #[cfg(target_os = "android")]
        let _ = crate::android_jni::autofill_save();
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
  #[cfg(not(target_os = "android"))]
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

  /// Can *Disconnect* act on anything?
  ///
  /// The verb belongs to the portal. On a link that is not the campus portal
  /// (mobile data, another network) or behind someone else's captive portal
  /// there is no session of ours to end, so the button is greyed out exactly as
  /// it is on an 802.1X network. The account form stays enabled: editing the
  /// saved credential does not touch the portal.
  fn can_disconnect(&self) -> bool {
    self.portal_verbs() && !matches!(self.env, LoginEnv::OtherNetwork | LoginEnv::OtherPortal)
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
      // Same reading, different cause: the link is not the campus network at
      // all (home Wi-Fi, mobile data). It is named separately so the label does
      // not claim a campus session that never existed.
      State::Offline if self.env == LoginEnv::OtherNetwork => {
        (t.online_other_network, app_fill(State::Online))
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
    while let Some(action) = self.tray.poll() {
      match action {
        Action::Connect => self.send(Cmd::Connect),
        Action::Disconnect => self.send(Cmd::Disconnect),
        Action::Quit => self.quit(),
        Action::Show => self.show_window(ctx),
        Action::Hide => self.hide_window(ctx),
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
        Ev::State(s) => {
          if s != self.state {
            // A new state starts a new poll cadence (15 s online, 10 s offline),
            // so the last cycle's position cannot be carried into it: a Turtle
            // at the old period's end would sit still until the next beat.
            self.last_heartbeat = Some(Instant::now());
          }
          self.state = s;
        }
        Ev::Info(v) => {
          self.info = info_from(&v);
        }
        Ev::Beat => self.last_heartbeat = Some(Instant::now()),
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
      }
    }
  }

  fn paint(&mut self, ui: &mut egui::Ui) {
    #[cfg(target_os = "android")]
    {
      let (top, bottom) = android_insets();
      let ppp = ui.ctx().pixels_per_point();
      let top = top / ppp;
      let bottom = bottom / ppp;
      // Keep the footer above the navigation bar and the keyboard. The pages
      // align their bottom edge to the layout, so shortening it is what moves
      // the footer up.
      let height = (ui.available_height() - bottom).max(0.0);
      ui.set_max_height(height);
      ui.add_space(top);
    }
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
            ui.add_space(6.0);
            status_track(ui, self.state, self.last_heartbeat, self.turtle.as_ref(), t);
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
            // Right to left, so this lands to the *left* of the gear. The eye
            // is the status page's shortcut to privacy mode, which otherwise
            // only lives in the settings page.
            if self.page == Page::Status {
              let hidden = self.cfg.privacy;
              let (name, image, tip) = if hidden {
                (
                  "thunet_eye_closed",
                  eye_closed_image as fn() -> egui::ColorImage,
                  t.privacy_show,
                )
              } else {
                (
                  "thunet_eye_open",
                  eye_open_image as fn() -> egui::ColorImage,
                  t.privacy_hide,
                )
              };
              let slot = if hidden {
                &mut self.eye_closed
              } else {
                &mut self.eye_open
              };
              let eye = icon_texture(slot, ui.ctx(), name, image);
              if ui.add(icon_button(eye.id())).on_hover_text(tip).clicked() {
                self.cfg.privacy = !self.cfg.privacy;
                if let Err(e) = self.cfg.save() {
                  self.cfg.privacy = !self.cfg.privacy;
                  self.toast_err(e);
                }
              }
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
          let account = private_value(&account, self.cfg.privacy);
          egui::Grid::new("info")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
              ui.label(t.account);
              ui.horizontal(|ui| {
                ui.label(egui::RichText::new(account).monospace());
                self.device_badge(ui, t);
              });
              ui.end_row();
              ui.label(t.link);
              ui.label(egui::RichText::new(self.uplink.text(t)).monospace());
              ui.end_row();
              // The portal's address while it has an answer, otherwise the one
              // this machine holds on the current link: mobile data and other
              // networks have no portal row, and "—" would hide the only
              // address there is to copy.
              let ip4 = if self.info.ip4.is_empty() {
                self.local_ip4.clone()
              } else {
                self.info.ip4.clone()
              };
              let ip6 = if self.info.ip6.is_empty() {
                self.local_ip6.clone()
              } else {
                self.info.ip6.clone()
              };
              ui.label(t.ipv4);
              if address_cell(ui, private_value(&ip4, self.cfg.privacy), t) {
                crate::clipboard::copy(ui.ctx(), &ip4);
                self.toast_ok(t.copied.to_string());
              }
              ui.end_row();
              ui.label(t.ipv6);
              if address_cell(ui, private_value(&ip6, self.cfg.privacy), t) {
                crate::clipboard::copy(ui.ctx(), &ip6);
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

          // Drawn after the grid, on top of everything else: the popup floats
          // over the page instead of taking a row of its own.
          self.device_popup(ui.ctx(), t);

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
              // The core's intent changed, and the *Disconnect* button is
              // drawn from it: ask before the 30 s round is due.
              self.next_core_query = Instant::now();
            }
            // A core that still wants to be online is trying or waiting: while a
            // login attempt is in flight the state is *Authenticating* and this
            // button calls it off. Between attempts the page says *Offline*, and
            // there is no session to end - leaving the button lit there read as
            // "both verbs are available with nothing connected".
            let can_stop = can_stop(self.state, self.can_disconnect());
            if ui
              .add_enabled(can_stop, egui::Button::new(t.disconnect))
              .on_disabled_hover_text(
                self
                  .env
                  .blocked_reason(t)
                  .unwrap_or_else(|| t.disconnect.into()),
              )
              .clicked()
            {
              self.send(Cmd::Disconnect);
              self.next_core_query = Instant::now();
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
          #[cfg(not(target_os = "android"))]
          {
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
          }

          // Startup and session behaviour share one row: the settings page has
          // to fit the fixed window height. *Start minimised* is only
          // meaningful when the tray is part of the autostart mode.
          let mut auto_conn = self.cfg.auto_connect;
          let mut keep_on = self.cfg.keep_online;
          // `Start minimised` and the tray are desktop concepts; Android has
          // neither a tray nor a hidden start.
          #[cfg(not(target_os = "android"))]
          let mut start_min = self.cfg.start_minimized;
          #[cfg(not(target_os = "android"))]
          let tray_starts = !self.portable && self.autostart_mode != Mode::Core;
          // These three labels are the widest set on the page, and English is
          // the tighter one: a slightly narrower gap keeps them on one row in
          // both languages (`the_startup_switches_fit_on_one_row` pins it).
          ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
              #[cfg(not(target_os = "android"))]
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
              if ui
                .checkbox(&mut keep_on, t.keep_online)
                .on_hover_text(t.keep_online_hint)
                .changed()
              {
                self.cfg.keep_online = keep_on;
                self.send(Cmd::SetKeepOnline(keep_on));
                match self.cfg.save() {
                  Ok(()) => self.toast_toggle(t.keep_online, keep_on),
                  Err(e) => self.toast_err(e),
                }
              }
            });
          });
          // Android's background level. It mirrors the desktop's autostart
          // triad (nothing / core only / core + always-present entry point);
          // the service and the notification permission are the Activity's, so
          // the row asks for a level and shows what the Activity wrote back.
          #[cfg(target_os = "android")]
          {
            use crate::background::Mode;
            let mut mode = self.background;
            ui.horizontal(|ui| {
              ui.label(t.background_mode);
              for (value, label) in [
                (Mode::Off, t.background_off),
                (Mode::Wake, t.background_wake),
                (Mode::Service, t.background_service),
              ] {
                ui.radio_value(&mut mode, value, label);
              }
            });
            if mode != self.background {
              if let Err(e) = crate::background::set(mode) {
                self.toast_err(e);
              }
              // The resident level can open the permission dialog, so the
              // answer is the files, not the request.
              self.background = crate::background::Mode::read();
              self.background_denied = crate::background::denied();
            }
            // Touch has no hover: what the chosen level costs is written
            // inline, and a refused permission says why it fell back.
            let hint = if self.background_denied {
              t.background_denied
            } else {
              match self.background {
                Mode::Off => t.background_off_hint,
                Mode::Wake => t.background_wake_hint,
                Mode::Service => t.background_service_hint,
              }
            };
            ui.add(
              egui::Label::new(
                egui::RichText::new(hint)
                  .small()
                  .color(ui.visuals().weak_text_color()),
              )
              .wrap(),
            );
            // A swipe from recents is a force-stop on aggressive ROMs, and it
            // takes the foreground service with it. Only the recents card lock
            // prevents that, so the level that is supposed to survive a swipe
            // says what still needs doing on the device.
            if self.background == Mode::Service {
              ui.add(
                egui::Label::new(
                  egui::RichText::new(t.background_rom_hint)
                    .small()
                    .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
              );
            }
            // Asking for this is what turns "wakes on the next Wi-Fi" into
            // something that survives a swipe: without the exemption the ROM
            // may force-stop the app, which cancels the registration too.
            //
            // The two toggles that matter on MIUI are unreadable and are not
            // the same switch as the AOSP whitelist the dialog below changes,
            // so the row never claims an authorisation of its own: it opens the
            // right screens and keeps the user's confirmation.
            if self.background != Mode::Off {
              let mut authorize = false;
              ui.horizontal(|ui| {
                ui.label(t.background_power);
                authorize = ui.button(t.background_authorize).clicked();
              });
              // Readable, and it is the switch the firewall follows: without it
              // the system drops this app's packets in the background.
              let (state, color) = if self.background_power {
                (t.background_power_ok, ui.visuals().weak_text_color())
              } else {
                (t.background_power_missing, ui.visuals().warn_fg_color)
              };
              ui.add(egui::Label::new(egui::RichText::new(state).small().color(color)).wrap());
              let mut allowed = self.background_allowed;
              if ui
                .checkbox(&mut allowed, t.background_power_check)
                .changed()
              {
                self.background_allowed = allowed;
                if let Err(e) = crate::background::set_allowed(allowed) {
                  self.toast_err(e);
                }
              }
              let hint = if self.background_allowed {
                t.background_power_ready
              } else {
                t.background_power_hint
              };
              ui.add(
                egui::Label::new(
                  egui::RichText::new(hint)
                    .small()
                    .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
              );
              if authorize && let Err(e) = crate::background::request_power() {
                self.toast_err(e);
              }
            }
          }
          ui.add_space(2.0);
          ui.horizontal(|ui| {
            // Privacy is window-only state: it changes what is drawn, never
            // what is stored or sent, so it is saved here and nowhere else.
            let label = if self.cfg.privacy {
              t.privacy_show
            } else {
              t.privacy_hide
            };
            if ui.button(label).clicked() {
              self.cfg.privacy = !self.cfg.privacy;
              if let Err(e) = self.cfg.save() {
                self.cfg.privacy = !self.cfg.privacy;
                self.toast_err(e);
              }
            }
            #[cfg(not(target_os = "android"))]
            if ui.button(t.desktop_shortcut).clicked() {
              match crate::shortcut::create_desktop_shortcut() {
                Ok(()) => self.toast_ok(t.done.to_string()),
                Err(e) => {
                  self.push_log(Line::error(&e));
                  self.toast_err(e);
                }
              }
            }
            // Reading the Wi-Fi name needs the location permission, which only
            // the user can grant; the Activity asks when this is ticked.
            #[cfg(target_os = "android")]
            {
              let mut secure = self.secure_detection;
              if ui
                .checkbox(&mut secure, t.secure_detection)
                .on_hover_text(t.secure_detection_hint)
                .changed()
              {
                if let Err(e) = crate::secure_wifi::set(secure) {
                  self.toast_err(e);
                }
                // The permission dialog answers asynchronously, so the file is
                // the answer, not this call.
                self.secure_detection = crate::secure_wifi::enabled();
              }
            }
          });
          // Touch has no hover, and the permission requirement is the one thing
          // the checkbox cannot say by itself.
          #[cfg(target_os = "android")]
          ui.add(
            egui::Label::new(
              egui::RichText::new(t.secure_detection_hint)
                .small()
                .color(ui.visuals().weak_text_color()),
            )
            .wrap(),
          );

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
          #[cfg(target_os = "android")]
          let mut field_focus: Option<(AutofillKind, egui::Rect, bool)> = None;
          ui.horizontal(|ui| {
            let label_w = field_label_width(ui, t);
            field_label(ui, t.account, label_w);
            let id = ui.make_persistent_id("thunet-account-field");
            let focused = ui.ctx().memory(|m| m.has_focus(id));
            let mut edit = egui::TextEdit::singleline(&mut self.user_edit)
              .id(id)
              .char_limit(ACCOUNT_MAX_CHARS)
              .desired_width(FIELD_WIDTH);
            // Privacy mode keeps the account off the screen, but a masked edit
            // box could not be typed into: it unmasks while it has the focus.
            if self.cfg.privacy && !focused {
              edit = edit.password(true);
            }
            #[cfg_attr(not(target_os = "android"), allow(unused_variables))]
            let resp = ui.add_enabled(credentials_live, edit);
            // egui gets no `InputConnection` on Android; the bridges are the
            // long-press paste here and the virtual autofill view plus the
            // keyboard connection below.
            #[cfg(target_os = "android")]
            {
              if focused {
                field_focus = Some((AutofillKind::User, resp.rect, resp.clicked()));
              }
              if credentials_live {
                self.paste_sync(ui, id, AutofillKind::User, resp.rect);
              } else {
                // A disabled box is not a paste target: the form refuses the
                // credential on this network (802.1X), so a finger resting on
                // it must not put the clipboard into it either.
                self.paste.forget();
              }
            }
            if stored.username {
              unsaved_marker(ui, t);
            }
          });
          // Android has no hover, and *Apply* is the only control on this page
          // whose greyed-out state is not explained by the row itself. Its hint
          // is written underneath instead of in a tooltip (item J).
          #[cfg(target_os = "android")]
          let mut apply_hint: Option<&'static str> = None;
          ui.horizontal(|ui| {
            let label_w = field_label_width(ui, t);
            field_label(ui, t.password, label_w);
            let id = ui.make_persistent_id("thunet-password-field");
            // Only the autofill bridge needs this; nothing else in the row does.
            #[cfg(target_os = "android")]
            let focused = ui.ctx().memory(|m| m.has_focus(id));
            let keep_hint = core_password && self.pass_edit.is_empty();
            let mut input = egui::TextEdit::singleline(&mut self.pass_edit)
              .id(id)
              .password(true)
              .char_limit(PASSWORD_MAX_CHARS)
              .desired_width(FIELD_WIDTH);
            if keep_hint {
              // The core holds a password the window cannot show; an empty
              // field means "keep it" rather than "no password".
              input = input.hint_text(t.password_keep);
            }
            #[cfg_attr(not(target_os = "android"), allow(unused_variables))]
            let resp = ui.add_enabled(credentials_live, input);
            #[cfg(target_os = "android")]
            {
              if focused {
                field_focus = Some((AutofillKind::Password, resp.rect, resp.clicked()));
              }
              if credentials_live {
                self.paste_sync(ui, id, AutofillKind::Password, resp.rect);
              } else {
                self.paste.forget();
              }
            }
            if stored.password {
              unsaved_marker(ui, t);
            }
          });
          // One update per frame, after both fields: a switch is a single
          // enter (Java exits the previous virtual id itself), and the IME is
          // never hidden and re-shown for it.
          #[cfg(target_os = "android")]
          self.autofill_update(ui.ctx(), field_focus);
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
            #[cfg(target_os = "android")]
            if credentials_live && !enabled && stored.any() {
              apply_hint = Some(t.password_required);
            }
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
          #[cfg(target_os = "android")]
          if let Some(hint) = apply_hint {
            ui.add(
              egui::Label::new(
                egui::RichText::new(hint)
                  .small()
                  .color(ui.visuals().weak_text_color()),
              )
              .wrap(),
            );
          }
        }
        let visible: Vec<&Line> = self
          .logs
          .iter()
          .filter(|l| l.level >= self.min_level)
          .collect();
        // Masking happens here rather than when the line is stored, so the
        // toggle also reaches lines that were already in the pane.
        // Android's log hold lives in `self`, so the masking names are owned
        // there instead of borrowed: a `&self` borrow from this point would
        // exclude the pane closure that drives the gesture.
        #[cfg(target_os = "android")]
        let accounts = self
          .accounts()
          .into_iter()
          .map(str::to_owned)
          .collect::<Vec<String>>();
        #[cfg(target_os = "android")]
        let accounts = accounts.iter().map(String::as_str).collect::<Vec<_>>();
        #[cfg(not(target_os = "android"))]
        let accounts = self.accounts();
        let hidden = self.cfg.privacy;
        if self.page == Page::Status && !visible.is_empty() {
          ui.add_space(8.0);
          ui.separator();
          let everything = visible
            .iter()
            .map(|l| masked_log(l.render(), &accounts, hidden))
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
                  for (index, line) in visible.iter().enumerate() {
                    #[cfg(not(target_os = "android"))]
                    let _ = index;
                    let color = {
                      let visuals = ui.visuals();
                      level_color(line.level, visuals)
                    };
                    let text = masked_log(line.render(), &accounts, hidden);
                    let resp = ui.add(log_entry(ui, text.clone(), color));
                    // The hold target is the whole row, glyphs or not: a short
                    // line leaves most of the width empty, and a finger resting
                    // on that blank part is still aiming at this line.
                    #[cfg(target_os = "android")]
                    let row = egui::Rect::from_min_max(
                      egui::pos2(ui.max_rect().left(), resp.rect.top()),
                      egui::pos2(ui.max_rect().right(), resp.rect.bottom()),
                    );
                    // A finger on a line is tinted from the moment it lands, so
                    // the target of the long press is never a guess.
                    #[cfg(target_os = "android")]
                    let held = {
                      let fired = self.log_hold.poll(ui, index, row);
                      if self.log_hold.marked(index) {
                        let bg = ui.visuals().selection.bg_fill;
                        let tint =
                          egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 56);
                        ui.painter().rect_filled(
                          row.expand(1.0),
                          egui::CornerRadius::same(3),
                          tint,
                        );
                      }
                      fired
                    };
                    #[cfg(not(target_os = "android"))]
                    let held = false;
                    #[cfg(target_os = "android")]
                    let armed = self.log_hold.fired();
                    #[cfg(not(target_os = "android"))]
                    let armed = false;
                    #[cfg(target_os = "android")]
                    let spot = self.log_hold.spot();
                    #[cfg(not(target_os = "android"))]
                    let spot = None;
                    log_menu(
                      &resp,
                      t,
                      &text,
                      &everything,
                      &mut copy_request,
                      held,
                      armed,
                      spot,
                    );
                  }
                });
            },
          );
          LogCopyClick::set_log_rect(ui.ctx(), pane.response.rect);
          if let Some(text) = copy_request {
            crate::clipboard::copy(ui.ctx(), &text);
            self.push_log(Line::debug(format!("copied {} chars", text.len())));
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
                // The build stamp is desktop-width; on a 412 pt phone it
                // overflows the strip. The version on the left still answers
                // "which build is this".
                #[cfg(not(target_os = "android"))]
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

/// The device list sizes itself to its content: a short list must not open a
/// wide dialog with an empty right margin. The height is capped instead, with a
/// scroll bar for an account with many devices.
const DEVICES_WIDTH_MIN: f32 = 180.0;
const DEVICES_WIDTH_MAX: f32 = 360.0;
const DEVICES_SCROLL_MAX: f32 = 180.0;
/// The local dot, the device icon and the gaps between the columns.
const DEVICES_DOT: f32 = 8.0;
const DEVICES_ICON: f32 = 16.0;
const DEVICES_GAP: f32 = 8.0;
/// Room between the last column and the right edge, so a floating scroll bar
/// does not sit on top of the local dot.
const DEVICES_EDGE_PAD: f32 = 6.0;
/// Widest the system/type column may get before it is elided.
const DEVICES_KIND_MAX: f32 = 110.0;

impl App {
  /// The account's online-device count beside its name, and the click that
  /// opens the list.
  ///
  /// The portal lists the devices of the *account*, campus-wide, not this
  /// machine's session, so the count belongs next to the account. A drawn icon
  /// plus `×` with the count as a superscript fits in the row without eating
  /// the log pane; nothing is drawn while the account has no session, so an
  /// offline machine keeps the plain row it always had.
  fn device_badge(&mut self, ui: &mut egui::Ui, t: &Texts) {
    let n = self.info.devices_total;
    if n == 0 {
      // No session: no badge, and no list to keep open.
      self.devices_open = false;
      return;
    }
    let ink = ui.visuals().weak_text_color();
    // The count is written as a power on the icon's corner: smaller, raised,
    // and with no multiplication sign in between.
    let count = ui
      .painter()
      .layout_no_wrap(n.to_string(), egui::FontId::proportional(9.5), ink);
    let icon = 14.0;
    let gap = 2.0;
    let pad = 5.0;
    let width = pad * 2.0 + icon + gap + count.size().x;
    let (rect, resp) = ui.allocate_exact_size(
      egui::vec2(width, ui.spacing().interact_size.y.max(18.0)),
      egui::Sense::click(),
    );
    let resp = resp
      .on_hover_cursor(egui::CursorIcon::PointingHand)
      .on_hover_text(format!("{}{n}", t.devices_title));
    if resp.hovered() || self.devices_open {
      let fill = if self.devices_open {
        ui.visuals().selection.bg_fill
      } else {
        ui.visuals().widgets.hovered.bg_fill
      };
      ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(4), fill);
    }
    let icon_rect = egui::Rect::from_min_size(
      egui::pos2(rect.left() + pad, rect.center().y - icon / 2.0),
      egui::vec2(icon, icon),
    );
    paint_device_icon(
      ui.painter(),
      icon_rect,
      DeviceKind::Other,
      ink,
      ui.visuals().window_fill(),
    );
    // Level with the icon's top edge, just past its right side.
    ui.painter().galley(
      egui::pos2(icon_rect.right() + gap, icon_rect.top() - 2.5),
      count,
      ink,
    );
    if resp.clicked() {
      self.devices_open = !self.devices_open;
    }
  }

  /// The device list, as a modal.
  ///
  /// A modal instead of a floating area: egui dims the page behind it and
  /// swallows its clicks, which is what makes the list read as "look, then
  /// dismiss". The framework centres it, so no anchor arithmetic is needed.
  fn device_popup(&mut self, ctx: &egui::Context, t: &Texts) {
    if !self.devices_open {
      return;
    }
    let devices = &self.info.devices;
    let n = self.info.devices_total;
    let mut open = true;
    // The list cannot call back into `self`, so a copy click records the address
    // and the clipboard is written once the modal has finished laying out.
    let mut copy = None;
    let modal = egui::Modal::new(egui::Id::unique("thunet_devices_popup")).show(ctx, |ui| {
      devices_list(ui, devices, n, t, &mut open, self.cfg.privacy, &mut copy);
    });
    if let Some(text) = copy {
      crate::clipboard::copy(ctx, &text);
      self.toast_ok(t.copied.to_string());
    }
    // The dimmed backdrop and Escape are the framework's own dismissal, so the
    // close button is one more way out rather than the only one.
    if !open || modal.should_close() {
      self.devices_open = false;
    }
  }
}

/// What one device list occupied, so the dialog's width can be checked against
/// its content instead of being eyeballed.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
struct DevicesLayout {
  /// Width the columns asked for (the value handed to the body's max rect).
  planned: f32,
  /// Width of the body itself, close button and rule included.
  used: f32,
  /// Width of one row inside the scroll area.
  grid: f32,
}

/// How tall the rows inside the device dialog may be.
///
/// A desktop window is short, so a fixed cap is right there. A phone is tall and
/// portrait: most of the modal, but never the whole screen, so the page behind
/// still shows around it. The screen height is passed in rather than read from
/// the dialog, because a modal lays its content out with the size that content
/// had last frame - a fraction of that is a feedback loop, not a measurement.
fn devices_scroll_max(screen_h: f32, phone: bool) -> f32 {
  if phone {
    (screen_h * 0.6).clamp(DEVICES_SCROLL_MAX, 620.0)
  } else {
    DEVICES_SCROLL_MAX
  }
}

/// May the *Disconnect* button act right now?
///
/// Only a state above *Offline* has a session to end. The core's intent flag is
/// deliberately not part of this: while an automatic attempt is between retries
/// the page says *Offline*, and a lit-up *Disconnect* there looked like "both
/// verbs are available with nothing connected". A login that is actually in
/// flight shows *Authenticating*, which is where cancelling belongs.
fn can_stop(state: State, verbs: bool) -> bool {
  state != State::Offline && verbs
}

/// Draw the dialog body: the title row with its close button, then the rows.
///
/// The width is measured before anything is laid out. The title row, the rule
/// and the rows all size themselves from `available_width()`, so they are only
/// bound to it once they sit in a sub-ui whose max rect *is* that width.
fn devices_list(
  ui: &mut egui::Ui,
  devices: &[Device],
  n: usize,
  t: &Texts,
  open: &mut bool,
  privacy: bool,
  copy: &mut Option<String>,
) -> DevicesLayout {
  let title = format!("{}{n}", t.devices_title);
  let body = egui::TextStyle::Body.resolve(ui.style());
  let mono = egui::TextStyle::Monospace.resolve(ui.style());
  let measure = |text: &str, font: &egui::FontId| {
    ui.fonts_mut(|f| {
      f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
        .size()
        .x
    })
  };
  let mut addr_w = 0.0_f32;
  let mut kind_w = 0.0_f32;
  for d in devices {
    let addr = private_value(if d.ip4.is_empty() { &d.ip6 } else { &d.ip4 }, privacy);
    let kind = &d.class;
    addr_w = addr_w.max(measure(addr, &mono));
    kind_w = kind_w.max(measure(kind, &body).min(DEVICES_KIND_MAX));
  }
  // Each column gets what its widest entry needs, so an address is only elided
  // when the dialog hits its cap - never because a neighbouring column was
  // reserved for nothing. The copy mark closes the address column, so its slot
  // is reserved on top of the widest address.
  let addr_w = if devices.is_empty() {
    0.0
  } else {
    addr_w + DEVICES_GAP + DEVICES_ICON
  };
  let rows_w = DEVICES_EDGE_PAD
    + DEVICES_DOT
    + DEVICES_GAP
    + DEVICES_ICON
    + DEVICES_GAP
    + addr_w
    + if kind_w > 0.0 {
      DEVICES_GAP + kind_w
    } else {
      0.0
    };
  let header_w = measure(&title, &body) + DEVICES_GAP + 16.0;
  let width = rows_w
    .max(header_w)
    .clamp(DEVICES_WIDTH_MIN, DEVICES_WIDTH_MAX);

  // The body is bounded with an explicit max rect: `set_max_width` alone is a
  // no-op here, because a vertical layout with a centred cross axis makes every
  // widget fill the *available* width (`Layout::next_frame`), so the title row
  // and the separator kept stretching to the panel's width.
  //
  // The height may not come from this dialog either: a modal lays its content
  // out with the size that content had *last frame* as the max rect, so
  // `available_height()` here is the list's own previous height and any fraction
  // of it converges to itself (on a phone it shrank frame by frame). The screen
  // is the only honest bound.
  let avail = ui.available_rect_before_wrap();
  let screen = ui.ctx().content_rect();
  let body_rect = egui::Rect::from_min_size(avail.min, egui::vec2(width, screen.height()));
  let row_h = ui
    .text_style_height(&egui::TextStyle::Body)
    .max(ui.text_style_height(&egui::TextStyle::Monospace))
    .max(DEVICES_ICON);
  let mut grid = 0.0_f32;
  let body = ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
    ui.horizontal(|ui| {
      ui.label(egui::RichText::new(&title).strong());
      ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if close_button(ui).clicked() {
          *open = false;
        }
      });
    });
    ui.separator();
    if devices.is_empty() {
      return;
    }
    // The rows fill the body: the dots line up against the dialog's right edge
    // instead of floating after the shortest address.
    //
    // A desktop window is short, so a fixed cap is right there. A phone is tall
    // and portrait, and a 180 pt letterbox in the middle of the screen reads
    // badly: the list takes most of the modal instead, while `auto_shrink`
    // still keeps a two-row list from stretching.
    let scroll_max = devices_scroll_max(screen.height(), cfg!(target_os = "android"));
    egui::ScrollArea::vertical()
      .max_height(scroll_max)
      .auto_shrink([false, true])
      .show(ui, |ui| {
        for d in devices {
          let ink = ui.visuals().text_color();
          let bg = ui.visuals().window_fill();
          let row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = DEVICES_GAP;
            let (icon, _) =
              ui.allocate_exact_size(egui::vec2(DEVICES_ICON, DEVICES_ICON), egui::Sense::hover());
            paint_device_icon(ui.painter(), icon, device_kind(&d.os, &d.class), ink, bg);
            fixed_cell(ui, (&d.id, "addr"), addr_w, row_h, |ui| {
              let raw = if d.ip4.is_empty() { &d.ip6 } else { &d.ip4 };
              let addr = private_value(raw, privacy);
              // The mark is laid out first in a right-to-left row, so it sits on
              // the column's right edge; the address then gets what is left and
              // elides there (`truncate` shows the whole value on hover). It
              // copies the real address even while privacy mode masks it, the
              // way the status page's *Copy* button does.
              ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if copy_mark(ui, t).clicked() {
                  *copy = Some(raw.to_string());
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                  ui.add(egui::Label::new(egui::RichText::new(addr).monospace()).truncate());
                });
              });
            });
            if kind_w > 0.0 {
              fixed_cell(ui, (&d.id, "kind"), kind_w, row_h, |ui| {
                // The icon says which system the device runs; this column is
                // the device type the portal reports (`class_name`), nothing
                // else.
                ui.add(egui::Label::new(&d.class).truncate());
              });
            }
            // The dot closes the row in a column of its own, so the marks line
            // up on the right edge instead of floating after a short address.
            // The space goes first in a right-to-left row, which is what puts
            // it outside the dot rather than inside it.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
              ui.add_space(DEVICES_EDGE_PAD);
              let (dot, _) =
                ui.allocate_exact_size(egui::vec2(DEVICES_DOT, DEVICES_DOT), egui::Sense::hover());
              if d.local {
                ui.painter()
                  .circle_filled(dot.center(), 3.0, ui.visuals().selection.bg_fill);
              }
            });
          });
          grid = grid.max(row.response.rect.width());
        }
      });
  });
  DevicesLayout {
    planned: width,
    // The body's own rect, not `min_rect`: a panel's ui starts with a min rect
    // as wide as the panel, so it says nothing about the content.
    used: body.response.rect.width(),
    grid,
  }
}

/// Reserve exactly `w` × `h` in the row, then draw inside it.
///
/// The reservation is what keeps the columns aligned: a sub-ui sized from its
/// content would let a short address pull every later column left. The child is
/// made with `new_child`, which does not advance the cursor a second time.
fn fixed_cell(
  ui: &mut egui::Ui,
  id_salt: impl std::hash::Hash + std::fmt::Debug,
  w: f32,
  h: f32,
  add: impl FnOnce(&mut egui::Ui),
) {
  let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
  let mut cell = ui.new_child(
    egui::UiBuilder::new()
      .id_salt(id_salt)
      .max_rect(rect)
      .layout(egui::Layout::left_to_right(egui::Align::Center)),
  );
  add(&mut cell);
}

/// The copy mark on the right edge of one device row's address column.
///
/// A painted glyph instead of the *Copy* text button the settings page uses:
/// the row is only one line tall, so a label would not fit. The caller lays it
/// out first in a right-to-left row, which is what pins it to the edge.
fn copy_mark(ui: &mut egui::Ui, t: &Texts) -> egui::Response {
  let (rect, resp) =
    ui.allocate_exact_size(egui::vec2(DEVICES_ICON, DEVICES_ICON), egui::Sense::click());
  let resp = resp
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text(t.copy);
  let ink = if resp.hovered() {
    ui.visuals().strong_text_color()
  } else {
    ui.visuals().text_color()
  };
  paint_copy_icon(ui.painter(), rect, ink, ui.visuals().window_fill());
  resp
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

/// The service row's status: "running v0.1.5 · 1m", or why nothing answers.
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

/// Longest account the portal accepts.
const ACCOUNT_MAX_CHARS: usize = 10;
/// Longest password the portal accepts.
const PASSWORD_MAX_CHARS: usize = 64;

/// The two virtual autofill ids, shared with `ThunetActivity.AutofillHost`.
#[cfg(target_os = "android")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum AutofillKind {
  User,
  Password,
}

#[cfg(target_os = "android")]
impl AutofillKind {
  /// Must match `AutofillHost.USER` / `AutofillHost.PASSWORD` on the Java side.
  fn id(self) -> i32 {
    match self {
      Self::User => 1,
      Self::Password => 2,
    }
  }
}

/// A finger resting on a credential field, for the clipboard paste.
#[cfg(target_os = "android")]
#[derive(Default)]
struct PasteHold {
  /// When the current hold began.
  started: Option<f64>,
  /// The paste already fired for this press.
  fired: bool,
}

#[cfg(target_os = "android")]
impl PasteHold {
  /// How long a finger stays on the field before the clipboard is read.
  const HOLD: f64 = 0.5;
  /// Movement that turns a hold into a drag (the text edit's own selection).
  const SLOP: f32 = 12.0;

  /// True once per hold on `rect`, after the finger has rested there.
  fn poll(&mut self, ui: &egui::Ui, rect: egui::Rect) -> bool {
    let (down, origin, latest, now) = ui.input(|i| {
      (
        i.pointer.primary_down(),
        i.pointer.press_origin(),
        i.pointer.latest_pos(),
        i.time,
      )
    });
    if !down {
      self.started = None;
      self.fired = false;
      return false;
    }
    let (Some(origin), Some(latest)) = (origin, latest) else {
      return false;
    };
    if !rect.contains(origin) || (latest - origin).length() > Self::SLOP {
      return false;
    }
    if self.fired {
      return false;
    }
    // A finger held still produces no input events, so the frame that would
    // notice the hold needs an explicit pulse.
    ui.ctx().request_repaint_after(Duration::from_millis(100));
    let started = *self.started.get_or_insert(now);
    if now - started < Self::HOLD {
      return false;
    }
    self.fired = true;
    true
  }

  /// Drop a hold in progress, e.g. because the field under the finger was
  /// disabled while it was being held.
  fn forget(&mut self) {
    self.started = None;
    self.fired = false;
  }
}

/// A finger resting on a log line, for the copy menu.
///
/// Shorter than egui's own 0.8 s long touch: a log line is a small target and
/// the menu is the only thing a hold can do. The line under the finger is
/// tinted so the target is obvious, and the hold ticks the phone.
#[cfg(target_os = "android")]
#[derive(Default)]
struct LogHold {
  /// Line index under the finger, and when it first landed there.
  pressed: Option<(usize, f64)>,
  /// The line whose menu is open (or just opened).
  kept: Option<usize>,
  /// Where the press that opened the menu landed, in points. The popup is
  /// anchored here: `at_pointer_fixed` has no pointer to read on the release
  /// frame (a finger leaves no hover), and the menu would be open but unpainted.
  spot: Option<egui::Pos2>,
  /// The tick already fired for this press.
  fired: bool,
}

#[cfg(target_os = "android")]
impl LogHold {
  /// How long a finger stays on a line before its menu opens.
  const HOLD: f64 = 0.45;
  /// Movement that turns a hold into a scroll.
  const SLOP: f32 = 12.0;

  /// True on the frame a hold on line `index` matures.
  fn poll(&mut self, ui: &egui::Ui, index: usize, rect: egui::Rect) -> bool {
    let (down, origin, latest, now) = ui.input(|i| {
      (
        i.pointer.primary_down(),
        i.pointer.press_origin(),
        i.pointer.latest_pos(),
        i.time,
      )
    });
    if !down {
      // `fired` deliberately survives the release: that release still counts
      // as a click in egui (the press was shorter than 0.8 s), and the menu
      // must not close on it.
      self.pressed = None;
      return false;
    }
    if self.pressed.is_none() {
      // A new press starts; the previous one's hold no longer applies.
      self.fired = false;
    }
    let (Some(origin), Some(latest)) = (origin, latest) else {
      return false;
    };
    if !rect.contains(origin) || (latest - origin).length() > Self::SLOP {
      return false;
    }
    if self.fired {
      return false;
    }
    // A finger held still produces no input events, so the frame that would
    // notice the hold needs an explicit pulse.
    ui.ctx().request_repaint_after(Duration::from_millis(80));
    let started = match self.pressed {
      Some((on, started)) if on == index => started,
      _ => {
        self.pressed = Some((index, now));
        now
      }
    };
    if now - started < Self::HOLD {
      return false;
    }
    self.fired = true;
    self.kept = Some(index);
    self.spot = Some(origin);
    let _ = crate::android_jni::haptic();
    true
  }

  /// Where the menu should be anchored, if a hold opened one.
  fn spot(&self) -> Option<egui::Pos2> {
    self.spot
  }

  /// Should line `index` be tinted right now?
  fn marked(&self, index: usize) -> bool {
    self.kept == Some(index) || self.pressed.is_some_and(|(on, _)| on == index)
  }

  /// Did the press that is (or just was) down open a menu?
  fn fired(&self) -> bool {
    self.fired
  }

  /// Forget the kept line once its menu has closed.
  fn clear_if_closed(&mut self, any_popup_open: bool) {
    if !any_popup_open {
      self.kept = None;
      self.spot = None;
    }
  }
}

#[cfg(target_os = "android")]
impl App {
  /// Keep the autofill framework's copy of the form in step with egui.
  ///
  /// The field only exists on the surface, so Java is told where it is (in
  /// physical pixels, which is what the service expects) and what it holds.
  /// The value is resent only when it changes: the save prompt depends on
  /// `notifyValueChanged`, but a frame-by-frame repeat is noise.
  fn autofill_update(
    &mut self,
    ctx: &egui::Context,
    focus: Option<(AutofillKind, egui::Rect, bool)>,
  ) {
    let Some((kind, rect, clicked)) = focus else {
      if self.autofill_focus.is_some() {
        self.autofill_focus = None;
        self.autofill_value.clear();
        self.autofill_rect = None;
        let _ = crate::android_jni::autofill_leave();
      }
      return;
    };
    let value = match kind {
      AutofillKind::User => self.user_edit.clone(),
      AutofillKind::Password => self.pass_edit.clone(),
    };
    let ppp = ctx.pixels_per_point();
    let min = rect.min.to_vec2() * ppp;
    let size = rect.size() * ppp;
    let px = [
      min.x.round() as i32,
      min.y.round() as i32,
      size.x.round() as i32,
      size.y.round() as i32,
    ];
    // `notifyViewEntered` starts a fill request: once when the field gains the
    // focus, and again only when the user taps it (a deliberate "show me the
    // suggestions again", because the system drops its response when the app
    // goes to the background). Window focus changes are ignored on purpose —
    // the dropdown itself takes that focus away, and reacting to it made the
    // field leave and re-enter, so the dropdown kept flashing. Switching fields
    // is also one enter, so Java never hides the keyboard in between.
    if self.autofill_focus != Some(kind) || clicked {
      self.autofill_focus = Some(kind);
      self.autofill_value = value.clone();
      self.autofill_rect = Some(px);
      let _ = crate::android_jni::autofill_enter(kind.id(), px[0], px[1], px[2], px[3], &value);
    } else {
      if self.autofill_rect != Some(px) {
        self.autofill_rect = Some(px);
        let _ = crate::android_jni::autofill_bounds(kind.id(), px[0], px[1], px[2], px[3]);
      }
      if self.autofill_value != value {
        self.autofill_value = value.clone();
        let _ = crate::android_jni::autofill_value(kind.id(), &value);
      }
    }
  }

  /// Long press on a credential field pastes the system clipboard into it.
  ///
  /// The field holds one value, so the pasted text replaces what is there
  /// instead of being inserted at a caret the user cannot see on a phone.
  fn paste_sync(&mut self, ui: &egui::Ui, id: egui::Id, kind: AutofillKind, rect: egui::Rect) {
    if !self.paste.poll(ui, rect) {
      return;
    }
    let Some(text) = crate::android_jni::clipboard_text() else {
      return;
    };
    let text = text.trim();
    if text.is_empty() {
      return;
    }
    let limit = match kind {
      AutofillKind::User => ACCOUNT_MAX_CHARS,
      AutofillKind::Password => PASSWORD_MAX_CHARS,
    };
    let text: String = text.chars().take(limit).collect();
    match kind {
      AutofillKind::User => self.user_edit = text.to_owned(),
      AutofillKind::Password => self.pass_edit = text.to_owned(),
    }
    self.toast = None;
    ui.ctx().memory_mut(|memory| memory.request_focus(id));
  }
}

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

/// The log row's menu: a right click on the desktop, a long press on a phone.
///
/// [`egui::Response::context_menu`] only opens on `secondary_clicked`, and a
/// touch hold is a *primary* click in egui (`long_touched`, which also sets
/// `clicked`), so the popup is opened here for both triggers instead.
fn log_menu(
  resp: &egui::Response,
  t: &Texts,
  text: &str,
  everything: &str,
  copy_request: &mut Option<String>,
  held: bool,
  armed: bool,
  spot: Option<egui::Pos2>,
) {
  let mut popup =
    egui::Popup::menu(resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
  if resp.secondary_clicked() || resp.long_touched() || held || (armed && resp.clicked()) {
    // `armed` is the release of the press that opened this menu. It is still a
    // click in egui (the press was shorter than 0.8 s), and `Popup::menu`
    // toggles on clicks, so the release has to be forced back to "open".
    popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
  } else if resp.clicked() {
    // A plain tap or click on the row closes the menu, exactly as
    // `context_menu` does.
    popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(false)));
  }
  // The menu is anchored at the press point, never at the pointer: opening a
  // `PointerFixed` popup re-reads the pointer position, and a lifted finger
  // reports none, so the release frame would leave the menu open but unpainted.
  popup = match spot {
    Some(pos) => popup.at_position(pos),
    None => popup.at_pointer_fixed(),
  };
  popup.show(|ui| {
    if ui.button(t.copy_line).clicked() {
      *copy_request = Some(text.to_owned());
      ui.close();
    }
    if ui.button(t.copy_all).clicked() {
      *copy_request = Some(everything.to_owned());
      ui.close();
    }
  });
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

  /// The three switches share one row in a fixed-width window, so the labels
  /// have to fit in both languages. This pins it instead of trusting that they
  /// look fine: the row may not be wider than the page's inner width.
  #[test]
  fn the_startup_switches_fit_on_one_row() {
    for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Zh] {
      let t = lang.texts();
      let ctx = egui::Context::default();
      let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
          egui::Pos2::ZERO,
          egui::vec2(DEFAULT_WIDTH, WINDOW_HEIGHT),
        )),
        ..Default::default()
      };
      let mut row = 0.0;
      let mut out = ctx.run_ui(input, |ui| {
        egui::Frame::new()
          .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 14,
            bottom: 8,
          })
          .show(ui, |ui| {
            let mut start_min = true;
            let mut auto_conn = true;
            let mut keep_on = true;
            row = ui
              .horizontal(|ui| {
                ui.checkbox(&mut start_min, t.start_minimized);
                ui.checkbox(&mut auto_conn, t.auto_connect);
                ui.checkbox(&mut keep_on, t.keep_online);
              })
              .response
              .rect
              .width();
          });
      });
      out.textures_delta.clear();
      let inner = DEFAULT_WIDTH - 28.0;
      assert!(
        row <= inner,
        "{lang:?}: the three switches need {row} px, the page has {inner}"
      );
    }
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
      LoginEnv::OtherNetwork,
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
    // Direct internet behind the campus portal (the portal answers, but names
    // no session) stays a warning: it is still the campus link, and a session
    // is exactly what is missing.
    assert_eq!(
      tones(LoginEnv::NoPortal, State::Offline, None),
      vec![BannerTone::Warn]
    );
    // A link that is not the campus network at all is not a warning: the
    // machine is online, just not through the portal. Green, like 802.1X.
    assert_eq!(
      tones(LoginEnv::OtherNetwork, State::Offline, None),
      vec![BannerTone::Ok]
    );
    assert_eq!(tones(LoginEnv::OtherNetwork, State::Online, None), vec![]);
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

  /// The walk is a full period long and never wraps.
  #[test]
  fn heartbeat_tracks_one_poll_period() {
    let period = Duration::from_secs(15);
    assert_eq!(heartbeat_fraction(Duration::ZERO, period), 0.0);
    assert!((heartbeat_fraction(Duration::from_secs_f32(7.5), period) - 0.5).abs() < 0.01);
    assert_eq!(heartbeat_fraction(period, period), 1.0);
    assert_eq!(heartbeat_fraction(Duration::from_secs(40), period), 1.0);
    assert_eq!(
      heartbeat_fraction(Duration::from_secs(1), Duration::ZERO),
      0.0
    );
  }

  /// The turtle walks the whole track and stops at both ends of it.
  #[test]
  fn the_turtle_stays_on_its_track() {
    assert_eq!(turtle_x(0.0, 100.0, 18.0), 0.0);
    assert!((turtle_x(0.5, 100.0, 18.0) - 41.0).abs() < 1e-3);
    assert_eq!(turtle_x(1.0, 100.0, 18.0), 82.0);
    assert_eq!(turtle_x(3.0, 100.0, 18.0), 82.0, "no walking past the end");
    assert_eq!(turtle_x(-1.0, 100.0, 18.0), 0.0);
  }

  /// Privacy mode replaces the account and the addresses with dots; addresses
  /// keep their family's length, and an empty value or the "no session" dash
  /// stay as they are.
  #[test]
  fn private_values_are_masked() {
    assert_eq!(private_value("192.0.2.10", true), MASK_IP4);
    assert_eq!(private_value("2001:db8::1", true), MASK_IP6);
    assert_eq!(private_value("1234567890", true), MASK);
    assert_eq!(private_value("1234567890", false), "1234567890");
    assert_eq!(private_value("", true), "");
    assert_eq!(private_value("—", true), "—");
    assert_eq!(MASK_IP4.chars().count(), 9);
    assert!(MASK_IP6.chars().count() > MASK_IP4.chars().count());
  }

  /// Privacy turns the account inside a log line into dots, but only where it
  /// stands as a token of its own: the sentence around it stays readable, and a
  /// name that happens to be a prefix of another word is left alone.
  #[test]
  fn logs_mask_the_account_only_as_a_whole_token() {
    let accounts = ["1234567890", "alice"];

    // The portal echoes the account in some answers; the line keeps its shape.
    let echo = Line::info(r#"logged out: {"error":"ok","user_name":"1234567890"}"#);
    let plain = masked_log(echo.render(), &accounts, false);
    assert_eq!(
      plain,
      echo.render(),
      "nothing is hidden while the toggle is off"
    );
    let hidden = masked_log(echo.render(), &accounts, true);
    assert!(!hidden.contains("1234567890"), "{hidden}");
    assert!(hidden.contains(MASK), "{hidden}");
    assert!(hidden.contains("user_name\":\""), "{hidden}");

    // The mismatch warning names both accounts, and both have to go.
    let warning = Line::warn("logout: the portal reports alice, but 1234567890 is saved");
    let shown = masked_log(warning.render(), &accounts, true);
    assert!(!shown.contains("alice"), "{shown}");
    assert!(!shown.contains("1234567890"), "{shown}");
    assert!(
      shown.contains("[WARN] logout: the portal reports"),
      "{shown}"
    );

    // A prefix inside a longer word is not the account.
    let bobby = Line::warn("bobby is a different name");
    assert_eq!(masked_log(bobby.render(), &["bob"], true), bobby.render());

    // Empty names would otherwise match everywhere.
    let quiet = Line::info("login ok");
    assert_eq!(masked_log(quiet.render(), &["", " "], true), quiet.render());
  }

  /// The operating system's account is private too: it arrives in paths rather
  /// than as a sentence of its own.
  #[test]
  fn logs_mask_the_system_account_in_paths() {
    let name = os_account();
    if name.is_empty() {
      return; // a stripped environment has nothing to hide
    }
    let line = Line::info(format!("config: /home/{name}/.config/thunet/config.json"));
    let shown = masked_log(line.render(), &[name], true);
    assert!(!shown.contains(name), "{shown}");
    assert!(shown.contains("/.config/thunet/config.json"), "{shown}");
  }

  /// The account badge counts what the portal lists and marks this machine.
  /// Documentation addresses (RFC 5737 / RFC 3849) and made-up session ids.
  #[test]
  fn the_account_lists_its_devices_with_this_machine_first() {
    let v: Value = serde_json::from_str(
      r#"{
        "online_ip": "192.0.2.10", "online_ip6": "2001:db8::1",
        "user_name": "1234567890", "bytes_in": 1, "bytes_out": 2,
        "online_device_total": "3",
        "online_device_detail": {
          "1221087022": {"class_name": "", "ip": "198.51.100.7", "ip6": "::", "os_name": ""},
          "1052067690": {"class_name": "Smartphones/PDAs/Tablets", "ip": "192.0.2.10",
                         "ip6": "2001:db8::1", "os_name": "AndroidOS"},
          "1222833864": {"class_name": "", "ip": "::", "ip6": "2001:db8::2", "os_name": ""}
        }
      }"#,
    )
    .unwrap();
    let info = info_from(&v);
    assert_eq!(info.devices_total, 3);
    assert_eq!(info.devices.len(), 3);
    // This machine first, then an IPv4 address, then IPv6-only.
    assert!(info.devices[0].local);
    assert_eq!(info.devices[0].ip4, "192.0.2.10");
    assert_eq!(info.devices[0].os, "AndroidOS");
    assert_eq!(info.devices[1].ip4, "198.51.100.7");
    assert_eq!(info.devices[2].ip6, "2001:db8::2");
    assert!(!info.devices[1].local);

    // A device with no address at all cannot be shown and is dropped, while
    // the portal's own total still says how many there are.
    let v: Value = serde_json::from_str(
      r#"{"online_device_total":"2","online_device_detail":{
            "1":{"ip":"::","ip6":"::"},"2":{"ip":"192.0.2.11","ip6":"::"}}}"#,
    )
    .unwrap();
    let info = info_from(&v);
    assert_eq!(info.devices.len(), 1);

    // An answer without the detail (an older core, or offline) still carries
    // the count, and no list.
    let v: Value = serde_json::from_str(r#"{"online_device_total":"2"}"#).unwrap();
    let info = info_from(&v);
    assert_eq!(info.devices_total, 2);
    assert!(info.devices.is_empty());
  }

  /// The dialog must be no wider than the columns it shows. A separator or a
  /// scroll area that quietly stretches it leaves the close button and the rule
  /// hanging in the middle of the dialog, which is exactly what happened while
  /// the body was laid out with a minimum width only: a vertical layout with a
  /// centred cross axis stretches every widget to the *available* width, so the
  /// body has to be bounded with a max rect of its own.
  #[test]
  fn the_device_dialog_is_only_as_wide_as_its_rows() {
    let t = crate::i18n::Lang::En.texts();
    // Documentation addresses (RFC 5737) of the same length as real ones.
    let devices = vec![
      Device {
        id: "1".into(),
        ip4: "192.0.2.191".into(),
        os: "AndroidOS".into(),
        local: true,
        ..Default::default()
      },
      Device {
        id: "2".into(),
        ip4: "198.51.100.241".into(),
        ..Default::default()
      },
      Device {
        id: "3".into(),
        ip4: "203.0.113.138".into(),
        ..Default::default()
      },
    ];
    let ctx = egui::Context::default();
    let input = egui::RawInput {
      screen_rect: Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(DEFAULT_WIDTH, WINDOW_HEIGHT),
      )),
      ..Default::default()
    };
    let mut layout = None;
    let mut out = ctx.run_ui(input, |ui| {
      egui::CentralPanel::default().show(ui, |ui| {
        let mut open = true;
        let mut copied = None;
        layout = Some(devices_list(
          ui,
          &devices,
          devices.len(),
          t,
          &mut open,
          false,
          &mut copied,
        ));
      });
    });
    out.textures_delta.clear();
    let layout = layout.expect("the list is laid out at least once");
    assert!(
      layout.grid <= layout.planned + 0.5,
      "the row grid is wider than the dialog: {layout:?}"
    );
    assert!(
      layout.used <= layout.planned + 0.5,
      "the body used more width than it planned: {layout:?}"
    );
  }

  #[test]
  fn disconnect_is_off_while_the_page_says_offline() {
    // Nothing to end while offline, even if the core still wants to be online:
    // an automatic attempt between retries keeps the state Offline, and both
    // verbs lit up there is what the phone showed.
    assert!(!can_stop(State::Offline, true));
    // A login in flight can be called off.
    assert!(can_stop(State::Authenticating, true));
    assert!(can_stop(State::Online, true));
    // A link this app does not manage never offers the verb, whatever the state.
    assert!(!can_stop(State::Online, false));
    assert!(!can_stop(State::Authenticating, false));
  }

  #[test]
  fn a_phone_gives_the_device_list_more_room_than_a_desktop() {
    // The desktop cap stays what it was.
    assert_eq!(devices_scroll_max(890.0, false), DEVICES_SCROLL_MAX);
    // A phone gets most of its screen, but not all of it, and never less than
    // the desktop cap.
    let phone = devices_scroll_max(890.0, true);
    assert!(phone > DEVICES_SCROLL_MAX * 2.0, "{phone}");
    assert!(phone < 890.0 * 0.75, "{phone}");
    // A short phone screen still shows more than the desktop letterbox.
    assert_eq!(devices_scroll_max(200.0, true), DEVICES_SCROLL_MAX);
    // And the cap holds on a tablet.
    assert_eq!(devices_scroll_max(2000.0, true), 620.0);
  }

  #[test]
  fn the_device_dialog_height_does_not_follow_itself() {
    // A modal lays its content out with the height it had last frame. Reading
    // `available_height()` and taking a fraction of it made the list converge
    // to its own size, and on a phone it shrank frame after frame.
    let t = crate::i18n::Lang::En.texts();
    let devices: Vec<Device> = (0..20)
      .map(|i| Device {
        id: i.to_string(),
        ip4: format!("192.0.2.{}", i + 1),
        os: "AndroidOS".into(),
        ..Default::default()
      })
      .collect();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
      screen_rect: Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(400.0, 890.0),
      )),
      ..Default::default()
    };
    let mut heights = Vec::new();
    for _ in 0..3 {
      let mut height = 0.0;
      let mut out = ctx.run_ui(input.clone(), |ui| {
        let modal = egui::Modal::new(egui::Id::new("devices")).show(ui.ctx(), |ui| {
          let mut open = true;
          let mut copied = None;
          devices_list(
            ui,
            &devices,
            devices.len(),
            t,
            &mut open,
            false,
            &mut copied,
          );
          ui.min_rect().height()
        });
        height = modal.inner;
      });
      out.textures_delta.clear();
      heights.push(height);
    }
    // The first frame has no text metrics yet, so only the steady state has to
    // hold still - the bug was that it kept changing after that.
    assert!(
      heights[1] == heights[2],
      "the dialog changed height between frames: {heights:?}"
    );
    assert!(
      heights[2] > DEVICES_SCROLL_MAX + 20.0,
      "the rows did not take the cap: {heights:?}"
    );
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
      // The internet works without a portal in the way, and the link is not the
      // campus network at all: home Wi-Fi, mobile data. Nothing to log in to is
      // good news, so it gets the same green band as an 802.1X network instead
      // of the amber "nothing to log in to" warning.
      LoginEnv::OtherNetwork if state != State::Online => {
        banners.push((BannerTone::Ok, t.online_no_portal.to_string()))
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
  // again — a leftover portal session, which outlives a core restart because
  // nothing ends it, hides exactly that. Information, not a failure, so it uses
  // the amber band either way.
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
    if self.turtle.is_none() {
      self.turtle = load_turtle(ctx);
    }
    // A phone parks the event loop while the app is in the background and
    // resumes it on return. The focus edge is the one local trace of that, and
    // it is what makes "the session survived being hidden" visible in the log.
    let focused = ctx.input(|i| i.focused);
    if focused && !self.was_focused {
      self.push_log(Line::debug("app: window focused"));
    }
    self.was_focused = focused;
    // The autofill service answers from Java; wake the loop when it does and
    // apply whichever fields it filled (a null side means "untouched").
    #[cfg(target_os = "android")]
    {
      crate::android::remember_context(ctx);
      if let Some((user, pass)) = crate::android::take_autofill_result() {
        if let Some(user) = user {
          self.user_edit = user.chars().take(ACCOUNT_MAX_CHARS).collect();
        }
        if let Some(pass) = pass {
          self.pass_edit = pass.chars().take(PASSWORD_MAX_CHARS).collect();
        }
        self.toast = None;
      }
      // A field that is no longer on screen must not keep an autofill session
      // (or a dropdown hovering over the status page) alive.
      if self.page != Page::Settings && self.autofill_focus.is_some() {
        self.autofill_focus = None;
        self.autofill_value.clear();
        self.autofill_rect = None;
        let _ = crate::android_jni::autofill_leave();
      }
      // The soft keyboard talks to the Activity over an `InputConnection`;
      // composition, commits and deletions arrive here and join egui's event
      // stream, where the focused `TextEdit` applies them.
      let ime = crate::android::take_ime_events();
      if !ime.is_empty() {
        ctx.input_mut(|input| input.events.extend(ime.into_iter().map(egui::Event::Ime)));
      }
      // The hold's tint follows its menu, so forget the line once it closes.
      self.log_hold.clear_if_closed(egui::Popup::is_any_open(ctx));
    }
    // The system back gesture (edge swipe, or the button) is answered by the
    // window when there is a step to take: an open menu, the switch
    // confirmation, the device list, or the settings page. Java asks once per
    // gesture through `Native.backPressed`; a "no" leaves the task in the
    // background, so the capability is published again every pass.
    #[cfg(target_os = "android")]
    {
      // egui only notices a long touch (the log menu, the fields' paste) on a
      // frame, and a finger held still produces no input events; without this
      // pulse the release is simply too late to count as a click.
      if ctx.input(|i| i.pointer.primary_down()) {
        ctx.request_repaint_after(Duration::from_millis(100));
      }
      if crate::android::take_back() {
        if egui::Popup::is_any_open(ctx) {
          egui::Popup::close_all(ctx);
        } else if self.confirm_switch.is_some() {
          self.confirm_switch = None;
        } else if self.devices_open {
          self.devices_open = false;
        } else if self.page == Page::Settings {
          self.page = Page::Status;
        }
      }
      crate::android::set_can_go_back(
        egui::Popup::is_any_open(ctx)
          || self.confirm_switch.is_some()
          || self.devices_open
          || self.page == Page::Settings,
      );
    }
    // The stored theme is pushed here rather than in `App::new`, which runs
    // before there is a context to apply it to. `applied_theme` remembers what
    // was sent, so this is a no-op on the frames in between. It holds the
    // resolved theme, not the setting, so `auto` also picks up a light/dark
    // switch of the desktop itself.
    let theme = theme_preference(&self.cfg.theme);
    if self.applied_theme != Some(theme) {
      ctx.set_theme(theme);
      self.applied_theme = Some(theme);
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

    // The machine-wide request reaches windows in other sessions, and unlike
    // the per-user doorbell below it does not need a core to relay it. Portable
    // windows are left alone by an uninstall, so they never look. It is checked
    // first because an installer rings both: the answer — this window's state —
    // has to leave before the doorbell ends the process. It says nothing when
    // the sender is gone, which is why the doorbell below still runs.
    if !self.quitting
      && !crate::ipc::portable()
      && crate::single::machine_quit_requested(!self.shown)
    {
      self.quit();
    }

    // An installer (or `thunet quit`) asks for a real exit, not the tray's
    // hide-on-close: it needs the image gone before it replaces the file.
    if !self.quitting && crate::single::quit_requested() {
      self.quit();
    }

    // The core may have been restarted (crash, upgrade, systemd unit or logon
    // task): reconnect instead of going deaf.
    self.ensure_link();

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
      // The address the machine holds on that link. Two route lookups, no
      // packet sent; cheap enough for the same tick the link kind is read on.
      self.local_ip4 = crate::srun::link_id()
        .map(|ip| ip.to_string())
        .unwrap_or_default();
      self.local_ip6 = crate::srun::link_id6()
        .map(|ip| ip.to_string())
        .unwrap_or_default();
      // The Activity rewrites the night mode when the system theme changes;
      // re-reading it on the same slow tick keeps the per-frame theme check
      // free of file access.
      #[cfg(target_os = "android")]
      crate::theme::refresh(ctx);
      // The keep-alive state is owned by the Activity (the permission dialog
      // answers asynchronously), so the rows follow the files, not the click.
      #[cfg(target_os = "android")]
      {
        self.background = crate::background::Mode::read();
        self.background_denied = crate::background::denied();
        self.background_allowed = crate::background::allowed();
        self.background_power = crate::background::power_ok();
        self.secure_detection = crate::secure_wifi::enabled();
      }
    }

    // Ask the service about itself now and then; the row it fills is advisory,
    // so a slow cadence is plenty.
    if self.link.as_ref().is_some_and(Link::alive) && Instant::now() >= self.next_core_query {
      self.next_core_query = Instant::now() + Duration::from_secs(30);
      self.send(Cmd::CoreInfo);
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
      } else if self.tray.available() {
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
      self.tray.set_color(rgb);
    }
    // The tooltip is Windows-only: `set_tooltip` is a no-op in the Linux
    // backend, so there it would just build a string nobody reads.
    #[cfg(windows)]
    {
      let tip = match self.state {
        State::Online => format!(
          "{} · {label} · {}",
          app_name(),
          dash(private_value(&self.info.ip4, self.cfg.privacy))
        ),
        _ => format!("{} · {label}", app_name()),
      };
      self.tray.set_tooltip(&tip);
    }
    self.tray.set_enabled(
      self.state != State::Online && self.env.allows_login(),
      self.state == State::Online && self.can_disconnect(),
    );

    // When hidden, wake up often enough to pick up a second launch quickly.
    // The turtle walks a short track, so the status page needs a finer tick
    // than the rest of the window.
    let tick = if !self.shown {
      200
    } else if self.page == Page::Status {
      60
    } else {
      500
    };
    ctx.request_repaint_after(Duration::from_millis(tick));
  }

  fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    self.paint(ui);
    self.sync_window_height(ui.ctx());
    paint_toast(ui.ctx(), self.toast.as_ref());
    self.paint_switch_confirm(ui.ctx());
    // Last: every copy this pass produced (the log menu, a label selection,
    // any `clipboard::copy`) leaves through the system clipboard here.
    #[cfg(target_os = "android")]
    crate::clipboard::flush(ui.ctx());
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
    // Android
    "/system/fonts/NotoSansCJK-Regular.ttc",
    "/system/fonts/NotoSansCJKsc-Regular.otf",
    "/system/fonts/NotoSansSC-Regular.otf",
    "/system/fonts/DroidSansFallbackFull.ttf",
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

/// OpenGL first (smallest footprint), then wgpu. Virtual machines and RDP
/// sessions often expose only OpenGL 1.1, where `egui_glow` refuses to start and
/// the process used to die with exit code 1 before any window appeared;
/// `THUNET_RENDERER=wgpu` skips straight to the fallback.
#[cfg(target_os = "android")]
type AndroidApp = winit::platform::android::activity::AndroidApp;
#[cfg(not(target_os = "android"))]
type AndroidApp = ();

#[cfg(target_os = "android")]
pub fn run(cfg: Config, minimized: bool, android_app: AndroidApp) -> Result<(), String> {
  run_all(cfg, minimized, android_app)
}

#[cfg(not(target_os = "android"))]
pub fn run(cfg: Config, minimized: bool) -> Result<(), String> {
  run_all(cfg, minimized, ())
}

fn run_all(cfg: Config, minimized: bool, android_app: AndroidApp) -> Result<(), String> {
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
    match run_with(cfg.clone(), minimized, *renderer, android_app.clone()) {
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

fn run_with(
  cfg: Config,
  minimized: bool,
  renderer: eframe::Renderer,
  android_app: AndroidApp,
) -> Result<(), String> {
  #[allow(unused_mut)]
  let mut viewport = egui::ViewportBuilder::default()
    .with_visible(!minimized)
    .with_icon(std::sync::Arc::new(app_icon()))
    .with_title(app_name());
  // Android sizes the window to the screen; an inner size and a non-resizable
  // hint only fight the system there.
  #[cfg(not(target_os = "android"))]
  {
    viewport = viewport
      .with_inner_size([DEFAULT_WIDTH, WINDOW_HEIGHT])
      .with_resizable(false);
  }
  // Mutated only by the X11 block below and the Android app handle; other
  // platforms build it as is.
  #[allow(unused_mut)]
  let mut options = eframe::NativeOptions {
    viewport,
    // The window size is fixed by the layout, so a stale persisted size (or a
    // previous position off-screen) must not override it on the next launch.
    persist_window: false,
    renderer,
    ..Default::default()
  };
  #[cfg(target_os = "android")]
  {
    options.android_app = Some(android_app);
  }
  #[cfg(not(target_os = "android"))]
  {
    let _ = android_app;
  }

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
      // Also before the first frame, so `auto` starts on the desktop's theme
      // instead of drawing one frame dark and switching afterwards.
      crate::theme::watch(cc.egui_ctx.clone());
      let font = install_fonts(&cc.egui_ctx);
      crate::tray::install_events(&cc.egui_ctx);
      cc.egui_ctx.add_plugin(LogCopyClick::default());
      Ok(Box::new(App::new(cfg, minimized, font)))
    }),
  )
  .map_err(|e| e.to_string())
}
