//! SRun3K wire primitives: quirk base64, XXTEA variant, MD5/SHA1 hex.
//!
//! Ported from the shipped `thunet.exe` string evidence and cross-checked
//! against the public SRun portal scheme; the unit tests below are the exact
//! published vectors, so a green `cargo test` proves wire compatibility.

use md5::{Digest as _, Md5};
use sha1::Sha1;

/// SRun's custom base64 alphabet (standard padding is kept).
const ALPHABET: &[u8; 64] = b"LVoJPiCN2R8G90yg+hmFHuacZ1OWMnrsSTXkYpUq/3dlbfKwv6xztjI7DeBE45QA";

/// Base64 with the SRun alphabet.
pub fn quirk_b64(data: &[u8]) -> String {
  let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
  for chunk in data.chunks(3) {
    let b0 = chunk[0] as u32;
    let b1 = *chunk.get(1).unwrap_or(&0) as u32;
    let b2 = *chunk.get(2).unwrap_or(&0) as u32;
    let h = (b0 << 16) | (b1 << 8) | b2;
    out.push(ALPHABET[((h >> 18) & 0x3f) as usize] as char);
    out.push(ALPHABET[((h >> 12) & 0x3f) as usize] as char);
    if chunk.len() > 1 {
      out.push(ALPHABET[((h >> 6) & 0x3f) as usize] as char);
    } else {
      out.push('=');
    }
    if chunk.len() > 2 {
      out.push(ALPHABET[(h & 0x3f) as usize] as char);
    } else {
      out.push('=');
    }
  }
  out
}

fn sencode(data: &[u8], include_len: bool) -> Vec<u32> {
  let mut v = Vec::with_capacity(data.len().div_ceil(4) + 1);
  for chunk in data.chunks(4) {
    let mut w = 0u32;
    for (i, b) in chunk.iter().enumerate() {
      w |= (*b as u32) << (8 * i);
    }
    v.push(w);
  }
  if include_len {
    v.push(data.len() as u32);
  }
  v
}

fn lencode(v: &[u32]) -> Vec<u8> {
  let mut out = Vec::with_capacity(v.len() * 4);
  for w in v {
    out.extend_from_slice(&w.to_le_bytes());
  }
  out
}

/// SRun's XXTEA variant (delta `0x9E3779B9`, wrapping u32 arithmetic).
pub fn xencode(data: &[u8], key: &[u8]) -> Vec<u8> {
  if data.is_empty() {
    return Vec::new();
  }
  let mut v = sencode(data, true);
  let mut k = sencode(key, false);
  if k.is_empty() {
    k.push(0);
  }
  let n = v.len() - 1;
  let mut z = v[n];
  let mut d: u32 = 0;
  let rounds = 6 + 52 / (n as u32 + 1);
  for _ in 0..rounds {
    d = d.wrapping_add(0x9E37_79B9);
    let e = ((d >> 2) & 3) as usize;
    for p in 0..=n {
      let y = if p == n { v[0] } else { v[p + 1] };
      let mut m = (z >> 5) ^ (y << 2);
      m = m.wrapping_add((y >> 3) ^ (z << 4) ^ (d ^ y));
      m = m.wrapping_add(k[(p & 3) ^ e] ^ z);
      v[p] = v[p].wrapping_add(m);
      z = v[p];
    }
  }
  lencode(&v)
}

pub fn md5_hex(s: &str) -> String {
  format!("{:x}", Md5::digest(s.as_bytes()))
}

pub fn sha1_hex(s: &str) -> String {
  format!("{:x}", Sha1::digest(s.as_bytes()))
}

#[cfg(test)]
mod tests {
  use super::*;

  const KEY: &str = "aa0edd0fff7dd9f1f0ae4e981ec0114c7b0bf6f67c4895bed4f4ac634e97ecf2";

  fn enc(plain: &str, key: &str) -> String {
    quirk_b64(&xencode(plain.as_bytes(), key.as_bytes()))
  }

  #[test]
  fn xencode_vectors() {
    assert_eq!(enc("", KEY), "");
    assert_eq!(enc("1", KEY), "NmsaR0fCm5H=");
    assert_eq!(
      enc("agfawegwq12834eqrge", KEY),
      "DAxHygvRUjlDyJjmvChIzuavMsjy7B9L"
    );
    assert_eq!(
      enc("agfawegwq12834eqrge", &"0".repeat(64)),
      "TOdQ9ggF2y/mskS6Orkg+eUZIok9vqJr"
    );
    assert_eq!(
      enc("9$02%8r89)(&22{}we[f]|s", KEY),
      "kCG+xmvGAhCV717Y80Fk0o1YJ8SYvBdnUmQoqS=="
    );
  }

  #[test]
  fn md5_vector() {
    assert_eq!(md5_hex("DEMO-PASSWORD"), "358785705ffba606cc083b89ffef6c16");
  }
}
