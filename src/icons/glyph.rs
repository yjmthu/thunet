//! Small white glyphs the window tints to its own text colour.

use eframe::egui;

/// Size of the header icons (*Settings*, *Back*) in points.
///
/// Set by hand rather than from the button's text height: the glyphs are
/// textures with their own proportions, and this size lines them up with the
/// labels next to them.
const ICON_SIDE: f32 = 15.0;

/// The same, beside a page heading, which is drawn larger.
const HEADING_ICON_SIDE: f32 = 18.0;

/// The gear in front of *Settings*, drawn in code: asset-free like the app
/// icon, and independent of the fonts, since egui bundles only the glyphs in
/// `special_emojis` and the system CJK fallback has no gear either.
pub fn gear_image() -> egui::ColorImage {
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
pub fn back_arrow_image() -> egui::ColorImage {
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

/// Open eye: shows that private values are on screen.
pub fn eye_open_image() -> egui::ColorImage {
  eye_image(true)
}

/// Closed eye: shows that private values are masked.
pub fn eye_closed_image() -> egui::ColorImage {
  eye_image(false)
}

/// The privacy eye, drawn the way the other header glyphs are.
///
/// The open eye is a lens, i.e. the intersection of two discs, so the two lids
/// meet without a corner; the pupil is a filled disc inside it. The closed eye
/// is a lid curving downwards with three lashes, which stays readable at 15 px
/// where an open eye with a slash does not.
fn eye_image(open: bool) -> egui::ColorImage {
  const S: usize = 64;
  const HALF: f32 = 0.045;
  // Lens geometry, in units of the icon's own size. A round eye: the first
  // shape was 2.5:1 and read as a line with a bump at 15 px, this one is about
  // 1.3:1.
  const R: f32 = 0.42;
  const D: f32 = 0.10;

  let mut strokes: Vec<((f32, f32), (f32, f32))> = Vec::new();
  let mut discs: Vec<((f32, f32), f32)> = Vec::new();

  if open {
    let edge = (R * R - D * D).sqrt();
    let steps = 24;
    let mut upper = Vec::with_capacity(steps + 1);
    let mut lower = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
      let x = -edge + 2.0 * edge * i as f32 / steps as f32;
      let bulge = (R * R - x * x).max(0.0).sqrt();
      upper.push((0.5 + x, 0.5 + D - bulge));
      lower.push((0.5 + x, 0.5 - D + bulge));
    }
    let mut outline: Vec<(f32, f32)> = upper;
    outline.extend(lower.into_iter().rev());
    for pair in outline.windows(2) {
      strokes.push((pair[0], pair[1]));
    }
    if let (Some(first), Some(last)) = (outline.first(), outline.last()) {
      strokes.push((*last, *first));
    }
    discs.push(((0.5, 0.5), 0.15));
  } else {
    // A lid whose middle is its lowest point, with lashes fanning out below it.
    let steps = 24;
    let mut arc = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
      let x = -0.38 + 0.76 * i as f32 / steps as f32;
      let y = 0.60 - 0.20 * (x / 0.38).powi(2);
      arc.push((0.5 + x, y));
    }
    for pair in arc.windows(2) {
      strokes.push((pair[0], pair[1]));
    }
    for (x, y, dx, dy) in [
      (-0.22, 0.533, -0.08, 0.09),
      (0.0, 0.60, 0.0, 0.10),
      (0.22, 0.533, 0.08, 0.09),
    ] {
      strokes.push(((0.5 + x, y), (0.5 + x + dx, y + dy)));
    }
  }

  let s = S as f32;
  let mut rgba = vec![0u8; S * S * 4];
  for y in 0..S {
    for x in 0..S {
      let p = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
      let mut alpha = strokes
        .iter()
        .map(|&(from, to)| segment_alpha(p, from, to, HALF))
        .fold(0.0, f32::max);
      for &(centre, radius) in &discs {
        let d = ((p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2)).sqrt();
        alpha = alpha.max(1.0 - ramp(d, radius - 0.012, radius + 0.012));
      }
      let i = (y * S + x) * 4;
      rgba[i] = 255;
      rgba[i + 1] = 255;
      rgba[i + 2] = 255;
      rgba[i + 3] = (alpha * 255.0).round() as u8;
    }
  }
  egui::ColorImage::from_rgba_unmultiplied([S, S], &rgba)
}

/// Upload an icon once and hand the same handle out afterwards.
///
/// `load_texture` allocates a new GPU texture on every call, and the window is
/// repainted twice a second even when nothing happens.
pub fn icon_texture(
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
pub fn icon_button(texture: egui::TextureId) -> egui::Button<'static> {
  egui::Button::image(egui::Image::new(egui::load::SizedTexture::new(
    texture,
    [ICON_SIDE, ICON_SIDE],
  )))
  .image_tint_follows_text_color(true)
}

/// An icon beside a page heading, painted in the text colour and drawn larger
/// than the header buttons.
pub fn heading_icon(
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

/// A close button in the macOS window-controls style.
///
/// A flat 13 px red circle with no outline; the cross only appears while the
/// pointer is over it, in a dark tint of the same red rather than white, which
/// is what keeps it from shouting. Drawn rather than made of a text glyph: the
/// bundled fonts may not carry a cross that looks like one.
pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
  const HIT: f32 = 16.0;
  const DIA: f32 = 13.0;
  let (rect, resp) = ui.allocate_exact_size(egui::vec2(HIT, HIT), egui::Sense::click());
  let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
  let hovered = resp.hovered();
  let fill = if hovered {
    egui::Color32::from_rgb(255, 120, 112)
  } else {
    egui::Color32::from_rgb(255, 95, 87)
  };
  let painter = ui.painter();
  painter.circle_filled(rect.center(), DIA / 2.0, fill);
  if hovered {
    let r = DIA * 0.22;
    let stroke = egui::Stroke::new(1.3, egui::Color32::from_rgba_unmultiplied(90, 8, 6, 200));
    painter.line_segment(
      [
        rect.center() + egui::vec2(-r, -r),
        rect.center() + egui::vec2(r, r),
      ],
      stroke,
    );
    painter.line_segment(
      [
        rect.center() + egui::vec2(-r, r),
        rect.center() + egui::vec2(r, -r),
      ],
      stroke,
    );
  }
  resp
}

#[cfg(test)]
mod tests {
  use super::*;

  fn coverage(image: &egui::ColorImage) -> usize {
    image.pixels.iter().filter(|p| p.a() > 0).count()
  }

  /// Both eye states have to draw something, and they have to differ: the
  /// button is the only cue for what privacy mode is doing.
  #[test]
  fn the_privacy_eye_has_two_states() {
    let open = eye_open_image();
    let closed = eye_closed_image();
    assert!(coverage(&open) > 100, "the open eye is drawn");
    assert!(coverage(&closed) > 100, "the closed eye is drawn");
    let centre = |img: &egui::ColorImage| img.pixels[32 * 64 + 32].a();
    assert_eq!(centre(&open), 255, "the open eye has a pupil");
    assert_eq!(centre(&closed), 0, "the closed eye is only a lid");
  }
}
