//! Single-instance guard.
//!
//! A second launch of the GUI must not start another tray icon and a second
//! network worker: it should bring the existing window back instead.

/// Held for the lifetime of the process; dropping it releases the guard.
pub struct Guard {
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

/// Consume a pending "quit" request (an installer or `thunet quit` asked).
///
/// Closing the window only hides it, because that is what a tray app has to do;
/// an uninstall or an upgrade needs the window to *end*, and killing the
/// process skips the orderly shutdown. This is the doorbell for that.
pub fn quit_requested() -> bool {
  imp::quit_requested()
}

/// Ask the running window to quit and wait until it is really gone.
///
/// `Ok(())` also when nothing was running. The wait polls the single-instance
/// name instead of spawning `taskkill` or any other console program, and
/// reports `Err` when the window outlives `timeout` so the caller can decide
/// what to do about it.
pub fn quit_and_wait(timeout: std::time::Duration) -> Result<(), String> {
  imp::quit_and_wait(timeout)
}

/// Best-effort cleanup before `std::process::exit`, which skips [`Guard::drop`].
///
/// Windows releases the mutex when the process ends; only the Unix lock file
/// has to be removed by hand.
pub fn release() {
  imp::release()
}

#[cfg(windows)]
mod imp {
  use std::sync::atomic::{AtomicIsize, Ordering};

  use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, WAIT_OBJECT_0,
  };
  use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, OpenMutexW, SetEvent,
    WaitForSingleObject,
  };

  /// Standard access right `SYNCHRONIZE` (not re-exported by `windows-sys`).
  const SYNCHRONIZE: u32 = 0x0010_0000;

  /// Auto-reset event a second launch signals; owned by the primary instance.
  static SHOW: AtomicIsize = AtomicIsize::new(0);

  /// Auto-reset event that asks the primary instance to quit for good.
  static QUIT: AtomicIsize = AtomicIsize::new(0);

  /// Session-local names: one window per session.
  const SHOW_EVENT: &str = r"Local\thunet-show-window";
  const QUIT_EVENT: &str = r"Local\thunet-quit";
  const INSTANCE_MUTEX: &str = r"Local\thunet-single-instance";

  /// Primary-instance mutex. Kept here (not in the `Guard`) so a process that
  /// is about to hand over to a freshly spawned successor can close it first.
  static MUTEX: AtomicIsize = AtomicIsize::new(0);

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  pub fn acquire() -> Option<super::Guard> {
    // A session-local mutex: per-user, released automatically on exit.
    let name = wide(INSTANCE_MUTEX);
    // SAFETY: plain Win32 call; the handle is leaked on purpose so the mutex
    // lives for the rest of the process.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
      return Some(super::Guard {});
    }
    let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !taken {
      MUTEX.store(handle as isize, Ordering::Relaxed);
      // Created before the guard is returned, so a second launch can only ever
      // find it once this process is the running instance.
      let ev = wide(SHOW_EVENT);
      let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, ev.as_ptr()) };
      SHOW.store(ev as isize, Ordering::Relaxed); // 0 = unavailable
      let quit = wide(QUIT_EVENT);
      let quit = unsafe { CreateEventW(std::ptr::null(), 0, 0, quit.as_ptr()) };
      QUIT.store(quit as isize, Ordering::Relaxed);
      return Some(super::Guard {});
    }

    // Ring the doorbell instead of touching the other process's window: its
    // own `logic()` turns this into a normal viewport command.
    let ev = wide(SHOW_EVENT);
    let ev = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, ev.as_ptr()) };
    if !ev.is_null() {
      // SAFETY: the handle came from OpenEventW with EVENT_MODIFY_STATE.
      unsafe {
        SetEvent(ev);
        CloseHandle(ev);
      }
      return None;
    }

    // No event means the running instance cannot be asked to show itself; this
    // launch just exits.
    None
  }

  pub fn show_requested() -> bool {
    let handle = SHOW.load(Ordering::Relaxed);
    // SAFETY: the handle is a live auto-reset event (0 is checked above).
    handle != 0 && unsafe { WaitForSingleObject(handle as _, 0) == WAIT_OBJECT_0 }
  }

  pub fn quit_requested() -> bool {
    let handle = QUIT.load(Ordering::Relaxed);
    // SAFETY: the handle is a live auto-reset event (0 is checked above).
    handle != 0 && unsafe { WaitForSingleObject(handle as _, 0) == WAIT_OBJECT_0 }
  }

  /// Ring the quit doorbell and poll until the instance name is gone.
  ///
  /// The mutex is not held while the app runs (it only has to exist), so its
  /// *name* is the liveness signal: once the owning process exits, the object
  /// is destroyed and `OpenMutexW` starts failing with `FILE_NOT_FOUND`. That
  /// is the loop that replaces "spawn taskkill and hope".
  pub fn quit_and_wait(timeout: std::time::Duration) -> Result<(), String> {
    let event = wide(QUIT_EVENT);
    // SAFETY: plain Win32 calls on NUL-terminated names that outlive them.
    let doorbell = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, event.as_ptr()) };
    if doorbell.is_null() {
      // Nothing is running, so there is nobody to wait for.
      return Ok(());
    }
    unsafe {
      SetEvent(doorbell);
      CloseHandle(doorbell);
    }

    let name = wide(INSTANCE_MUTEX);
    let deadline = std::time::Instant::now() + timeout;
    loop {
      // SAFETY: as above; a successful open is closed again immediately.
      let instance = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
      if instance.is_null() {
        return Ok(());
      }
      unsafe { CloseHandle(instance) };
      if std::time::Instant::now() >= deadline {
        return Err(format!(
          "the window is still running after {} s",
          timeout.as_secs()
        ));
      }
      std::thread::sleep(std::time::Duration::from_millis(50));
    }
  }

  /// Close the mutex so a successor spawned a moment ago can take over.
  ///
  /// Idempotent: `Drop` calls it too, and the swap makes the second call a
  /// no-op. The event needs no cleanup — it dies with the process.
  pub fn release() {
    let handle = MUTEX.swap(0, Ordering::Relaxed);
    if handle != 0 {
      // SAFETY: the handle came from CreateMutexW and is closed exactly once
      // thanks to the swap above.
      unsafe { CloseHandle(handle as _) };
    }
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

  fn quit_path() -> PathBuf {
    lock_path().with_extension("quit")
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
          // A quit marker left by an installer for the *previous* instance must
          // not end this one.
          let _ = std::fs::remove_file(quit_path());
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

  /// Same marker file, a different meaning: "end, do not hide".
  pub fn quit_requested() -> bool {
    let flag = quit_path();
    if flag.exists() {
      let _ = std::fs::remove_file(&flag);
      return true;
    }
    false
  }

  /// Write the quit marker and wait for the lock file to disappear.
  pub fn quit_and_wait(timeout: std::time::Duration) -> Result<(), String> {
    let lock = lock_path();
    if !lock.exists() {
      return Ok(());
    }
    std::fs::write(quit_path(), b"").map_err(|e| format!("{}: {e}", quit_path().display()))?;
    let deadline = std::time::Instant::now() + timeout;
    while lock.exists() {
      if std::time::Instant::now() >= deadline {
        return Err(format!(
          "the window is still running after {} s",
          timeout.as_secs()
        ));
      }
      std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
  }

  /// Remove the lock file the guard would have removed on drop.
  pub fn release() {
    let _ = std::fs::remove_file(lock_path());
  }
}

impl Drop for Guard {
  fn drop(&mut self) {
    #[cfg(unix)]
    let _ = std::fs::remove_file(&self.path);
    #[cfg(windows)]
    imp::release();
  }
}
