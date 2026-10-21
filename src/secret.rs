//! Credential storage: one sealed blob, identical on every platform.
//!
//! The account/password pair is encrypted with ChaCha20-Poly1305 under a key
//! derived from this machine's identity, and written to `credential.bin` in the
//! config directory (mode 600). `config.json` never holds the password.
//!
//! Why not the OS keyrings (Windows Credential Manager, macOS Keychain, Secret
//! Service)? All three belong to a logged-on interactive user, but the core has
//! to log in *before* anyone logs in, and it may run under a different account
//! than the window (a system service, a lingering systemd user manager). A
//! machine-bound key is the only form that works in all of those cases, and one
//! code path is easier to keep honest than three.
//!
//! What this buys: the blob is useless once it leaves this machine (backups,
//! dotfile repositories, a synced config directory) and never contains
//! plaintext. What it does not buy: protection from code already running on
//! this machine — that code can read the same machine identity and decrypt the
//! blob itself. Only a user-supplied passphrase or a TPM would stop that, and
//! both cost unattended startup.

use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// File magic. Also fed to the AEAD as associated data, so a truncated or
/// foreign file is rejected instead of decrypted into garbage.
const MAGIC: &[u8; 8] = b"THUNET-1";
/// Domain separation: this machine identity must not yield the same key in
/// some other feature.
const DOMAIN: &[u8] = b"thunet/credential/v1";
const NONCE_LEN: usize = 12;
/// Sealed credential file in the config directory.
const CREDENTIAL_FILE: &str = "credential.bin";
/// The plain-text file written by 0.1.x; removed whenever the store is written
/// or cleared, so an old password cannot linger on disk.
const LEGACY_FILE: &str = "credential.json";

#[derive(Serialize, Deserialize)]
struct Entry {
  username: String,
  password: String,
}

/// Save or overwrite the stored credential.
pub fn save(username: &str, password: &str) -> Result<(), String> {
  save_in(&crate::config::dir(), username, password)
}

/// Load `(username, password)`; `None` when nothing is stored or when the blob
/// was written by another machine and cannot be opened.
pub fn load() -> Option<(String, String)> {
  load_in(&crate::config::dir())
}

/// Remove the stored credential (best effort).
pub fn delete() {
  delete_in(&crate::config::dir())
}

fn save_in(dir: &Path, username: &str, password: &str) -> Result<(), String> {
  let id = machine_id().ok_or("cannot identify this machine, refusing to store the password")?;
  let plain = serde_json::to_vec(&Entry {
    username: username.to_string(),
    password: password.to_string(),
  })
  .map_err(|e| e.to_string())?;
  let blob = seal(&id, &plain)?;

  std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
  let file = dir.join(CREDENTIAL_FILE);
  std::fs::write(&file, blob).map_err(|e| e.to_string())?;
  // Still worth setting: other users have no business reading the ciphertext.
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
      .map_err(|e| e.to_string())?;
  }
  remove_legacy(dir);
  Ok(())
}

fn load_in(dir: &Path) -> Option<(String, String)> {
  let blob = std::fs::read(dir.join(CREDENTIAL_FILE)).ok()?;
  let plain = unseal(&machine_id()?, &blob)?;
  let entry: Entry = serde_json::from_slice(&plain).ok()?;
  Some((entry.username, entry.password))
}

fn delete_in(dir: &Path) {
  let _ = std::fs::remove_file(dir.join(CREDENTIAL_FILE));
  remove_legacy(dir);
}

fn remove_legacy(dir: &Path) {
  let _ = std::fs::remove_file(dir.join(LEGACY_FILE));
}

/// 32-byte key for `identity`, domain-separated from every other use.
fn derive(identity: &str) -> [u8; 32] {
  let mut h = Sha256::new();
  h.update(DOMAIN);
  h.update([0u8]);
  h.update(identity.as_bytes());
  h.finalize().into()
}

fn cipher(identity: &str) -> ChaCha20Poly1305 {
  // The only failure mode of `new_from_slice` is a wrong key length.
  ChaCha20Poly1305::new_from_slice(&derive(identity)).expect("32-byte key")
}

/// `MAGIC ‖ nonce ‖ ciphertext`.
fn seal(identity: &str, plain: &[u8]) -> Result<Vec<u8>, String> {
  let mut nonce = [0u8; NONCE_LEN];
  getrandom::fill(&mut nonce).map_err(|e| format!("rng: {e}"))?;
  let ct = cipher(identity)
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: plain,
        aad: MAGIC,
      },
    )
    .map_err(|_| "encryption failed".to_string())?;
  let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ct.len());
  out.extend_from_slice(MAGIC);
  out.extend_from_slice(&nonce);
  out.extend_from_slice(&ct);
  Ok(out)
}

fn unseal(identity: &str, blob: &[u8]) -> Option<Vec<u8>> {
  let rest = blob.strip_prefix(MAGIC.as_slice())?;
  let (nonce, ct) = rest.split_at_checked(NONCE_LEN)?;
  cipher(identity)
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: ct,
        aad: MAGIC,
      },
    )
    .ok()
}

/// Something stable and machine-specific to derive the key from.
fn machine_id() -> Option<String> {
  platform_id().or_else(hostname)
}

/// Linux keeps a random 128-bit id in a file, Windows keeps the same thing in
/// the registry; macOS has neither, so `hostname` stands in until a release
/// actually needs more (IOKit's `IOPlatformUUID` would mean linking IOKit).
#[cfg(target_os = "linux")]
fn platform_id() -> Option<String> {
  first_line(&["/etc/machine-id", "/var/lib/dbus/machine-id"])
}

