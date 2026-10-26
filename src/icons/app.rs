//! The app, window and tray artwork: a disc with a white "T".

use eframe::egui;
#[cfg(not(target_os = "android"))]
use tray_icon::Icon;

use crate::session::State;

/// Fill of one state's icons, so the tray, the window title and the status row
/// all read the same colour.
pub fn color_for(state: State) -> [u8; 3] {
  match state {
    State::Online => [46, 160, 67],
    State::Authenticating => [210, 153, 34],
    State::Offline => [140, 140, 140],
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

/// Tray icon in one state's colour.
#[cfg(not(target_os = "android"))]
pub fn make_icon(rgb: [u8; 3]) -> Icon {
  const S: usize = 64;
  Icon::from_rgba(disc_rgba(S, rgb), S as u32, S as u32).expect("valid rgba")
}

/// Blue the portable setup uses where the installed app is green.
///
/// The letter stays the brand "T": the two modes are the same application, and
/// a second glyph only made the small tray icon unreadable.
const PORTABLE_FILL: [u8; 3] = [58, 120, 200];

/// Window / taskbar icon: the packaged app icon (green disc with a white "T"),
/// blue while this process is the portable one.
pub fn app_icon() -> egui::IconData {
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
pub fn app_fill(state: State) -> [u8; 3] {
  if !crate::ipc::portable() {
    return color_for(state);
  }
  match state {
    State::Online => PORTABLE_FILL,
    State::Authenticating => [108, 158, 216],
    State::Offline => [72, 88, 116],
  }
}
