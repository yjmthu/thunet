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
/// an uninstall leaves them alone. `minimized` is this window's state, which the
/// answer carries so an upgrade can restore it the same way.
pub fn machine_quit_requested(minimized: bool) -> bool {
  imp::machine_quit_requested(minimized)
}

pub use imp::MachineQuit;

/// One machine window that answered the quit signal.
///
/// The answer is what lets an upgrade put the windows back afterwards: only a
/// machine-mode window answers, and it names the user and the state to restore.
/// Portable windows never see the signal and never answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuitAck {
  pub uid: u32,
  pub pid: u32,
  /// Windows session id; 0 on Unix, where the uid is what a relaunch uses.
  pub session: u32,
  /// The window was hidden or minimized to the tray when it answered.
  pub minimized: bool,
}

/// Write the record an upgrade reads to put the windows back.
///
/// An empty list writes nothing and removes nothing: an upgrade runs the quit
/// step twice (the new package's sequence, then the removed package's), and the
/// second run must not erase what the first one recorded. [`restore_windows`]
/// is the only consumer, and it deletes the record it reads; the ten-minute
/// freshness check drops whatever an aborted install left behind.
pub fn write_quit_record(acks: &[QuitAck]) -> Result<(), String> {
  imp::write_quit_record(acks)
}

/// Put back the windows a previous installer step closed.
///
/// The MSI runs this as SYSTEM after the files are in place; the `.deb` runs
/// it from `postinst`. A session (Windows) or user (Linux) with nobody logged
/// on is skipped, and a record older than ten minutes is ignored.
pub fn restore_windows() -> Result<(), String> {
  imp::restore_windows()
}

/// One line of the record an upgrade reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordEntry {
  Uid { uid: u32, minimized: bool },
  Session { session: u32, minimized: bool },
}

/// Parse the record written by [`write_quit_record`].
///
/// Line-based on purpose: the file sits between two installer processes, and a
/// human reading it during a failed upgrade should not need a decoder.
fn parse_quit_record(text: &str) -> (u128, Vec<RecordEntry>) {
  let mut at = 0;
  let mut entries = Vec::new();
  for line in text.lines() {
    let mut parts = line.split_whitespace();
    match parts.next() {
      Some("at") => at = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
      Some("uid") => {
        let uid = parts.next().and_then(|v| v.parse::<u32>().ok());
        let state = parts.next().unwrap_or("minimized");
        if let Some(uid) = uid {
          entries.push(RecordEntry::Uid {
            uid,
            minimized: state != "visible",
          });
        }
      }
      Some("session") => {
        let session = parts.next().and_then(|v| v.parse::<u32>().ok());
        let state = parts.next().unwrap_or("minimized");
        if let Some(session) = session {
          entries.push(RecordEntry::Session {
            session,
            minimized: state != "visible",
          });
        }
      }
      _ => {}
    }
  }
  (at, entries)
}

