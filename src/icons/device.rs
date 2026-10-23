//! The per-system badges in the online-device list.

use eframe::egui;

/// Which icon a device row gets.
///
/// The portal's `os_name` is the only real hint (and it is often empty), with
/// the class name ("Smartphones/PDAs/Tablets") as a weaker one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceKind {
  Android,
  Apple,
  Windows,
  /// Anything unidentified - and what the badge itself shows.
  Other,
}

pub fn device_kind(os: &str, class: &str) -> DeviceKind {
  let s = format!("{os} {class}").to_ascii_lowercase();
  if s.contains("android") {
    DeviceKind::Android
  } else if ["ios", "iphone", "ipad", "mac os", "macos", "darwin"]
    .iter()
    .any(|hint| s.contains(hint))
  {
    DeviceKind::Apple
  } else if s.contains("windows") {
    DeviceKind::Windows
  } else {
    DeviceKind::Other
  }
}

/// Brand colours, so a 16 px icon still says which system it is at a glance.
const LOGO_RED: egui::Color32 = egui::Color32::from_rgb(0xF2, 0x50, 0x22);
const LOGO_GREEN: egui::Color32 = egui::Color32::from_rgb(0x7F, 0xBA, 0x00);
const LOGO_BLUE: egui::Color32 = egui::Color32::from_rgb(0x00, 0xA4, 0xEF);
const LOGO_YELLOW: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xB9, 0x00);
const ANDROID_GREEN: egui::Color32 = egui::Color32::from_rgb(0x3D, 0xDC, 0x84);

/// A filled polygon.
///
/// `epaint` fans fills from the first point, so a fill has to be **convex**;
/// anything with a notch (the Apple bite) is cut by overdrawing instead.
fn filled_path(points: Vec<egui::Pos2>, fill: egui::Color32) -> egui::Shape {
  egui::Shape::Path(egui::epaint::PathShape {
    points,
    closed: true,
    fill,
    stroke: egui::epaint::PathStroke::NONE,
  })
}

/// Sample an ellipse outline, which is always convex.
fn ellipse_points(center: egui::Pos2, rx: f32, ry: f32, steps: usize) -> Vec<egui::Pos2> {
  (0..steps)
    .map(|i| {
      let a = std::f32::consts::TAU * i as f32 / steps as f32;
      center + egui::vec2(rx * a.cos(), ry * a.sin())
    })
    .collect()
}

/// Sample the upper half of a circle, left end first: the robot's head.
fn dome_points(center: egui::Pos2, r: f32, steps: usize) -> Vec<egui::Pos2> {
  (0..=steps)
    .map(|i| {
      let a = std::f32::consts::PI + std::f32::consts::PI * i as f32 / steps as f32;
      center + egui::vec2(r * a.cos(), r * a.sin())
    })
    .collect()
}

/// Sample a cubic Bézier, both ends included.
fn cubic_points(
  from: egui::Pos2,
  c1: egui::Pos2,
  c2: egui::Pos2,
  to: egui::Pos2,
  steps: usize,
) -> Vec<egui::Pos2> {
  (0..=steps)
    .map(|i| {
      let t = i as f32 / steps as f32;
      // de Casteljau, because `Pos2 * f32` is a `Vec2` and the four terms
      // cannot be summed directly.
      let lerp = |a: egui::Pos2, b: egui::Pos2, t: f32| a + (b - a) * t;
      let p01 = lerp(from, c1, t);
      let p12 = lerp(c1, c2, t);
      let p23 = lerp(c2, to, t);
      lerp(lerp(p01, p12, t), lerp(p12, p23, t), t)
    })
    .collect()
}

