//! Credential storage: one sealed blob, identical on every platform.
//!
//! The account/password pair is encrypted with ChaCha20-Poly1305 under a random
//! per-install key (`key.bin`) and written to `credential.bin` in the machine
//! state directory (mode 600 / service-only ACL). `config.json` never holds the
//! password.
//!
//! Why not the OS keyrings (Windows Credential Manager, macOS Keychain, Secret
//! Service)? All three belong to a logged-on interactive user, but the core has
//! to log in *before* anyone logs in, and it runs under a different account than
//! the window (a system service). A machine-scoped random key is the form that
//! works in all of those cases without a passphrase.
//!
//! What this buys: the blob is useless without `key.bin`, and both files are
//! readable only by the service account and administrators. What it does not
//! buy: protection from root/Administrators (they can read the key or the core's
//! memory), or from a full-disk image that includes the key. Only a
//! user-supplied passphrase or a TPM would stop those, and both cost unattended
//! startup.
//!
//! There is only one format; a blob whose magic does not match is ignored.

use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// File magic. Also fed to the AEAD as associated data, so a truncated or
/// foreign file is rejected instead of decrypted into garbage.
const MAGIC: &[u8; 8] = b"THUNET-1";
/// Domain separation for the random data key.
const DOMAIN: &[u8] = b"thunet/credential/v1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// Sealed credential file in the config directory.
const CREDENTIAL_FILE: &str = "credential.bin";
/// Random data key. Lives next to the blob and is protected by the same ACL.
const KEY_FILE: &str = "key.bin";

#[derive(Serialize, Deserialize)]
struct Entry {
  username: String,
  password: String,
}

pub fn save_in(dir: &Path, username: &str, password: &str) -> Result<(), String> {
  let plain = serde_json::to_vec(&Entry {
    username: username.to_string(),
    password: password.to_string(),
  })
  .map_err(|e| e.to_string())?;
  std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
  private_dir(dir)?;
  let key = data_key(dir)?;
  let blob = seal(&key, &plain)?;
  write_atomic(&dir.join(CREDENTIAL_FILE), &blob)
}

pub fn delete_in(dir: &Path) {
  let _ = std::fs::remove_file(dir.join(CREDENTIAL_FILE));
}

pub fn load_in(dir: &Path) -> Option<(String, String)> {
  let blob = std::fs::read(dir.join(CREDENTIAL_FILE)).ok()?;
  if !blob.starts_with(MAGIC.as_slice()) {
    return None;
  }
  let plain = unseal(&read_key(dir).ok().flatten()?, &blob)?;
  let entry: Entry = serde_json::from_slice(&plain).ok()?;
  Some((entry.username, entry.password))
}

/// Read `key.bin` without creating it.
///
/// `Ok(None)` means "there is no usable key": the file is absent (first save)
/// or has the wrong length (a truncated write). A permission error is returned
/// instead of being folded into `None`, so the caller can say *why* the key
/// could not be used rather than reporting it as missing.
fn read_key(dir: &Path) -> Result<Option<[u8; KEY_LEN]>, String> {
  let path = dir.join(KEY_FILE);
  match std::fs::read(&path) {
    Ok(bytes) if bytes.len() == KEY_LEN => Ok(Some(bytes.try_into().expect("length checked"))),
    Ok(_) => Ok(None),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(format!("{}: {e}", path.display())),
  }
}

/// Read `key.bin`, creating it if this is the first save. The key is random:
/// unlike the machine id it is not derivable from public information, so a
/// copied `credential.bin` alone is useless.
fn data_key(dir: &Path) -> Result<[u8; KEY_LEN], String> {
  if let Some(key) = read_key(dir)? {
    return Ok(key);
  }
  let path = dir.join(KEY_FILE);
  // A wrong-length file is not a key. It may be the remains of an interrupted
  // first write; the blob it would have opened is unusable either way, so drop
  // both and start over instead of failing every subsequent save.
  if path.exists() {
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    delete_in(dir);
  }
  let mut key = [0u8; KEY_LEN];
  getrandom::fill(&mut key).map_err(|e| format!("rng: {e}"))?;
  std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
  private_dir(dir)?;
  // `create_new` avoids clobbering a key that appeared in between; if it did,
  // re-read it so two concurrent first saves use the same key.
  match write_new(&path, &key) {
    Ok(()) => Ok(key),
    Err(e) => read_key(dir)?.ok_or_else(|| format!("cannot create {}: {e}", path.display())),
  }
}

fn private_dir(dir: &Path) -> Result<(), String> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
      .map_err(|e| e.to_string())?;
  }
  let _ = dir;
  Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  let mut file = options.open(path).map_err(|e| e.to_string())?;
  use std::io::Write as _;
  file.write_all(bytes).map_err(|e| e.to_string())?;
  file.sync_all().map_err(|e| e.to_string())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
  let tmp = path.with_extension("tmp");
  let _ = std::fs::remove_file(&tmp);
  write_new(&tmp, bytes)?;
  #[cfg(unix)]
  std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
  #[cfg(not(unix))]
  {
    // Windows refuses to rename over an existing file; Unix replaces it
    // atomically and keeps the old blob on a crash.
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
      .map_err(|e| e.to_string())?;
  }
  Ok(())
}