#[cfg(windows)]
fn platform_id() -> Option<String> {
  registry_string(r"SOFTWARE\Microsoft\Cryptography", "MachineGuid")
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_id() -> Option<String> {
  None
}

fn hostname() -> Option<String> {
  first_line(&["/proc/sys/kernel/hostname", "/etc/hostname"])
    .or_else(|| std::env::var("COMPUTERNAME").ok())
    .or_else(|| std::env::var("HOSTNAME").ok())
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// First non-empty line of the first readable file in `paths`.
fn first_line(paths: &[&str]) -> Option<String> {
  for path in paths {
    let Ok(text) = std::fs::read_to_string(path) else {
      continue;
    };
    if let Some(line) = text.lines().next() {
      let line = line.trim();
      if !line.is_empty() {
        return Some(line.to_string());
      }
    }
  }
  None
}

/// Read a `REG_SZ` value from `HKLM\<subkey>`, 64-bit view: a 32-bit build would
/// otherwise be redirected to `WOW6432Node`, where `MachineGuid` is not
/// guaranteed to exist.
#[cfg(windows)]
fn registry_string(subkey: &str, value: &str) -> Option<String> {
  use std::mem::{size_of, size_of_val};

  use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, RRF_RT_REG_SZ, RegGetValueW,
  };

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  let key = wide(subkey);
  let name = wide(value);
  let mut buf = [0u16; 128];
  let mut len = size_of_val(&buf) as u32;
  // SAFETY: both strings are NUL-terminated and outlive the call; `buf` is
  // valid for `len` bytes and the call updates `len` to what it wrote.
  let status = unsafe {
    RegGetValueW(
      HKEY_LOCAL_MACHINE,
      key.as_ptr(),
      name.as_ptr(),
      RRF_RT_REG_SZ | KEY_WOW64_64KEY,
      std::ptr::null_mut(),
      buf.as_mut_ptr().cast(),
      &mut len,
    )
  };
  if status != 0 {
    return None;
  }
  let written = (len as usize / size_of::<u16>()).min(buf.len());
  let text = String::from_utf16_lossy(&buf[..written]);
  let text = text.trim_end_matches('\0').trim();
  (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_sealed_blob_round_trips() {
    let blob = seal("machine-a", b"hello").unwrap();
    assert_eq!(unseal("machine-a", &blob).as_deref(), Some(&b"hello"[..]));
  }

  #[test]
  fn another_machine_cannot_open_it() {
    let blob = seal("machine-a", b"hello").unwrap();
    assert!(unseal("machine-b", &blob).is_none());
  }

  #[test]
  fn tampering_and_truncation_are_rejected() {
    let mut blob = seal("machine-a", b"hello").unwrap();
    let last = blob.len() - 1;
    blob[last] ^= 1;
    assert!(unseal("machine-a", &blob).is_none(), "flipped byte");

    let blob = seal("machine-a", b"hello").unwrap();
    for cut in [0, MAGIC.len() - 1, MAGIC.len(), MAGIC.len() + NONCE_LEN - 1] {
      assert!(unseal("machine-a", &blob[..cut]).is_none(), "cut at {cut}");
    }
  }

  #[test]
  fn every_save_uses_a_fresh_nonce() {
    let a = seal("machine-a", b"same").unwrap();
    let b = seal("machine-a", b"same").unwrap();
    assert_ne!(a, b, "a reused nonce would break the cipher");
  }

  #[test]
  fn the_key_depends_on_the_identity() {
    assert_ne!(derive("machine-a"), derive("machine-b"));
  }

  #[test]
  fn this_machine_has_an_identity() {
    assert!(
      machine_id().is_some(),
      "storing a password would be impossible here"
    );
  }

  /// Scratch directory per test: tests in a crate share one process, so the
  /// name has to be unique even when they run in parallel.
  fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("thunet-secret-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
  }

  #[test]
  fn the_stored_file_is_sealed_and_round_trips() {
    let dir = scratch("roundtrip");
    save_in(&dir, "1234567890", "hunter2").expect("save should succeed");

    let file = dir.join(CREDENTIAL_FILE);
    let raw = std::fs::read(&file).expect("the file should exist");
    assert!(raw.starts_with(MAGIC), "magic prefix");
    assert!(
      !raw.windows(7).any(|w| w == b"hunter2"),
      "the password must not be readable in the file"
    );
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
      assert_eq!(mode, 0o600, "only the owner may read the ciphertext");
    }

    assert_eq!(
      load_in(&dir),
      Some(("1234567890".to_string(), "hunter2".to_string()))
    );

    delete_in(&dir);
    assert_eq!(load_in(&dir), None, "delete should remove the file");
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// An upgrade must not leave the 0.1.x plain-text password behind.
  #[test]
  fn writing_or_clearing_removes_the_plaintext_legacy_file() {
    let dir = scratch("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(LEGACY_FILE), b"{\"password\":\"old\"}").unwrap();
    save_in(&dir, "u", "p").unwrap();
    assert!(!dir.join(LEGACY_FILE).exists(), "save should clean it up");

    std::fs::write(dir.join(LEGACY_FILE), b"{}").unwrap();
    delete_in(&dir);
    assert!(!dir.join(LEGACY_FILE).exists(), "delete should clean it up");
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A blob from another machine is treated as "nothing stored", which is what
  /// makes the file safe to copy around and useless when stolen.
  #[test]
  fn a_blob_from_another_machine_reads_as_empty() {
    let dir = scratch("foreign");
    std::fs::create_dir_all(&dir).unwrap();
    let blob = seal("some-other-machine", br#"{"username":"u","password":"p"}"#).unwrap();
    std::fs::write(dir.join(CREDENTIAL_FILE), blob).unwrap();
    assert_eq!(load_in(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
  }
}
