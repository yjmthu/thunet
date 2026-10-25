//! Every icon the window, the tray and the device list draw.
//!
//! They are painted in code rather than shipped as assets, so the binary stays
//! asset-free and the tray can be recoloured per state. Split by where an icon
//! is used: `app` is the tray/window artwork, `glyph` the small tinted shapes
//! in the header, and `device` the per-system badges in the device list.

mod app;
mod device;
mod glyph;

pub use app::{app_fill, app_icon, color_for, make_icon};
pub use device::{DeviceKind, device_kind, paint_device_icon};
pub use glyph::{
  back_arrow_image, close_button, eye_closed_image, eye_open_image, gear_image, heading_icon,
  icon_button, icon_texture, paint_copy_icon,
};
