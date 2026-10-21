//! Where diagnostics go.
//!
//! A windowed binary has no console and a Windows service has no stderr at
//! all: `eprintln!` would vanish exactly where the output matters most. The
//! service therefore points this at a file next to its config, and everything
//! written through [`line`] lands there. Elsewhere the file is never set and
//! the line only goes to stderr (the journal, under systemd).

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

static FILE: OnceLock<PathBuf> = OnceLock::new();

/// Also append diagnostics to `path`. Called once, by the service wrapper.
#[cfg(windows)]
pub fn set_file(path: PathBuf) {
  let _ = FILE.set(path);
}

/// Write one line to stderr and, when configured, to the log file.
pub fn line(text: &str) {
  eprintln!("{text}");
  let Some(path) = FILE.get() else {
    return;
  };
  if let Some(dir) = path.parent() {
    let _ = std::fs::create_dir_all(dir);
  }
  if let Ok(mut file) = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
  {
    let _ = writeln!(file, "{text}");
  }
}