/// 32-byte key derived from the random `key.bin` material.
fn derive(key: &[u8]) -> [u8; 32] {
  let mut h = Sha256::new();
  h.update(DOMAIN);
  h.update([0u8]);
  h.update(key);
  h.finalize().into()
}

fn cipher(key: &[u8]) -> ChaCha20Poly1305 {
  // The only failure mode of `new_from_slice` is a wrong key length.
  ChaCha20Poly1305::new_from_slice(&derive(key)).expect("32-byte key")
}

/// `MAGIC ‖ nonce ‖ ciphertext`.
fn seal(key: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
  let mut nonce = [0u8; NONCE_LEN];
  getrandom::fill(&mut nonce).map_err(|e| format!("rng: {e}"))?;
  let ct = cipher(key)
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

fn unseal(key: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
  let rest = blob.strip_prefix(MAGIC.as_slice())?;
  let (nonce, ct) = rest.split_at_checked(NONCE_LEN)?;
  cipher(key)
    .decrypt(
      Nonce::from_slice(nonce),
      Payload {
        msg: ct,
        aad: MAGIC,
      },
    )
    .ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_sealed_blob_round_trips() {
    let key = [7u8; KEY_LEN];
    let blob = seal(&key, b"hello").unwrap();
    assert_eq!(unseal(&key, &blob).as_deref(), Some(&b"hello"[..]));
  }

  #[test]
  fn another_key_cannot_open_it() {
    let blob = seal(&[7u8; KEY_LEN], b"hello").unwrap();
    assert!(unseal(&[8u8; KEY_LEN], &blob).is_none());
  }

  #[test]
  fn tampering_and_truncation_are_rejected() {
    let key = [7u8; KEY_LEN];
    let mut blob = seal(&key, b"hello").unwrap();
    let last = blob.len() - 1;
    blob[last] ^= 1;
    assert!(unseal(&key, &blob).is_none(), "flipped byte");

    let blob = seal(&key, b"hello").unwrap();
    for cut in [0, MAGIC.len() - 1, MAGIC.len(), MAGIC.len() + NONCE_LEN - 1] {
      assert!(unseal(&key, &blob[..cut]).is_none(), "cut at {cut}");
    }
  }

  #[test]
  fn a_blob_with_a_foreign_key_is_ignored() {
    let dir = scratch("foreign");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(CREDENTIAL_FILE), b"THUNET-1junk").unwrap();
    std::fs::write(dir.join(KEY_FILE), [7u8; KEY_LEN]).unwrap();
    assert_eq!(load_in(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn a_truncated_key_is_replaced_instead_of_blocking_saves() {
    let dir = scratch("badkey");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(KEY_FILE), b"short").unwrap();

    save_in(&dir, "alice", "hunter2").expect("a corrupt key must not block a save");
    assert_eq!(
      std::fs::read(dir.join(KEY_FILE)).unwrap().len(),
      KEY_LEN,
      "a fresh key is written"
    );
    assert_eq!(
      load_in(&dir),
      Some(("alice".to_string(), "hunter2".to_string()))
    );
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn every_save_uses_a_fresh_nonce() {
    let key = [7u8; KEY_LEN];
    let a = seal(&key, b"same").unwrap();
    let b = seal(&key, b"same").unwrap();
    assert_ne!(a, b, "a reused nonce would break the cipher");
  }

  #[test]
  fn the_derived_key_depends_on_the_key_file() {
    assert_ne!(derive(&[7u8; KEY_LEN]), derive(&[8u8; KEY_LEN]));
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
    save_in(&dir, "alice", "hunter2").expect("save should succeed");

    let file = dir.join(CREDENTIAL_FILE);
    let raw = std::fs::read(&file).expect("the file should exist");
    assert!(raw.starts_with(MAGIC), "current magic prefix");
    assert!(
      !raw.windows(7).any(|w| w == b"hunter2"),
      "the password must not be readable in the file"
    );
    assert!(dir.join(KEY_FILE).exists(), "a random data key is written");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
      assert_eq!(mode, 0o600, "only the owner may read the ciphertext");
      let key_mode = std::fs::metadata(dir.join(KEY_FILE))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
      assert_eq!(key_mode, 0o600, "only the owner may read the key");
    }

    assert_eq!(
      load_in(&dir),
      Some(("alice".to_string(), "hunter2".to_string()))
    );

    delete_in(&dir);
    assert_eq!(load_in(&dir), None, "delete should remove the file");
    let _ = std::fs::remove_dir_all(&dir);
  }
}
