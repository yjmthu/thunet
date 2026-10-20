//! Wi-Fi SSID inspection.
//!
//! Some campus SSIDs (`Tsinghua-Secure`) authenticate at layer 2 through
//! WPA2/WPA3-Enterprise: the OS logs in with the saved 802.1X credentials and
//! there is **no captive portal** to talk to. Attempting an SRun portal login
//! there is pointless, so the worker skips it.

/// SSIDs whose authentication happens below the portal. Compared after
/// separator normalization, so `Tsinghua Secure`/`Tsinghua_Secure` also match.
const NO_PORTAL_SSIDS: &[&str] = &["tsinghua-secure"];

/// Lower-case and unify `_`/space to `-` so minor naming differences match.
fn normalize(ssid: &str) -> String {
  ssid.trim().to_ascii_lowercase().replace([' ', '_'], "-")
}

/// True for SSIDs that never need a portal login.
pub fn is_no_portal_ssid(ssid: &str) -> bool {
  let normalized = normalize(ssid);
  NO_PORTAL_SSIDS.iter().any(|s| normalized == *s)
}

/// Pull the SSID out of `netsh wlan show interfaces` output.
///
/// The `SSID` label itself is not localized, but `BSSID` also starts with it,
/// hence the `:` right after the key.
fn parse_ssid(text: &str) -> Option<String> {
  for line in text.lines() {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("SSID") else {
      continue;
    };
    let Some(rest) = rest.trim_start().strip_prefix(':') else {
      continue;
    };
    let name = rest.trim();
    if !name.is_empty() {
      return Some(name.to_string());
    }
  }
  None
}

/// SSID of the connected Wi-Fi network, if any.
#[cfg(windows)]
pub fn current_ssid() -> Option<String> {
  use std::os::windows::process::CommandExt as _;
  use std::sync::OnceLock;
  /// Do not flash a console window in this GUI-subsystem process.
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  /// Windows only reveals WLAN details when location access is granted for
  /// desktop apps; probing once is enough because that permission cannot
  /// change while we run.
  static USABLE: OnceLock<bool> = OnceLock::new();

  if !*USABLE.get_or_init(|| {
    std::process::Command::new("netsh")
      .args(["wlan", "show", "interfaces"])
      .creation_flags(CREATE_NO_WINDOW)
      .output()
      .map(|o| o.status.success() && !o.stdout.is_empty())
      .unwrap_or(false)
  }) {
    return None;
  }

  let out = std::process::Command::new("netsh")
    .args(["wlan", "show", "interfaces"])
    .creation_flags(CREATE_NO_WINDOW)
    .output()
    .ok()?;
  parse_ssid(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
pub fn current_ssid() -> Option<String> {
  None
}

/// `Some(ssid)` when the current Wi-Fi authenticates without a portal.
pub fn no_portal_wifi() -> Option<String> {
  let ssid = current_ssid()?;
  is_no_portal_ssid(&ssid).then_some(ssid)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ssid_is_scraped_from_netsh_output() {
    // netsh localizes the other labels, but the `SSID` key itself is untranslated.
    let sample = "\r\nName                   : Wi-Fi\r\nState                  : connected\r\n\
              SSID                   : Tsinghua-Secure\r\n\
              BSSID                  : 00:11:22:33:44:55\r\n";
    assert_eq!(parse_ssid(sample).as_deref(), Some("Tsinghua-Secure"));

    let en = "    Name                   : Wi-Fi\n    State                  : connected\n\
              \x20   SSID                   : Tsinghua\n    BSSID                  : aa:bb\n";
    assert_eq!(parse_ssid(en).as_deref(), Some("Tsinghua"));

    assert_eq!(parse_ssid("    State : disconnected\n"), None);
  }

  #[test]
  fn enterprise_ssids_are_recognized() {
    // The real SSID is hyphenated; the space form is accepted too.
    assert!(is_no_portal_ssid("Tsinghua-Secure"));
    assert!(is_no_portal_ssid("Tsinghua Secure"));
    assert!(is_no_portal_ssid("  tsinghua_secure "));
    assert!(is_no_portal_ssid("TSINGHUA-SECURE"));
    assert!(!is_no_portal_ssid("Tsinghua"));
    assert!(!is_no_portal_ssid("Tsinghua-Secure-5G"));
    assert!(!is_no_portal_ssid(""));
  }
}
