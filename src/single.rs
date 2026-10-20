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

/// Consume a pending "show the window" request from a second launch.
///
/// The running instance performs the actual `ShowWindow` through its own event
/// loop. Doing it from the second process would bypass winit's cached
/// visibility flag, after which the app could no longer hide its own window
/// (clicking the close button would appear to do nothing).
pub fn show_requested() -> bool {
  imp::show_requested()
}

#[cfg(windows)]
mod imp {
  use std::sync::atomic::{AtomicIsize, Ordering};

  use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, WAIT_OBJECT_0,
  };
  use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
  };
  use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
  };

  /// Auto-reset event a second launch signals; owned by the primary instance.
  static SHOW: AtomicIsize = AtomicIsize::new(0);

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
      // Created before the guard is returned, so a second launch can only ever
      // find it once this process is the running instance.
      let ev = wide(r"Local\thunet-show-window");
      let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, ev.as_ptr()) };
      SHOW.store(ev as isize, Ordering::Relaxed); // 0 = unavailable
      return Some(super::Guard {
        handle: handle as isize,
      });
    }

    // Ring the doorbell instead of touching the other process's window: its
    // own `logic()` turns this into a normal viewport command.
    let ev = wide(r"Local\thunet-show-window");
    let ev = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, ev.as_ptr()) };
    if !ev.is_null() {
      // SAFETY: the handle came from OpenEventW with EVENT_MODIFY_STATE.
      unsafe {
        SetEvent(ev);
        CloseHandle(ev);
      }
      return None;
    }

    // Fallback for a running build that predates the doorbell.
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

  pub fn show_requested() -> bool {
    let handle = SHOW.load(Ordering::Relaxed);
    // SAFETY: the handle is a live auto-reset event (0 is checked above).
    handle != 0 && unsafe { WaitForSingleObject(handle as _, 0) == WAIT_OBJECT_0 }
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

  fn show_path() -> PathBuf {
    lock_path().with_extension("show")
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
          let _ = std::fs::remove_file(show_path());
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
          // The running instance polls this marker and shows its window.
          let _ = std::fs::write(show_path(), b"");
          return None;
        }
      }
    }
    None
  }

  pub fn show_requested() -> bool {
    let flag = show_path();
    if flag.exists() {
      let _ = std::fs::remove_file(&flag);
      return true;
    }
    false
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