/// A record older than ten minutes belongs to an installation that never
/// completed, and must not reopen anything.
fn record_is_fresh(at: u128, now: u128) -> bool {
  at > 0 && now.saturating_sub(at) <= 600_000
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
  /// The answer channel. A `Global\` event carries no payload, so the state each
  /// window reports travels on a machine-wide pipe instead. Its transport, its
  /// DACL and the peer identity come from [`crate::ipc`], so this half cannot
  /// drift from what the core link already does.
  const MACHINE_QUIT_PIPE: &str = "thunet-machine-quit";

  pub struct MachineQuit {
    event: windows_sys::Win32::Foundation::HANDLE,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    acks: std::sync::Arc<std::sync::Mutex<Vec<super::QuitAck>>>,
  }

  impl Drop for MachineQuit {
    fn drop(&mut self) {
      self.stop.store(true, Ordering::SeqCst);
      if let Some(thread) = self.thread.take() {
        let _ = thread.join();
      }
      // SAFETY: the handle came from CreateEventW and is closed exactly once.
      unsafe { CloseHandle(self.event) };
    }
  }

  impl MachineQuit {
    pub fn acks(&self) -> Vec<super::QuitAck> {
      match self.acks.lock() {
        Ok(list) => list.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
      }
    }
  }

  /// Build the security attributes a named object is created with.
  fn security(
    sddl: &str,
  ) -> Result<
    (
      windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
      windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    ),
    String,
  > {
    use windows_sys::Win32::Security::Authorization::{
      ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    let text = wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: both pointers are valid for the call; the descriptor is released
    // by the caller.
    let ok = unsafe {
      ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text.as_ptr(),
        SDDL_REVISION_1,
        &mut descriptor,
        std::ptr::null_mut(),
      )
    };
    if ok == 0 || descriptor.is_null() {
      let err = unsafe { GetLastError() };
      return Err(format!("cannot parse the quit DACL (error {err})"));
    }
    let attributes = SECURITY_ATTRIBUTES {
      nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
      lpSecurityDescriptor: descriptor,
      bInheritHandle: 0,
    };
    Ok((attributes, descriptor))
  }

  pub fn machine_quit() -> Result<MachineQuit, String> {
    use windows_sys::Win32::Foundation::LocalFree;

    // The answer channel first: a window that sees the event connects here, so
    // the pipe must exist before the event can be seen.
    let listener = crate::ipc::endpoint::Endpoint::bind(
      MACHINE_QUIT_PIPE,
      crate::ipc::INTERACTIVE_SDDL,
    )?;

    let (attributes, descriptor) = security(MACHINE_QUIT_SDDL)?;
    let event_name = wide(MACHINE_QUIT_NAME);
    // Manual reset: every waiting window has to see the request, not just the
    // first one. The name lives in the Global namespace, which needs
    // SeCreateGlobalPrivilege - i.e. the administrator rights an uninstall has.
    // SAFETY: the name and the attributes outlive the call.
    let event = unsafe { CreateEventW(&attributes, 1, 0, event_name.as_ptr()) };
    // SAFETY: the descriptor was allocated by `security`.
    unsafe { LocalFree(descriptor as _) };
    if event.is_null() {
      let err = unsafe { GetLastError() };
      return Err(format!(
        "cannot create the machine quit event (error {err})"
      ));
    }

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let acks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let thread = {
      let stop = stop.clone();
      let acks = acks.clone();
      std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
          let Some(mut conn) = listener.accept() else {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
          };
          // The kernel names the client, so the session cannot be spoofed; the
          // one line only says which state to restore. A window that says
          // nothing is put back in the tray.
          let pid = conn.pid();
          let minimized = conn
            .read_line()
            .is_none_or(|line| !line.trim().eq_ignore_ascii_case("visible"));
          if let Some(pid) = pid
            && let Some(session) = crate::ident::process_session(pid)
            && session > 0
            && let Ok(mut list) = acks.lock()
          {
            list.push(super::QuitAck {
              uid: 0,
              pid,
              session,
              minimized,
            });
          }
          // Only now may the window go away: its answer is recorded.
          conn.ack();
        }
      })
    };
    // Only now may a window see the signal.
    unsafe { SetEvent(event) };
    Ok(MachineQuit {
      event,
      stop,
      thread: Some(thread),
      acks,
    })
  }

  pub fn machine_quit_requested(minimized: bool) -> bool {
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
    if !signaled {
      return false;
    }

    // Leave the state on the answer channel and stay connected until the sender
    // has it: a pipe drops both the buffered line and the client's identity if
    // the client goes away before the server accepts the connection. A failure
    // here still ends the window; the sender then restores it in the tray.
    if let Ok(mut conn) = crate::ipc::endpoint::connect(MACHINE_QUIT_PIPE) {
      let line = if minimized {
        "minimized\n"
      } else {
        "visible\n"
      };
      let _ = conn.write_line(line);
      let _ = conn.wait_ack();
    }
    true
  }

  pub fn write_quit_record(acks: &[super::QuitAck]) -> Result<(), String> {
    use std::collections::BTreeMap;

    // Nothing answered: write nothing, and deliberately delete nothing. An
    // upgrade runs the quit step twice - the new package's sequence, then the
    // removed package's - and the second run finds the machine quiet; erasing
    // the record here would leave the restore step with no windows to put back.
    if acks.is_empty() {
      return Ok(());
    }

    let dir = crate::config::machine_dir();
    let path = dir.join("quit-record");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let at = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis())
      .unwrap_or(0);
    // One line per session: a machine setup has one window per session, and a
    // visible one wins over a hidden one.
    let mut sessions: BTreeMap<u32, bool> = BTreeMap::new();
    for ack in acks {
      if ack.session == 0 {
        continue;
      }
      sessions
        .entry(ack.session)
        .and_modify(|minimized| *minimized = *minimized && ack.minimized)
        .or_insert(ack.minimized);
    }
    let mut text = format!("at {at}\n");
    for (session, minimized) in sessions {
      let state = if minimized { "minimized" } else { "visible" };
      text.push_str(&format!("session {session} {state}\n"));
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
  }

  /// Put back the windows a previous installer step closed, one per session.
  pub fn restore_windows() -> Result<(), String> {
    let path = crate::config::machine_dir().join("quit-record");
    let text = match std::fs::read_to_string(&path) {
      Ok(text) => text,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
      Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let _ = std::fs::remove_file(&path);

    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis())
      .unwrap_or(0);
    let (at, entries) = super::parse_quit_record(&text);
    if !super::record_is_fresh(at, now) {
      return Ok(());
    }
    for entry in entries {
      let super::RecordEntry::Session { session, minimized } = entry else {
        continue;
      };
      if let Err(e) = launch_in_session(session, minimized) {
        crate::diag::line(&format!("quit-record: session {session}: {e}"));
      }
    }
    Ok(())
  }

  /// Start the window as the user logged on to `session`.
  fn launch_in_session(session: u32, minimized: bool) -> Result<(), String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Security::{
      DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TokenPrimary,
    };
    use windows_sys::Win32::System::Environment::{
      CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    use windows_sys::Win32::System::RemoteDesktop::WTSQueryUserToken;
    use windows_sys::Win32::System::Threading::{
      CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { WTSQueryUserToken(session, &mut token) } == 0 {
      let err = unsafe { GetLastError() };
      return Err(format!("no user token (error {err})"));
    }
    let mut primary: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
      DuplicateTokenEx(
        token,
        TOKEN_ALL_ACCESS,
        std::ptr::null(),
        SecurityImpersonation,
        TokenPrimary,
        &mut primary,
      )
    } != 0;
    // SAFETY: the token came from WTSQueryUserToken and is closed once.
    unsafe { CloseHandle(token) };
    if !duplicated {
      let err = unsafe { GetLastError() };
      return Err(format!("cannot duplicate the token (error {err})"));
    }

    let mut environment: *mut std::ffi::c_void = std::ptr::null_mut();
    let have_environment = unsafe { CreateEnvironmentBlock(&mut environment, primary, 0) } != 0;

    let exe = crate::exe_path().ok_or("cannot determine the executable path")?;
    let mut command: Vec<u16> = format!(
      "\"{}\" {}",
      exe.display(),
      if minimized {
        "--minimized"
      } else {
        "--visible"
      }
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();
    let mut desktop: Vec<u16> = "winsta0\\default"
      .encode_utf16()
      .chain(std::iter::once(0))
      .collect();
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let created = unsafe {
      CreateProcessAsUserW(
        primary,
        std::ptr::null(),
        command.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        0,
        CREATE_UNICODE_ENVIRONMENT,
        if have_environment {
          environment
        } else {
          std::ptr::null()
        },
        std::ptr::null(),
        &startup,
        &mut process,
      )
    } != 0;
    if have_environment {
      unsafe { DestroyEnvironmentBlock(environment) };
    }
    if created {
      // SAFETY: the handles came from CreateProcessAsUserW.
      unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
      }
    }
    // SAFETY: the token came from DuplicateTokenEx and is closed once.
    unsafe { CloseHandle(primary) };
    if created {
      Ok(())
    } else {
      let err = unsafe { GetLastError() };
      Err(format!("cannot start the window (error {err})"))
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

  // ------------------------------------------------------------ quit record

  #[cfg(target_os = "linux")]
  pub(super) fn record_text(acks: &[super::QuitAck], at: u128) -> String {
    let mut text = format!("at {at}\n");
    // One line per user: a machine setup has one window per session, and the
    // relaunch is per user manager. If any of that user's windows was open,
    // the restored one is open too.
    let mut users = std::collections::BTreeMap::new();
    for ack in acks {
      users
        .entry(ack.uid)
        .and_modify(|minimized| *minimized = *minimized && ack.minimized)
        .or_insert(ack.minimized);
    }
    for (uid, minimized) in users {
      let state = if minimized { "minimized" } else { "visible" };
      text.push_str(&format!("uid {uid} {state}\n"));
    }
    text
  }

  #[cfg(target_os = "linux")]
  pub fn write_quit_record(acks: &[super::QuitAck]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    // `/run`, not the machine state directory: the record lives for one install
    // transaction, and the service's `RuntimeDirectory` is removed when systemd
    // stops it - which the same transaction does right after the quit.
    const RECORD: &str = "/run/thunet-quit-record";
    let path = std::path::Path::new(RECORD);
    // Write-only, like the Windows half: a quit that closed no window must not
    // erase a record an earlier step of the same transaction wrote, and
    // `restore_windows` is what deletes the record it reads.
    if acks.is_empty() {
      return Ok(());
    }
    let at = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis())
      .unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(path)
      .map_err(|e| format!("{RECORD}: {e}"))?;
    file
      .write_all(record_text(acks, at).as_bytes())
      .map_err(|e| format!("{RECORD}: {e}"))
  }

  #[cfg(not(target_os = "linux"))]
  pub fn write_quit_record(_acks: &[super::QuitAck]) -> Result<(), String> {
    Ok(())
  }

  /// Put back the windows a previous installer step closed, one per user.
  ///
  /// `postinst` runs this as root. `systemd-run --user --machine=<user>@.host`
  /// is what puts the window into that user's graphical session: the user
  /// manager already carries `DISPLAY`/`WAYLAND_DISPLAY`, which a maintainer
  /// script cannot reconstruct by hand.
  #[cfg(target_os = "linux")]
  pub fn restore_windows() -> Result<(), String> {
    use std::process::{Command, Stdio};

    const RECORD: &str = "/run/thunet-quit-record";
    let text = match std::fs::read_to_string(RECORD) {
      Ok(text) => text,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
      Err(e) => return Err(format!("{RECORD}: {e}")),
    };
    let _ = std::fs::remove_file(RECORD);

    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_millis())
      .unwrap_or(0);
    let (at, entries) = super::parse_quit_record(&text);
    if !super::record_is_fresh(at, now) {
      return Ok(());
    }
    let exe = crate::exe_path().ok_or("cannot determine the executable path")?;
    for entry in entries {
      let super::RecordEntry::Uid { uid, minimized } = entry else {
        continue;
      };
      let Some(user) = user_name(uid) else {
        continue;
      };
      let flag = if minimized {
        "--minimized"
      } else {
        "--visible"
      };
      let machine = format!("--machine={user}@.host");
      let status = Command::new("systemd-run")
        .args(["--user", &machine, "--collect", "--quiet"])
        .arg(&exe)
        .arg(flag)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
      if let Err(e) = status {
        crate::diag::line(&format!("quit-record: {user}: systemd-run: {e}"));
      }
    }
    Ok(())
  }

  /// The account name for a uid, without a `libc` passwd-buffer dance.
  /// `getent` is part of every Debian base system, which is where the `.deb`
  /// runs this.
  #[cfg(target_os = "linux")]
  fn user_name(uid: u32) -> Option<String> {
    let output = std::process::Command::new("getent")
      .args(["passwd", &uid.to_string()])
      .output()
      .ok()?;
    if !output.status.success() {
      return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text
      .split(':')
      .next()
      .filter(|name| !name.is_empty())
      .map(str::to_string)
  }

  /// The `.deb` is the only user of this command, and macOS has no record.
  #[cfg(not(target_os = "linux"))]
  pub fn restore_windows() -> Result<(), String> {
    Ok(())
  }

  /// The machine-wide quit channel on Linux: a socket only root can create.
  ///
  /// A named event has no direct Unix equivalent; a filesystem socket in `/run`
  /// is the closest thing with the same properties. It exists only while the
  /// sender holds it, any user can connect to ask "is it up?", and nobody but
  /// root can create, replace or remove it, because `/run` belongs to root.
  ///
  /// The answer travels back on the same connection: a machine-mode window
  /// writes `{"uid":…,"pid":…}` before it ends, which is what an upgrade reads
  /// later to put it back. A portable window never connects, so it is neither
  /// counted nor restarted.
  #[cfg(target_os = "linux")]
  pub(super) mod machine {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Fixed name: the signal is per machine, not per user or state directory.
    const SOCKET: &str = "/run/thunet-quit.sock";

    pub struct MachineQuit {
      stop: Arc<AtomicBool>,
      thread: Option<std::thread::JoinHandle<()>>,
      path: PathBuf,
      /// Answers collected from the windows that saw the signal.
      acks: Arc<Mutex<Vec<super::super::QuitAck>>>,
    }

    impl MachineQuit {
      /// The windows that answered so far, one entry per answer.
      pub fn acks(&self) -> Vec<super::super::QuitAck> {
        match self.acks.lock() {
          Ok(list) => list.clone(),
          Err(poisoned) => poisoned.into_inner().clone(),
        }
      }
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

    pub fn machine_quit_requested(minimized: bool) -> bool {
      request_at(Path::new(SOCKET), minimized)
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
      // The transport is the one the core link uses; only the "is the signal up"
      // question is per platform, and on Unix the socket file answers it.
      let listener = crate::ipc::endpoint::Endpoint::bind(&path.to_string_lossy(), "")?;
      // Connect needs write permission on the socket; creation stays impossible
      // for users because `/run` itself is root-owned.
      let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666));
      let stop = Arc::new(AtomicBool::new(false));
      let acks = Arc::new(Mutex::new(Vec::new()));
      let thread = {
        let stop = stop.clone();
        let acks = acks.clone();
        std::thread::spawn(move || {
          // The connection *is* the answer: the kernel names the peer, so the
          // sender records who asked without trusting anything the window
          // writes. Portable windows never connect at all.
          while !stop.load(Ordering::SeqCst) {
            let Some(mut conn) = listener.accept() else {
              std::thread::sleep(Duration::from_millis(20));
              continue;
            };
            // The uid and pid come from `SO_PEERCRED`, so they cannot be
            // spoofed; the one line says which state to restore. A window that
            // says nothing is restored in the tray.
            let pid = conn.pid().unwrap_or(0);
            let minimized = conn
              .read_line()
              .is_none_or(|line| !line.trim().eq_ignore_ascii_case("visible"));
            if let Some(uid) = conn.uid().filter(|uid| *uid > 0)
              && let Ok(mut list) = acks.lock()
            {
              list.push(super::super::QuitAck {
                uid,
                pid,
                session: 0,
                minimized,
              });
            }
            // Only now may the window go away: its answer is recorded.
            conn.ack();
          }
        })
      };
      Ok(MachineQuit {
        stop,
        thread: Some(thread),
        path,
        acks,
      })
    }

    /// Ask whether the signal is up and report this window's state.
    ///
    /// A plain file, or a socket whose sender is gone, fails to connect: a
    /// crash must never turn into a spurious quit later. The uid and pid come
    /// from the kernel, so they cannot be spoofed; the line only says whether
    /// the window was hidden in the tray. `true` means the window ends.
    pub(crate) fn request_at(path: &Path, minimized: bool) -> bool {
      let Ok(mut conn) = crate::ipc::endpoint::connect(&path.to_string_lossy()) else {
        return false;
      };
      let line = if minimized {
        "minimized\n"
      } else {
        "visible\n"
      };
      // A failed write still means the signal was there; the sender then
      // restores the window in the tray instead of its original state. The wait
      // is what keeps the connection alive until the answer is recorded.
      let _ = conn.write_line(line);
      let _ = conn.wait_ack();
      true
    }
  }

  #[cfg(target_os = "linux")]
  pub use machine::{MachineQuit, machine_quit, machine_quit_requested};

  /// No machine-wide service on this Unix: a portable-only platform has no
  /// windows outside the caller's own session to signal.
  #[cfg(not(target_os = "linux"))]
  pub struct MachineQuit;

  #[cfg(not(target_os = "linux"))]
  impl MachineQuit {
    pub fn acks(&self) -> Vec<super::QuitAck> {
      Vec::new()
    }
  }

  #[cfg(not(target_os = "linux"))]
  pub fn machine_quit() -> Result<MachineQuit, String> {
    Err("this platform has no machine-wide service".into())
  }

  #[cfg(not(target_os = "linux"))]
  pub fn machine_quit_requested(_minimized: bool) -> bool {
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

  /// The signal exists exactly while the sender holds it; a window's answer
  /// travels back on the same connection and is kept by the sender.
  #[test]
  fn the_machine_signal_collects_the_windows_that_answer() {
    let path = socket_path("live");
    assert!(!imp::machine::request_at(&path, false), "nothing is up yet");
    let guard = imp::machine::machine_quit_at(path.clone()).expect("bind");
    assert!(imp::machine::request_at(&path, false), "the signal is up");
    // The accept thread reads the answer asynchronously.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while guard.acks().is_empty() && std::time::Instant::now() < deadline {
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let acks = guard.acks();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].uid, me);
    assert_eq!(acks[0].pid, std::process::id());
    assert!(!acks[0].minimized, "the answer said the window was open");
    drop(guard);
    assert!(
      !imp::machine::request_at(&path, false),
      "dropping withdraws it"
    );
    assert!(!path.exists(), "and removes the socket file");
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
  }

  /// A socket left by a crashed sender is dead, not a request, and the next
  /// sender replaces it instead of failing.
  #[test]
  fn a_stale_socket_is_not_a_request() {
    let path = socket_path("stale");
    std::fs::write(&path, b"").expect("stale file");
    assert!(
      !imp::machine::request_at(&path, true),
      "a plain file says nothing"
    );
    let guard = imp::machine::machine_quit_at(path.clone()).expect("replace");
    assert!(imp::machine::request_at(&path, true));
    drop(guard);
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
  }

  /// The record is line-based, one line per user, so `postinst` can read it
  /// with `awk`; two sessions of one user come back once, and a visible one
  /// wins over a hidden one.
  #[test]
  fn the_record_names_each_user_and_its_state() {
    let text = imp::record_text(
      &[
        QuitAck {
          uid: 1000,
          pid: 1,
          session: 0,
          minimized: true,
        },
        QuitAck {
          uid: 1000,
          pid: 2,
          session: 1,
          minimized: false,
        },
        QuitAck {
          uid: 1001,
          pid: 3,
          session: 0,
          minimized: true,
        },
      ],
      123,
    );
    assert_eq!(text, "at 123\nuid 1000 visible\nuid 1001 minimized\n");
  }

  /// The record is shared with Windows: one parser, two key kinds, and a state
  /// that defaults to the tray.
  #[test]
  fn the_record_parses_by_kind() {
    let (at, entries) = parse_quit_record(
      "at 123\nuid 1000 minimized\nsession 2 visible\nnot a record line\nuid 1001\n",
    );
    assert_eq!(at, 123);
    assert_eq!(
      entries,
      vec![
        RecordEntry::Uid {
          uid: 1000,
          minimized: true
        },
        RecordEntry::Session {
          session: 2,
          minimized: false
        },
        RecordEntry::Uid {
          uid: 1001,
          minimized: true
        }
      ]
    );
  }

  /// Ten minutes is the line between "this transaction" and "a leftover".
  #[test]
  fn a_record_is_fresh_for_ten_minutes() {
    assert!(record_is_fresh(1_000, 1_000));
    assert!(record_is_fresh(1_000, 601_000));
    assert!(!record_is_fresh(1_000, 601_001));
    assert!(record_is_fresh(1_000, 999), "clock skew is not staleness");
    assert!(!record_is_fresh(0, 1_000), "no timestamp is not fresh");
  }
}
