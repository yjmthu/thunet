//! Single-instance guard.
//!
//! A second launch of the GUI must not start another tray icon and a second
//! network worker: it should bring the existing window back instead.

/// Held for the lifetime of the process; dropping it releases the guard.
pub struct Guard {
  #[cfg(windows)]
  handle: isize,
  #[cfg(unix)]
  path: std::path::PathBuf,
}

/// Try to become the one running instance.
///
/// `Some(guard)` means this process owns the instance; `None` means another
/// process already runs, and it has been asked to show its window.
pub fn acquire() -> Option<Guard> {
  imp::acquire()
}

#[cfg(windows)]
mod imp {
  use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
  use windows_sys::Win32::System::Threading::CreateMutexW;
  use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
  };

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  pub fn acquire() -> Option<super::Guard> {
    // A session-local mutex: per-user, released automatically on exit.
    let name = wide(r"Local\thunet-single-instance");
    // SAFETY: plain Win32 call; the handle is leaked on purpose so the mutex
    // lives for the rest of the process.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
      return Some(super::Guard { handle: 0 });
    }
    let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !taken {
      return Some(super::Guard {
        handle: handle as isize,
      });
    }

    // Ask the running copy to come back from the tray.
    let title = wide("THUnet");
    // SAFETY: both calls take NUL-terminated strings/handles owned above.
    unsafe {
      let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
      if !hwnd.is_null() {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
      }
    }
    None
  }
}

#[cfg(unix)]
mod imp {
  use std::io::Write as _;
  use std::path::PathBuf;

  fn lock_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
      .map(PathBuf::from)
      .unwrap_or_else(std::env::temp_dir);
    base.join("thunet.lock")
  }

  /// A lock file whose contents are the owner's pid. A leftover file from a
  /// crashed run is reaped when that pid no longer exists.
  pub fn acquire() -> Option<super::Guard> {
    let path = lock_path();
    for _ in 0..2 {
      match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
      {
        Ok(mut file) => {
          let _ = writeln!(file, "{}", std::process::id());
          return Some(super::Guard { path });
        }
        Err(_) => {
          let stale = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists())
            .unwrap_or(true);
          if stale {
            let _ = std::fs::remove_file(&path);
            continue;
          }
          return None;
        }
      }
    }
    None
  }
}

impl Drop for Guard {
  fn drop(&mut self) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(&self.path);
    #[cfg(windows)]
    {
      use windows_sys::Win32::Foundation::CloseHandle;
      if self.handle != 0 {
        // SAFETY: the handle came from CreateMutexW and is closed once.
        unsafe { CloseHandle(self.handle as _) };
      }
    }
  }
}
