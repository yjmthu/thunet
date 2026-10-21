//! One-shot administrator elevation (Windows).
//!
//! The window never runs elevated. The few operations the Service Control
//! Manager refuses to an ordinary caller — creating the core service, changing
//! its start type, deleting it — re-launch this executable with the `runas`
//! verb, which shows the UAC prompt. The elevated copy performs exactly one SCM
//! call and exits, so the privilege is held for the length of that call and not
//! by anything that keeps running.

/// Error string returned when the user dismisses the UAC prompt. Callers that
/// show a message can translate it instead of reporting a failure.
pub const CANCELLED: &str = "cancelled";

/// `true` when this process already has the rights an SCM mutation needs.
#[cfg(windows)]
pub fn is_elevated() -> bool {
  use windows_sys::Win32::Foundation::CloseHandle;
  use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
  };
  use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

  let mut token = std::ptr::null_mut();
  // SAFETY: the token handle is closed below; the buffers outlive the call.
  unsafe {
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
      return false;
    }
    let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
    let mut len = 0u32;
    let ok = GetTokenInformation(
      token,
      TokenElevation,
      &mut elevation as *mut _ as *mut std::ffi::c_void,
      std::mem::size_of::<TOKEN_ELEVATION>() as u32,
      &mut len,
    );
    CloseHandle(token);
    ok != 0 && elevation.TokenIsElevated != 0
  }
}

/// Run this executable again as administrator and wait for it to finish.
///
/// `args` are the command-line arguments of the second process, e.g.
/// `["service-admin", "enable"]`. Returns [`Err`] with [`CANCELLED`] when the
/// user says no, and with a description otherwise.
#[cfg(windows)]
pub fn run_as_admin(args: &[&str]) -> Result<(), String> {
  use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
  use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
  use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
  };

  fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
  }

  let exe = crate::exe_path().ok_or("cannot determine the executable path")?;
  let verb = wide("runas");
  let file = wide(&exe.to_string_lossy());
  // `lpParameters` is one command line, not an argument vector: a state
  // directory with a space in it has to be quoted or the child sees two.
  let line = args
    .iter()
    .map(|a| {
      if a.contains(' ') {
        format!("\"{a}\"")
      } else {
        (*a).to_string()
      }
    })
    .collect::<Vec<_>>()
    .join(" ");
  let params = wide(&line);

  let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
  info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
  info.fMask = SEE_MASK_NOCLOSEPROCESS;
  info.lpVerb = verb.as_ptr();
  info.lpFile = file.as_ptr();
  info.lpParameters = params.as_ptr();
  info.nShow = 1; // SW_SHOWNORMAL

  // SAFETY: the structure points at buffers that outlive the call, and the
  // process handle it reports is closed below.
  unsafe {
    if ShellExecuteExW(&mut info) == 0 {
      let err = GetLastError();
      return Err(if err == ERROR_CANCELLED {
        CANCELLED.into()
      } else {
        format!("cannot request administrator rights (error {err})")
      });
    }
    if info.hProcess.is_null() {
      // The child was handed off without a handle; nothing left to wait for.
      return Ok(());
    }
    WaitForSingleObject(info.hProcess, INFINITE);
    let mut code = 1u32;
    GetExitCodeProcess(info.hProcess, &mut code);
    CloseHandle(info.hProcess);
    if code == 0 {
      Ok(())
    } else {
      Err(format!("the elevated helper failed (exit code {code})"))
    }
  }
}
