//! Levelled, timestamped log lines shown in the window.
//!
//! Rendering is deliberately plain text so the same line can be copied out
//! verbatim:
//!
//! ```text
//! [2018-09-09 12:56:26] [INFO] login ok
//! ```

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// Severity, ordered from least to most important.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
  Debug,
  Info,
  Warn,
  Error,
  /// Unrecoverable: the app cannot continue meaningfully.
  #[allow(dead_code)] // reserved for fatal startup failures
  Fatal,
}

impl Level {
  /// Levels offered in the settings page, quietest last.
  pub const CHOICES: [Level; 4] = [Level::Debug, Level::Info, Level::Warn, Level::Error];

  pub fn tag(self) -> &'static str {
    match self {
      Level::Debug => "DEBUG",
      Level::Info => "INFO",
      Level::Warn => "WARN",
      Level::Error => "ERROR",
      Level::Fatal => "FATAL",
    }
  }

  /// Parse the stored `log_level` setting. Anything unrecognised means "show
  /// everything" rather than silently hiding diagnostics.
  pub fn from_setting(setting: &str) -> Self {
    match setting.trim().to_ascii_lowercase().as_str() {
      "info" => Level::Info,
      "warn" | "warning" => Level::Warn,
      "error" => Level::Error,
      _ => Level::Debug,
    }
  }

  /// Inverse of [`Level::from_setting`], for the config file.
  pub fn setting(self) -> &'static str {
    match self {
      Level::Debug => "debug",
      Level::Info => "info",
      Level::Warn => "warn",
      Level::Error | Level::Fatal => "error",
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
  pub at: DateTime<Local>,
  pub level: Level,
  pub msg: String,
}

impl Line {
  pub fn new(level: Level, msg: impl Into<String>) -> Self {
    Self {
      at: Local::now(),
      level,
      msg: msg.into(),
    }
  }

  pub fn debug(msg: impl Into<String>) -> Self {
    Self::new(Level::Debug, msg)
  }

  pub fn info(msg: impl Into<String>) -> Self {
    Self::new(Level::Info, msg)
  }

  pub fn warn(msg: impl Into<String>) -> Self {
    Self::new(Level::Warn, msg)
  }

  pub fn error(msg: impl Into<String>) -> Self {
    Self::new(Level::Error, msg)
  }

  /// `[2018-09-09 12:56:26] [INFO] message` in the local time zone.
  pub fn render(&self) -> String {
    format!(
      "[{}] [{}] {}",
      self.at.format("%Y-%m-%d %H:%M:%S"),
      self.level.tag(),
      self.msg
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn render_has_expected_shape() {
    let line = Line::info("login ok");
    let s = line.render();
    let b = s.as_bytes();
    // `[YYYY-MM-DD HH:MM:SS]`
    assert_eq!(b[0], b'[');
    assert_eq!(b[20], b']');
    assert_eq!(b[5], b'-');
    assert_eq!(b[8], b'-');
    assert_eq!(b[11], b' ');
    assert_eq!(b[14], b':');
    assert_eq!(b[17], b':');
    for i in [1, 2, 3, 4, 6, 7, 9, 10, 12, 13, 15, 16, 18, 19] {
      assert!(
        b[i].is_ascii_digit(),
        "position {i} of {s:?} is not a digit"
      );
    }
    assert!(s.ends_with("] [INFO] login ok"), "{s}");
    assert_eq!(&s[1..20], line.at.format("%Y-%m-%d %H:%M:%S").to_string());
  }

  #[test]
  fn levels_are_ordered_and_tagged() {
    assert!(Level::Debug < Level::Info);
    assert!(Level::Warn < Level::Error);
    assert!(Level::Error < Level::Fatal);
    assert_eq!(Level::Warn.tag(), "WARN");
    assert!(Line::error("boom").render().contains("[ERROR] boom"));
  }

  #[test]
  fn log_level_setting_roundtrips() {
    for level in Level::CHOICES {
      assert_eq!(Level::from_setting(level.setting()), level);
    }
    // Unknown values keep every line visible.
    assert_eq!(Level::from_setting(""), Level::Debug);
    assert_eq!(Level::from_setting("noise"), Level::Debug);
    assert_eq!(Level::from_setting("WARN"), Level::Warn);
  }
}
