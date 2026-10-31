//! Where diagnostics go.
//!
//! A windowed binary has no console and the machine core has no stderr anybody
//! can see: under systemd it is the journal, under the SCM it is nothing at all.
//! The machine core therefore points this at a file next to its config, and
//! everything written through [`line`] lands there as well as on stderr, so
//! Linux keeps both copies (journal + file) and Windows gets the only one it
//! can have. A portable run never sets the file: its stderr is a terminal or
//! the journal of whoever started it.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

static FILE: OnceLock<PathBuf> = OnceLock::new();

/// Size at which the log file is dropped and started again.
///
/// `Restart=always` (systemd) and the SCM's failure actions keep a crashing
/// core coming back, and a crash loop writes a stack trace every few seconds;
/// an unbounded file would fill `/var` or the system disk. Truncation is
/// deliberately crude: this file is a diagnostic aid, not an audit trail.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Also append diagnostics to `path`. Called once, by the machine core.
pub fn set_file(path: PathBuf) {
  let _ = FILE.set(path);
}

#[cfg(target_os = "android")]
mod logcat_impl {
  use std::ffi::{CString, c_char};

  #[link(name = "log")]
  unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
  }

  /// `ANDROID_LOG_INFO`.
  const INFO: i32 = 4;

  pub fn write(text: &str) {
    let Ok(text) = CString::new(text) else {
      return;
    };
    // SAFETY: both pointers are valid NUL-terminated strings for the duration of
    // the call, and liblog copies what it needs.
    unsafe { __android_log_write(INFO, c"THUnet".as_ptr(), text.as_ptr()) };
  }
}

/// Mirror one line to logcat.
///
/// Android gives a process no stderr until a window exists - and a process the
/// keep-alive service starts never has one - so a core that fails to come up
/// would fail invisibly: the service is restarted, the notification reappears,
/// and only `core.log` (unreadable without root) would say why. The service
/// path is exactly the one that cannot be watched from the window, so it has to
/// be visible from logcat.
#[cfg(target_os = "android")]
pub fn logcat(text: &str) {
  logcat_impl::write(text);
}

#[cfg(not(target_os = "android"))]
pub fn logcat(_text: &str) {}

/// Write one line to stderr and, when configured, to the log file.
pub fn line(text: &str) {
  eprintln!("{text}");
  logcat(text);
  let Some(path) = FILE.get() else {
    return;
  };
  if let Some(dir) = path.parent() {
    let _ = std::fs::create_dir_all(dir);
  }
  // Checked before opening, so the file only ever overshoots the cap by the one
  // line being written.
  if std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
    let _ = std::fs::remove_file(path);
  }
  if let Ok(mut file) = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
  {
    let _ = writeln!(file, "{text}");
  }
}
