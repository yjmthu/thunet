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
