//! Peer and service identity.
//!
//! The control channel is machine-wide, so both ends authenticate each other:
//!
//! * the core checks that a frontend comes from an interactive user;
//! * the frontend checks that the process listening on the endpoint really is
//!   the service, not a user-run process that grabbed the free name while the
//!   service was stopped.
//!
//! Windows uses token SIDs and session ids; Unix uses the effective uid from
//! `SO_PEERCRED` and the owner of the machine state directory.

#[cfg(windows)]
mod imp {
  use std::mem::size_of;

  use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
  use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, LookupAccountNameW, TOKEN_QUERY, TOKEN_USER, TokenSessionId,
    TokenUser,
  };
  use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
  };

  /// Virtual service account used by the installed service.
  const SERVICE_ACCOUNT: &str = r"NT SERVICE\thunet";

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  fn token_of(pid: u32) -> Option<HANDLE> {
    // SAFETY: plain Win32 calls; every handle is closed below.
    unsafe {
      let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
      if process.is_null() {
        return None;
      }
      let mut token: HANDLE = std::ptr::null_mut();
      let ok = OpenProcessToken(process, TOKEN_QUERY, &mut token);
      CloseHandle(process);
      (ok != 0).then_some(token)
    }
  }

  /// The token user SID as raw bytes. SIDs have no embedded NULs and the
  /// length is part of the structure, so a byte vector is a faithful copy.
  pub fn process_user_sid(pid: u32) -> Option<Vec<u8>> {
    let token = token_of(pid)?;
    let sid = token_user_sid(token);
    // SAFETY: the handle came from `OpenProcessToken` and is no longer used.
    unsafe {
      CloseHandle(token);
    }
    sid
  }

  fn token_user_sid(token: HANDLE) -> Option<Vec<u8>> {
    // SAFETY: the token handle is valid; buffers outlive the calls and are
    // sized by the first, size-querying call.
    unsafe {
      let mut len = 0u32;
      GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
      if len == 0 {
        return None;
      }
      // TOKEN_USER contains pointers, so keep the buffer pointer-aligned.
      let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
      let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
      if ok == 0 {
        return None;
      }
      let user = &*(buf.as_ptr() as *const TOKEN_USER);
      if user.User.Sid.is_null() {
        return None;
      }
      let sid_len = GetLengthSid(user.User.Sid) as usize;
      let mut sid = vec![0u8; sid_len];
      std::ptr::copy_nonoverlapping(user.User.Sid.cast::<u8>(), sid.as_mut_ptr(), sid_len);
      Some(sid)
    }
  }

  /// The user SID of the token currently impersonated by this thread.
  pub fn current_thread_user_sid() -> Option<Vec<u8>> {
    let token = thread_token()?;
    let sid = token_user_sid(token);
    // SAFETY: the handle came from `OpenThreadToken` and is no longer used.
    unsafe {
      CloseHandle(token);
    }
    sid
  }

  /// The session id of the token currently impersonated by this thread.
  #[allow(dead_code)]
  pub fn current_thread_session() -> Option<u32> {
    let token = thread_token()?;
    // SAFETY: the token handle is valid; `session` is sized as TokenSessionId
    // expects.
    let session = unsafe {
      let mut session = 0u32;
      let mut len = 0u32;
      let ok = GetTokenInformation(
        token,
        crate::ident::imp::TokenSessionId,
        &mut session as *mut u32 as *mut _,
        size_of::<u32>() as u32,
        &mut len,
      );
      CloseHandle(token);
      (ok != 0).then_some(session)
    };
    session
  }

  fn thread_token() -> Option<HANDLE> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `OpenThreadToken` with `TRUE` falls back to the process token;
    // the handle is closed by the caller.
    let ok = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    (ok != 0).then_some(token)
  }

  pub fn is_service_sid(sid: &[u8]) -> bool {
    account_sid(SERVICE_ACCOUNT).is_some_and(|expected| expected == sid)
  }

  pub fn process_session(pid: u32) -> Option<u32> {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

    let mut session = 0u32;
    // SAFETY: plain Win32 call; the output pointer is valid.
    let ok = unsafe { ProcessIdToSessionId(pid, &mut session) };
    (ok != 0).then_some(session)
  }

  fn account_sid(name: &str) -> Option<Vec<u8>> {
    let name = wide(name);
    // SAFETY: null system name means the local machine; the size-query call is
    // documented, and the second call writes into buffers of the reported size.
    unsafe {
      let mut sid_len = 0u32;
      let mut domain_len = 0u32;
      let mut use_type = 0i32;
      LookupAccountNameW(
        std::ptr::null(),
        name.as_ptr(),
        std::ptr::null_mut(),
        &mut sid_len,
        std::ptr::null_mut(),
        &mut domain_len,
        &mut use_type,
      );
      if sid_len == 0 {
        return None;
      }
      let mut sid = vec![0u8; sid_len as usize];
      let mut domain = vec![0u16; domain_len as usize];
      let ok = LookupAccountNameW(
        std::ptr::null(),
        name.as_ptr(),
        sid.as_mut_ptr().cast(),
        &mut sid_len,
        domain.as_mut_ptr(),
        &mut domain_len,
        &mut use_type,
      );
      (ok != 0).then_some(sid)
    }
  }

  fn current_pid() -> u32 {
    // SAFETY: no arguments, always succeeds.
    unsafe { GetCurrentProcessId() }
  }

  pub fn is_service_pid(pid: u32) -> bool {
    let Some(sid) = process_user_sid(pid) else {
      return false;
    };
    is_service_sid(&sid)
  }

  pub fn is_current_service() -> bool {
    is_service_pid(current_pid())
  }

  /// Owner SID of the named-pipe handle, obtained without opening the server
  /// process token. A standard user may query the pipe's security descriptor
  /// because `GENERIC_READ` carries `READ_CONTROL`.
  pub fn pipe_owner_sid(handle: HANDLE) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};

    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is valid; the API allocates the descriptor and the
    // owner pointer points inside it, released with LocalFree below.
    unsafe {
      let status = GetSecurityInfo(
        handle,
        SE_KERNEL_OBJECT,
        OWNER_SECURITY_INFORMATION,
        &mut owner,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut sd,
      );
      if status != 0 || owner.is_null() {
        if !sd.is_null() {
          LocalFree(sd as _);
        }
        return None;
      }
      let len = GetLengthSid(owner) as usize;
      let mut sid = vec![0u8; len];
      std::ptr::copy_nonoverlapping(owner.cast::<u8>(), sid.as_mut_ptr(), len);
      LocalFree(sd as _);
      Some(sid)
    }
  }

  pub fn pipe_owner_is_service(handle: HANDLE) -> bool {
    let Some(sid) = pipe_owner_sid(handle) else {
      return false;
    };
    account_sid(SERVICE_ACCOUNT).is_some_and(|expected| expected == sid)
  }

  pub fn pipe_owner_is_current_user(handle: HANDLE) -> bool {
    matches!(
      (pipe_owner_sid(handle), process_user_sid(current_pid())),
      (Some(owner), Some(me)) if owner == me
    )
  }

  /// The current process's user SID as an SDDL string (`S-1-5-21-...`).
  pub fn current_user_sid_string() -> Option<String> {
    sid_string(&process_user_sid(current_pid())?)
  }

  /// SID the machine pipe must grant access to: the dedicated virtual account.
  /// The pipe DACL is applied to every new instance, so the service itself
  /// needs `FILE_CREATE_PIPE_INSTANCE`; without this, the first client can
  /// connect but the listener cannot create the next instance.
  pub fn service_sid_strings() -> Vec<String> {
    account_sid(SERVICE_ACCOUNT)
      .and_then(|sid| sid_string(&sid))
      .into_iter()
      .collect()
  }

  pub fn sid_to_string(sid: &[u8]) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; the API allocates `text`, released below.
    if unsafe { ConvertSidToStringSidW(sid.as_ptr() as *mut _, &mut text) } == 0 {
      return None;
    }
    let mut len = 0usize;
    // SAFETY: the API returned a NUL-terminated UTF-16 string.
    unsafe {
      while !(*text.add(len)).eq(&0) {
        len += 1;
      }
      let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
      LocalFree(text as _);
      Some(value)
    }
  }

  fn sid_string(sid: &[u8]) -> Option<String> {
    sid_to_string(sid)
  }

  pub fn describe(pid: u32) -> String {
    match process_session(pid) {
      Some(session) => format!("pid {pid}, session {session}"),
      None => format!("pid {pid}"),
    }
  }
}

