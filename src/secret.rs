//! Credential storage.
//!
//! Windows: the account/password pair lives in **Windows Credential Manager**
//! as a generic credential (visible in Control Panel → Credential Manager →
//! Windows Credentials), encrypted by the OS and readable only by the current user.
//!
//! Other platforms: there is no portable OS keyring available without extra
//! dependencies, so the pair is written to `credential.json` in the config
//! directory with owner-only (`0600`) permissions.

/// Target name shown in Windows Credential Manager.
pub const TARGET: &str = "thunet/auth4.tsinghua.edu.cn";

/// Save or overwrite the stored credential.
pub fn save(username: &str, password: &str) -> Result<(), String> {
  imp::save(TARGET, username, password)
}

/// Load `(username, password)`; `None` when nothing is stored.
pub fn load() -> Option<(String, String)> {
  imp::load(TARGET)
}

/// Remove the stored credential (best effort).
pub fn delete() {
  imp::delete(TARGET)
}

#[cfg(windows)]
mod imp {
  use std::ffi::c_void;

  use windows_sys::Win32::Security::Credentials::{
    CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
  };

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  unsafe fn pwstr_to_string(p: *const u16) -> String {
    if p.is_null() {
      return String::new();
    }
    let mut len = 0usize;
    // SAFETY: caller guarantees a NUL-terminated string.
    while unsafe { *p.add(len) } != 0 {
      len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
  }

  pub fn save(target_name: &str, username: &str, password: &str) -> Result<(), String> {
    let mut target = wide(target_name);
    let mut user = wide(username);
    // Credential-Manager blobs are raw bytes; UTF-8 round-trips fine.
    let mut blob = password.as_bytes().to_vec();

    let cred = CREDENTIALW {
      Flags: 0,
      Type: CRED_TYPE_GENERIC,
      TargetName: target.as_mut_ptr(),
      Comment: std::ptr::null_mut(),
      LastWritten: unsafe { std::mem::zeroed() },
      CredentialBlobSize: blob.len() as u32,
      CredentialBlob: blob.as_mut_ptr(),
      Persist: 2, // CRED_PERSIST_LOCAL_MACHINE
      AttributeCount: 0,
      Attributes: std::ptr::null_mut(),
      TargetAlias: std::ptr::null_mut(),
      UserName: user.as_mut_ptr(),
    };

    // SAFETY: buffers outlive the call; the struct matches the Win32 layout.
    let ok = unsafe { CredWriteW(&cred, 0) };
    if ok == 0 {
      Err(format!(
        "CredWriteW failed: {}",
        std::io::Error::last_os_error()
      ))
    } else {
      Ok(())
    }
  }

  pub fn load(target_name: &str) -> Option<(String, String)> {
    let target = wide(target_name);
    let mut p: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: standard CredRead usage; result freed with CredFree.
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) };
    if ok == 0 || p.is_null() {
      return None;
    }
    let (user, pass) = unsafe {
      let cred = &*p;
      let user = pwstr_to_string(cred.UserName);
      let bytes = std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
      (user, String::from_utf8_lossy(bytes).into_owned())
    };
    unsafe { CredFree(p as *const c_void) };
    Some((user, pass))
  }

  pub fn delete(target_name: &str) {
    let target = wide(target_name);
    // SAFETY: deleting a possibly-missing entry just returns FALSE.
    unsafe {
      CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0);
    }
  }

  #[cfg(test)]
  mod tests {
    const SCRATCH: &str = "thunet/__selftest__";

    #[test]
    fn credential_roundtrip() {
      super::delete(SCRATCH);
      assert!(
        super::load(SCRATCH).is_none(),
        "scratch entry should be gone"
      );

      super::save(SCRATCH, "some-user", "p@ss w0rd \u{00e4}\u{00f6}").expect("save should succeed");
      let (user, pass) = super::load(SCRATCH).expect("entry should exist after save");
      assert_eq!(user, "some-user");
      assert_eq!(pass, "p@ss w0rd \u{00e4}\u{00f6}");

      // Overwrite must replace, not duplicate.
      super::save(SCRATCH, "other-user", "new").expect("overwrite should succeed");
      let (user, pass) = super::load(SCRATCH).expect("entry should exist after overwrite");
      assert_eq!((user.as_str(), pass.as_str()), ("other-user", "new"));

      super::delete(SCRATCH);
      assert!(
        super::load(SCRATCH).is_none(),
        "delete should remove the entry"
      );
    }
  }
}

#[cfg(not(windows))]
mod imp {
  use std::path::PathBuf;

  /// Fallback location: next to `config.json`, never inside it.
  fn path() -> PathBuf {
    crate::config::dir().join("credential.json")
  }

  #[derive(serde::Serialize, serde::Deserialize)]
  struct Entry {
    username: String,
    password: String,
  }

  pub fn save(_target: &str, username: &str, password: &str) -> Result<(), String> {
    let file = path();
    if let Some(dir) = file.parent() {
      std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let entry = Entry {
      username: username.to_string(),
      password: password.to_string(),
    };
    let body = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
    std::fs::write(&file, body).map_err(|e| e.to_string())?;
    // Owner-only: this is the only protection on platforms without a keyring.
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    }
    Ok(())
  }

  pub fn load(_target: &str) -> Option<(String, String)> {
    let body = std::fs::read(path()).ok()?;
    let entry: Entry = serde_json::from_slice(&body).ok()?;
    Some((entry.username, entry.password))
  }

  pub fn delete(_target: &str) {
    let _ = std::fs::remove_file(path());
  }
}