/// The shapes of one device icon, laid out on a 16 × 16 grid scaled into
/// `rect`.
///
/// Shapes rather than text glyphs on purpose: the Apple logo is a private-use
/// code point, Windows has no code point at all, and the bundled emoji font is
/// only a subset, so a glyph would come out as a tofu box on some systems.
/// Returned instead of painted so the geometry can be checked without a
/// renderer.
///
/// `ink` colours the single-colour logos (the Apple one is monochrome in its
/// own brand too), and `bg` is what the icon is drawn on - the Apple bite is an
/// overdraw in that colour, because a concave fill is not something `epaint`
/// can do.
fn device_icon_shapes(
  rect: egui::Rect,
  kind: DeviceKind,
  ink: egui::Color32,
  bg: egui::Color32,
) -> Vec<egui::Shape> {
  let s = rect.width() / 16.0;
  let at = |x: f32, y: f32| rect.min + egui::vec2(x * s, y * s);
  let line = |from: egui::Pos2, to: egui::Pos2, w: f32| egui::Shape::LineSegment {
    points: [from, to],
    stroke: egui::Stroke::new((w * s).max(1.0), ink).into(),
  };
  match kind {
    // The Microsoft window: four panes, brand colours, hairline gap.
    DeviceKind::Windows => {
      let pane = 6.4;
      let gap = 1.2;
      let (x0, y0) = (1.0, 1.0);
      let x1 = x0 + pane + gap;
      let y1 = y0 + pane + gap;
      let pane_rect = |x: f32, y: f32, c: egui::Color32| {
        egui::Shape::Rect(egui::epaint::RectShape::filled(
          egui::Rect::from_min_max(at(x, y), at(x + pane, y + pane)),
          egui::CornerRadius::same(0),
          c,
        ))
      };
      vec![
        pane_rect(x0, y0, LOGO_RED),
        pane_rect(x1, y0, LOGO_GREEN),
        pane_rect(x0, y1, LOGO_BLUE),
        pane_rect(x1, y1, LOGO_YELLOW),
      ]
    }
    // The bugdroid's head: dome, antennae, and eyes that are holes in the
    // green rather than painted on it.
    DeviceKind::Android => {
      let mut shapes = vec![
        line(at(5.2, 7.3), at(3.6, 4.6), 1.1),
        line(at(10.8, 7.3), at(12.4, 4.6), 1.1),
        filled_path(dome_points(at(8.0, 11.4), 5.0 * s, 24), ANDROID_GREEN),
      ];
      for x in [6.1, 9.9] {
        shapes.push(egui::Shape::Circle(egui::epaint::CircleShape {
          center: at(x, 8.3),
          radius: 0.9 * s,
          fill: egui::Color32::from_rgb(0xFA, 0xFA, 0xFA),
          stroke: egui::Stroke::NONE.into(),
        }));
      }
      shapes
    }
    // The apple: an elliptical body, a circle in the background colour for the
    // bite, a short stem and an almond leaf. The body is a single convex fill
    // because `epaint` fans fills and cannot draw a concave notch by itself.
    DeviceKind::Apple => {
      let leaf = [
        cubic_points(
          at(8.7, 3.9),
          at(10.9, 3.8),
          at(11.7, 3.3),
          at(12.5, 1.2),
          10,
        ),
        cubic_points(at(12.5, 1.2), at(10.3, 1.3), at(9.5, 1.8), at(8.7, 3.9), 10),
      ]
      .concat();
      vec![
        line(at(7.9, 4.6), at(8.3, 3.2), 0.9),
        filled_path(leaf, ink),
        filled_path(ellipse_points(at(7.6, 9.8), 5.8 * s, 5.6 * s, 40), ink),
        egui::Shape::Circle(egui::epaint::CircleShape {
          center: at(13.9, 8.6),
          radius: 2.8 * s,
          fill: bg,
          stroke: egui::Stroke::NONE.into(),
        }),
      ]
    }
    // A monitor: screen, stand, base.
    DeviceKind::Other => {
      let screen = egui::Rect::from_min_max(at(1.0, 2.3), at(15.0, 10.7));
      let stroke: egui::Stroke = egui::Stroke::new((1.4 * s).max(1.0), ink);
      vec![
        egui::Shape::Rect(egui::epaint::RectShape::new(
          screen,
          egui::CornerRadius::same(2),
          egui::Color32::TRANSPARENT,
          stroke,
          egui::StrokeKind::Inside,
        )),
        egui::Shape::Rect(egui::epaint::RectShape::filled(
          egui::Rect::from_min_max(at(6.9, 10.7), at(9.1, 13.4)),
          egui::CornerRadius::same(0),
          ink,
        )),
        filled_path(
          vec![at(4.2, 14.5), at(11.8, 14.5), at(11.0, 13.2), at(5.0, 13.2)],
          ink,
        ),
      ]
    }
  }
}