#[cfg(unix)]
mod imp {
  /// The uid the machine core runs as: the trust anchor for the machine
  /// endpoint, and the answer to "may this process serve it?".
  ///
  /// `/run/thunet` is the anchor that works. systemd creates it for the unit
  /// (`RuntimeDirectory=thunet`), owns it by the service account and leaves it
  /// world-readable, and removes it when the unit stops. The state directory
  /// cannot serve that purpose under `DynamicUser=` + `StateDirectory=`: the
  /// real directory lives in `/var/lib/private/thunet` (mode 0700 root:root),
  /// so the `/var/lib/thunet` symlink cannot even be stat()ed by an
  /// unprivileged frontend - every window would refuse the real service. The
  /// fallback keeps unit files that predate the runtime directory working (a
  /// tarball install, `--state-dir`).
  pub fn expected_service_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let runtime = std::path::Path::new("/run/thunet");
    let fallback = crate::config::machine_dir();
    let anchor = if runtime.is_dir() {
      runtime
    } else {
      fallback.as_path()
    };
    std::fs::metadata(anchor).ok().map(|meta| meta.uid())
  }

  /// May this process serve the machine endpoint?
  ///
  /// Root always may: it can read the sealed credential anyway, and a
  /// hand-started `thunet core --machine` runs as root.
  pub fn is_current_service() -> bool {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    euid == 0 || expected_service_uid().is_some_and(|uid| uid == euid)
  }

  pub fn is_current_user(uid: u32) -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == uid }
  }

  pub fn describe(uid: u32) -> String {
    format!("uid {uid}")
  }

  pub fn is_interactive(_uid: u32) -> bool {
    // Every local uid that can open the socket is a user. A stricter check
    // would consult logind; the important boundary is the server identity.
    true
  }
}

pub use imp::*;

#[cfg(all(test, windows))]
mod tests {
  /// The service must be able to see that a frontend is interactive without
  /// opening its process; `ProcessIdToSessionId` is the access-free check.
  #[test]
  fn this_process_is_interactive() {
    assert!(super::process_session(std::process::id()).is_some_and(|session| session != 0));
  }
}
