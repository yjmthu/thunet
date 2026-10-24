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

/// Raise the machine-wide "every window ends" signal.
///
/// The per-user doorbells above cannot cross a session, and the core that used
/// to relay a broadcast may not be running at all - exactly the state an
/// uninstall often finds the machine in. This signal is machine-scoped and
/// needs no core; windows in machine mode poll it and end themselves, while a
/// portable window ignores it. Dropping the guard withdraws it.
pub fn machine_quit() -> Result<MachineQuit, String> {
  imp::machine_quit()
}

/// Has a machine-wide quit been asked for right now?
///
/// Machine-mode windows poll this every frame; portable ones never do, because
/// an uninstall leaves them alone.
pub fn machine_quit_requested() -> bool {
  imp::machine_quit_requested()
}

pub use imp::MachineQuit;

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

  /// Session-local names: one window per session **and state directory**.
  ///
  /// The tag is what keeps two portable setups of one user apart: without it a
  /// second window would attach to the first one's core and drive that
  /// session.
  fn show_name() -> String {
    format!(r"Local\thunet-{}-show-window", crate::config::state_tag())
  }
  fn quit_name() -> String {
    format!(r"Local\thunet-{}-quit", crate::config::state_tag())
  }
  fn instance_name() -> String {
    format!(
      r"Local\thunet-{}-single-instance",
      crate::config::state_tag()
    )
  }

  /// Primary-instance mutex. Kept here (not in the `Guard`) so a process that
  /// is about to hand over to a freshly spawned successor can close it first.
  static MUTEX: AtomicIsize = AtomicIsize::new(0);

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  pub fn acquire() -> Option<super::Guard> {
    // A session-local mutex: per-user, released automatically on exit.
    let name = wide(&instance_name());
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
      let ev = wide(&show_name());
      let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, ev.as_ptr()) };
      SHOW.store(ev as isize, Ordering::Relaxed); // 0 = unavailable
      let quit = wide(&quit_name());
      let quit = unsafe { CreateEventW(std::ptr::null(), 0, 0, quit.as_ptr()) };
      QUIT.store(quit as isize, Ordering::Relaxed);
      return Some(super::Guard {});
    }

    // Ring the doorbell instead of touching the other process's window: its
    // own `logic()` turns this into a normal viewport command.
    let ev = wide(&show_name());
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
    let event = wide(&quit_name());
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

    let name = wide(&instance_name());
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

  /// The machine-wide quit event.
  ///
  /// `Local\` names end at the session boundary, so the per-user doorbells
  /// cannot reach a window in another session. This name lives in `Global\`,
  /// and it exists only while the process that raised it holds the handle — no
  /// stale marker can end a later window. Only an elevated process may create
  /// it; anyone may open it with `SYNCHRONIZE` just to look.
  const MACHINE_QUIT_NAME: &str = r"Global\thunet-machine-quit";
  /// SYSTEM and administrators may set it; interactive users may only wait.
  const MACHINE_QUIT_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100000;;;IU)";

  pub struct MachineQuit {
    handle: windows_sys::Win32::Foundation::HANDLE,
  }

  impl Drop for MachineQuit {
    fn drop(&mut self) {
      // SAFETY: the handle came from CreateEventW and is closed exactly once.
      unsafe { CloseHandle(self.handle) };
    }
  }

  pub fn machine_quit() -> Result<MachineQuit, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
      ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    let sddl = wide(MACHINE_QUIT_SDDL);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: both pointers are valid for the call; the descriptor is released
    // below.
    let ok = unsafe {
      ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl.as_ptr(),
        SDDL_REVISION_1,
        &mut descriptor,
        std::ptr::null_mut(),
      )
    };
    if ok == 0 || descriptor.is_null() {
      // SAFETY: plain call with no arguments.
      let err = unsafe { GetLastError() };
      return Err(format!("cannot parse the quit DACL (error {err})"));
    }
    let attributes = SECURITY_ATTRIBUTES {
      nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
      lpSecurityDescriptor: descriptor,
      bInheritHandle: 0,
    };
    let name = wide(MACHINE_QUIT_NAME);
    // Manual reset: every waiting window has to see the request, not just the
    // first one. The name lives in the Global namespace, which needs
    // SeCreateGlobalPrivilege - i.e. the administrator rights an uninstall has.
    // SAFETY: the name and the attributes outlive the call.
    let handle = unsafe { CreateEventW(&attributes, 1, 0, name.as_ptr()) };
    // SAFETY: the descriptor was allocated by the conversion above.
    unsafe { LocalFree(descriptor as _) };
    if handle.is_null() {
      // SAFETY: plain call with no arguments.
      let err = unsafe { GetLastError() };
      return Err(format!(
        "cannot create the machine quit event (error {err})"
      ));
    }
    // SAFETY: the handle is live.
    unsafe { SetEvent(handle) };
    Ok(MachineQuit { handle })
  }

  pub fn machine_quit_requested() -> bool {
    let name = wide(MACHINE_QUIT_NAME);
    // SAFETY: plain Win32 calls on a NUL-terminated name that outlives them.
    let handle = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
      return false;
    }
    // SAFETY: the handle came from OpenEventW.
    let signaled = unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
    // SAFETY: the handle is closed exactly once.
    unsafe { CloseHandle(handle) };
    signaled
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
    // One running window per **state directory**, not per user: two portable
    // setups may run side by side, and neither may ring the other's doorbell.
    // `quit_path`/`show_path` derive from this name, so they follow along.
    base.join(format!("thunet-{}.lock", crate::config::state_tag()))
  }

  fn show_path() -> PathBuf {
    lock_path().with_extension("show")
  }

  fn quit_path() -> PathBuf {
    // A `.deb` uninstall rings this marker from `packaging/debian/prerm`
    // (the MSI uses `thunet quit`); renaming it there in the same commit.
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

  /// The machine-wide quit channel on Linux: a socket only root can create.
  ///
  /// A named event has no direct Unix equivalent; a filesystem socket in `/run`
  /// is the closest thing with the same properties. It exists only while the
  /// sender holds it, any user can connect to ask "is it up?", and nobody but
  /// root can create, replace or remove it, because `/run` belongs to root.
  #[cfg(target_os = "linux")]
  pub(super) mod machine {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Fixed name: the signal is per machine, not per user or state directory.
    const SOCKET: &str = "/run/thunet-quit.sock";

    pub struct MachineQuit {
      stop: Arc<AtomicBool>,
      thread: Option<std::thread::JoinHandle<()>>,
      path: PathBuf,
    }

    impl Drop for MachineQuit {
      fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
          let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
      }
    }

    pub fn machine_quit() -> Result<MachineQuit, String> {
      machine_quit_at(PathBuf::from(SOCKET))
    }

    pub fn machine_quit_requested() -> bool {
      requested_at(Path::new(SOCKET))
    }

    /// Bind the signal at `path`. Split out so a test can use a temporary
    /// directory instead of `/run`.
    pub(crate) fn machine_quit_at(path: PathBuf) -> Result<MachineQuit, String> {
      // A crashed sender leaves the socket file behind. It is not listening, so
      // removing it is safe; only root reaches this code at all.
      match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot replace {}: {e}", path.display())),
      }
      let listener =
        UnixListener::bind(&path).map_err(|e| format!("cannot bind {}: {e}", path.display()))?;
      // Connect needs write permission on the socket; creation stays impossible
      // for users because `/run` itself is root-owned.
      let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666));
      let stop = Arc::new(AtomicBool::new(false));
      let thread = {
        let stop = stop.clone();
        std::thread::spawn(move || {
          // Every window probes by connecting and sends nothing, so the
          // connections are accepted and dropped to keep the backlog empty.
          let _ = listener.set_nonblocking(true);
          while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
              Ok(_) => {}
              Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
          }
        })
      };
      Ok(MachineQuit {
        stop,
        thread: Some(thread),
        path,
      })
    }

    pub(crate) fn requested_at(path: &Path) -> bool {
      // A plain file, or a socket whose sender is gone, answers with an error:
      // a crash must never turn into a spurious quit later.
      UnixStream::connect(path).is_ok()
    }
  }

  #[cfg(target_os = "linux")]
  pub use machine::{MachineQuit, machine_quit, machine_quit_requested};

  /// No machine-wide service on this Unix: a portable-only platform has no
  /// windows outside the caller's own session to signal.
  #[cfg(not(target_os = "linux"))]
  pub struct MachineQuit;

  #[cfg(not(target_os = "linux"))]
  pub fn machine_quit() -> Result<MachineQuit, String> {
    Err("this platform has no machine-wide service".into())
  }

  #[cfg(not(target_os = "linux"))]
  pub fn machine_quit_requested() -> bool {
    false
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use super::*;

  fn socket_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("thunet-single-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("quit.sock")
  }

  /// The machine-wide signal exists exactly while the sender holds it, and a
  /// window sees it by connecting.
  #[test]
  fn the_machine_signal_is_a_live_socket() {
    let path = socket_path("live");
    assert!(!imp::machine::requested_at(&path), "nothing is up yet");
    let guard = imp::machine::machine_quit_at(path.clone()).expect("bind");
    assert!(imp::machine::requested_at(&path), "the signal is up");
    drop(guard);
    assert!(!imp::machine::requested_at(&path), "dropping withdraws it");
    assert!(!path.exists(), "and removes the socket file");
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
  }

  /// A marker left by a crashed sender is dead, not a request, and the next
  /// sender replaces it instead of failing.
  #[test]
  fn a_stale_socket_is_not_a_request() {
    let path = socket_path("stale");
    std::fs::write(&path, b"").expect("stale file");
    assert!(
      !imp::machine::requested_at(&path),
      "a plain file says nothing"
    );
    let guard = imp::machine::machine_quit_at(path.clone()).expect("replace");
    assert!(imp::machine::requested_at(&path));
    drop(guard);
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
  }
}