/// Draw one device icon into `rect`.
///
/// Clipped to the icon's own box: the Apple bite is drawn as a circle in the
/// background colour and reaches past the body, so without a clip it would
/// rub out a few pixels of whatever sits to its right.
pub fn paint_device_icon(
  painter: &egui::Painter,
  rect: egui::Rect,
  kind: DeviceKind,
  ink: egui::Color32,
  bg: egui::Color32,
) {
  let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
  painter.extend(device_icon_shapes(rect, kind, ink, bg));
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The portal names an OS only sometimes (`os_name`), so the class name is
  /// the fallback; anything unrecognised gets the generic icon.
  #[test]
  fn devices_are_told_apart_by_their_os_name() {
    assert_eq!(device_kind("AndroidOS", ""), DeviceKind::Android);
    assert_eq!(device_kind("iOS", ""), DeviceKind::Apple);
    assert_eq!(device_kind("Mac OS X", ""), DeviceKind::Apple);
    assert_eq!(device_kind("Windows", ""), DeviceKind::Windows);
    assert_eq!(device_kind("Linux", ""), DeviceKind::Other);
    assert_eq!(
      device_kind("", "Smartphones/PDAs/Tablets"),
      DeviceKind::Other
    );
    assert_eq!(device_kind("", ""), DeviceKind::Other);
  }

  /// Does one shape paint over `p`?
  fn icon_covers(shape: &egui::Shape, p: egui::Pos2) -> bool {
    use egui::Color32;
    let on_edge = |points: &[egui::Pos2], closed: bool, width: f32| {
      let n = points.len();
      let last = if closed { n } else { n - 1 };
      (0..last).any(|i| {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let ab = b - a;
        let len2 = ab.length_sq();
        let t = if len2 > 0.0 {
          ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
        } else {
          0.0
        };
        (a + ab * t).distance(p) <= width / 2.0
      })
    };
    match shape {
      egui::Shape::Path(path) => {
        (path.fill != Color32::TRANSPARENT && point_in_polygon(&path.points, p))
          || on_edge(&path.points, true, path.stroke.width)
      }
      egui::Shape::Circle(circle) => {
        let r = circle.center.distance(p);
        (circle.fill != Color32::TRANSPARENT && r <= circle.radius)
          || (circle.stroke.width > 0.0 && (r - circle.radius).abs() <= circle.stroke.width / 2.0)
      }
      egui::Shape::Rect(rect) => {
        if !rect.rect.contains(p) {
          return false;
        }
        let w = rect.stroke.width;
        rect.fill != Color32::TRANSPARENT || w <= 0.0 || !rect.rect.shrink(w / 2.0).contains(p)
      }
      egui::Shape::LineSegment { points, stroke } => on_edge(points, false, stroke.width),
      _ => false,
    }
  }

  /// Ray casting, for the convex fills the icons are made of.
  fn point_in_polygon(points: &[egui::Pos2], p: egui::Pos2) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
      let (a, b) = (points[i], points[j]);
      if (a.y > p.y) != (b.y > p.y) {
        let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
        if p.x < x {
          inside = !inside;
        }
      }
      j = i;
    }
    inside
  }

  /// Every icon stays inside its box and covers a fair share of it: a shape
  /// pushed off the rect, or an icon that lost its fill, would otherwise only
  /// show up in a screenshot.
  #[test]
  fn every_device_icon_fills_its_box() {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0));
    let bg = egui::Color32::from_rgb(1, 2, 3);
    // Each icon's share of the box, with room for the shapes being tuned: a
    // filled window is most of the box, a robot head only its lower half.
    for (kind, least) in [
      (DeviceKind::Windows, 0.50),
      (DeviceKind::Android, 0.12),
      (DeviceKind::Apple, 0.25),
      (DeviceKind::Other, 0.08),
    ] {
      let shapes = device_icon_shapes(rect, kind, egui::Color32::BLACK, bg);
      assert!(!shapes.is_empty(), "{kind:?} draws nothing");
      for shape in &shapes {
        // The bite is a background-coloured circle that deliberately reaches
        // past the body; `paint_device_icon` clips it to the box.
        if matches!(shape, egui::Shape::Circle(circle) if circle.fill == bg) {
          continue;
        }
        let bounds = shape.visual_bounding_rect();
        assert!(
          rect.expand(0.6).contains_rect(bounds),
          "{kind:?} paints outside its box: {bounds:?}"
        );
      }
      let cells = 16;
      let covered = (0..cells)
        .flat_map(|y| (0..cells).map(move |x| (x, y)))
        .filter(|(x, y)| {
          let p = egui::pos2(*x as f32 + 0.5, *y as f32 + 0.5);
          shapes.iter().any(|s| icon_covers(s, p))
        })
        .count();
      let share = covered as f32 / (cells * cells) as f32;
      assert!(share > least, "{kind:?} only covers {share:.2} of its box");
    }
  }
}
