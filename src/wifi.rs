//! Wi-Fi SSID inspection.
//!
//! Some campus SSIDs (`Tsinghua-Secure`) authenticate at layer 2 through
//! WPA2/WPA3-Enterprise: the OS logs in with the saved 802.1X credentials and
//! there is **no captive portal** to talk to. Attempting an SRun portal login
//! there is pointless, so the worker skips it.
//!
//! The other campus SSIDs (`Tsinghua`, `Tsinghua-IPv4`) keep the portal, but
//! the gateway does not always intercept HTTP on them, so the name is the
//! better authority for "this is the campus network": the probe alone would
//! call such a link "internet works, nothing to log in to".

/// SSIDs whose authentication happens below the portal. Compared after
/// separator normalization, so `Tsinghua Secure`/`Tsinghua_Secure` also match.
const NO_PORTAL_SSIDS: &[&str] = &["tsinghua-secure"];

/// Prefixes of the SSIDs that belong to the campus network and still use the
/// portal. The campus adds suffixes (`-IPv4`, `-WiFi6`), hence the prefix test.
const CAMPUS_SSID_PREFIXES: &[&str] = &["tsinghua"];

/// Lower-case and unify `_`/space to `-` so minor naming differences match,
/// and drop the ` 2`, ` 3`, ... that Windows appends when a profile name is
/// already taken (`Tsinghua-Secure 2` is the same network).
fn normalize(ssid: &str) -> String {
  let base = ssid.trim();
  let base = match base.rsplit_once(' ') {
    Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
    _ => base,
  };
  base.to_ascii_lowercase().replace([' ', '_'], "-")
}

/// True for SSIDs that never need a portal login.
pub fn is_no_portal_ssid(ssid: &str) -> bool {
  let normalized = normalize(ssid);
  NO_PORTAL_SSIDS.iter().any(|s| normalized == *s)
}

/// True for SSIDs of the campus network that are served by the portal.
pub fn is_campus_ssid(ssid: &str) -> bool {
  let normalized = normalize(ssid);
  !is_no_portal_ssid(ssid)
    && CAMPUS_SSID_PREFIXES
      .iter()
      .any(|p| normalized.starts_with(p))
}

/// Pull the SSID out of `netsh wlan show interfaces` output.
///
/// The `SSID` label itself is not localized, but `BSSID` also starts with it,
/// hence the `:` right after the key.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
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

/// What the current Wi-Fi says about the portal.
pub enum WifiKind {
  /// This SSID authenticates below the portal (802.1X): no login to make.
  Enterprise(String),
  /// A campus SSID served by the portal: a login is the right thing to do,
  /// even when the gateway does not intercept HTTP on it.
  Campus(String),
  /// A named wireless network that goes through the portal.
  Portal(String),
  /// Nothing wireless is connected: a definitive answer.
  NoWifi,
  /// No name is available *yet*; the reason is for the log. Worth asking again
  /// shortly after a network is joined, because Windows names the profile a
  /// second or two after the address appears.
  Unknown(String),
}

/// Classify the current wireless link. The network profile fallback is only
/// consulted when Windows refuses to name the SSID, and only a whitelisted name
/// decides the verdict, so a wired profile can never be mistaken for a
/// wireless one.
pub fn classify() -> WifiKind {
  match probe_ssid() {
    Ok(Some(ssid)) if is_no_portal_ssid(&ssid) => WifiKind::Enterprise(ssid),
    Ok(Some(ssid)) if is_campus_ssid(&ssid) => WifiKind::Campus(ssid),
    Ok(Some(ssid)) => WifiKind::Portal(ssid),
    Ok(None) => WifiKind::NoWifi,
    Err(reason) => match profile_names() {
      Ok(names) => {
        if let Some(name) = names.iter().find(|name| is_no_portal_ssid(name)) {
          WifiKind::Enterprise(name.clone())
        } else if let Some(name) = names.iter().find(|name| is_campus_ssid(name)) {
          WifiKind::Campus(name.clone())
        } else if names.is_empty() {
          WifiKind::Unknown(format!("{reason}; no network profiles"))
        } else {
          WifiKind::Unknown(format!(
            "{reason}; connected profiles: {}",
            names.join(", ")
          ))
        }
      }
      Err(why) => WifiKind::Unknown(format!("{reason}; {why}")),
    },
  }
}

/// Names of the networks the machine is connected to, via the Network List
/// Manager (the API behind `Get-NetConnectionProfile`).
#[cfg(windows)]
fn profile_names() -> Result<Vec<String>, String> {
  nlm::connected_names()
}

#[cfg(not(windows))]
fn profile_names() -> Result<Vec<String>, String> {
  Ok(Vec::new())
}

/// A COM vtable is positional, so a prefix of it is enough to call the entries
/// in it - but the count of leading slots has to be exact. All three interfaces
/// derive from `IDispatch`, so their own methods start after three `IUnknown`
/// slots plus four `IDispatch` ones.
#[cfg(windows)]
mod nlm {
  use std::ffi::c_void;
  use windows_sys::Win32::Foundation::SysFreeString;
  use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoInitializeEx, CoUninitialize,
  };
  use windows_sys::core::{BSTR, GUID};

  /// `CLSID_NetworkListManager`
  const CLSID_MANAGER: GUID = GUID {
    data1: 0xDCB0_0C01,
    data2: 0x570F,
    data3: 0x4A9B,
    data4: [0x8D, 0x69, 0x19, 0x9F, 0xDB, 0xA5, 0x72, 0x3B],
  };
  /// `IID_INetworkListManager`
  const IID_MANAGER: GUID = GUID {
    data1: 0xDCB0_0000,
    data2: 0x570F,
    data3: 0x4A9B,
    data4: [0x8D, 0x69, 0x19, 0x9F, 0xDB, 0xA5, 0x72, 0x3B],
  };
  /// `NLM_ENUM_NETWORK_CONNECTED`
  const ENUM_CONNECTED: i32 = 1;
  /// `COINIT_APARTMENTTHREADED`
  const COINIT_APARTMENTTHREADED: u32 = 2;
  /// `RPC_E_CHANGED_MODE`: this thread already has COM, in another mode.
  const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

  /// A vtable slot that is never called, so only its width matters.
  type Slot = *const c_void;
  type Release = unsafe extern "system" fn(*mut c_void) -> u32;

  #[repr(C)]
  struct ManagerVtbl {
    unknown: [Slot; 3],
    dispatch: [Slot; 4],
    get_networks: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32,
  }

  #[repr(C)]
  struct EnumVtbl {
    unknown: [Slot; 3],
    dispatch: [Slot; 4],
    /// `get__NewEnum`
    new_enum: Slot,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32,
  }

  #[repr(C)]
  struct NetworkVtbl {
    unknown: [Slot; 3],
    dispatch: [Slot; 4],
    get_name: unsafe extern "system" fn(*mut c_void, *mut BSTR) -> i32,
  }

  /// One interface pointer, released on drop.
  struct Com(*mut c_void);

  impl Com {
    /// Release is slot 2 of every COM vtable.
    fn release(ptr: *mut c_void) {
      unsafe {
        // The object's first word points at the vtable; step through *that*,
        // not through the object.
        let table: *const Release = *(ptr as *const *const Release);
        let release: Release = *table.add(2);
        release(ptr);
      }
    }

    unsafe fn vtbl<T>(&self) -> &T {
      unsafe { &**(self.0 as *const *const T) }
    }
  }

  impl Drop for Com {
    fn drop(&mut self) {
      if !self.0.is_null() {
        Com::release(self.0);
      }
    }
  }

  /// The string a BSTR points at, freed afterwards.
  unsafe fn take(b: BSTR) -> Option<String> {
    if b.is_null() {
      return None;
    }
    unsafe {
      let mut len = 0usize;
      while *b.add(len) != 0 {
        len += 1;
      }
      let text = String::from_utf16_lossy(std::slice::from_raw_parts(b, len));
      SysFreeString(b);
      Some(text)
    }
  }

  /// Names of the connected networks. For a wireless one this is the SSID.
  pub fn connected_names() -> Result<Vec<String>, String> {
    unsafe {
      let hr = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED);
      // S_OK/S_FALSE: this call initialised it, so it must be undone here.
      // RPC_E_CHANGED_MODE: something already did, keep going and leave it.
      let owned = hr >= 0;
      if !owned && hr != RPC_E_CHANGED_MODE {
        return Err(format!("COM is unavailable (0x{:08x})", hr as u32));
      }
      let names = enumerate();
      if owned {
        CoUninitialize();
      }
      names
    }
  }

  unsafe fn enumerate() -> Result<Vec<String>, String> {
    unsafe {
      let mut manager: *mut c_void = std::ptr::null_mut();
      let hr = CoCreateInstance(
        &CLSID_MANAGER,
        std::ptr::null_mut(),
        CLSCTX_INPROC_SERVER,
        &IID_MANAGER,
        &mut manager,
      );
      if hr < 0 || manager.is_null() {
        return Err(format!(
          "the network list is unavailable (0x{:08x})",
          hr as u32
        ));
      }
      let manager = Com(manager);
      let mut list: *mut c_void = std::ptr::null_mut();
      let hr = (manager.vtbl::<ManagerVtbl>().get_networks)(manager.0, ENUM_CONNECTED, &mut list);
      if hr < 0 || list.is_null() {
        return Err(format!(
          "the network list could not be read (0x{:08x})",
          hr as u32
        ));
      }
      let list = Com(list);
      let mut names = Vec::new();
      loop {
        let mut network: *mut c_void = std::ptr::null_mut();
        let mut fetched = 0u32;
        let hr = (list.vtbl::<EnumVtbl>().next)(list.0, 1, &mut network, &mut fetched);
        if hr < 0 || fetched == 0 || network.is_null() {
          break;
        }
        let network = Com(network);
        let mut name: BSTR = std::ptr::null();
        let hr = (network.vtbl::<NetworkVtbl>().get_name)(network.0, &mut name);
        if hr >= 0
          && let Some(name) = take(name)
        {
          names.push(name);
        }
      }
      Ok(names)
    }
  }
}

#[cfg(windows)]
mod imp {
  use super::*;
  use std::os::windows::process::CommandExt as _;
  use std::sync::Mutex;
  use std::time::{Duration, Instant};

  /// Do not flash a console window in this GUI-subsystem process.
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  /// A failure repeats until something outside changes (the radio is switched
  /// on, Location permission is granted, the WLAN service restarts), so it is
  /// worth caching - but it must expire, or one bad moment at start-up would
  /// hide the SSID for the rest of the run. (`OnceLock` did exactly that.)
  const RETRY_AFTER: Duration = Duration::from_secs(60);

  static LAST_FAILURE: Mutex<Option<(Instant, String)>> = Mutex::new(None);

  fn cached_failure() -> Option<String> {
    let guard = LAST_FAILURE.lock().ok()?;
    let (when, reason) = guard.as_ref()?;
    (when.elapsed() < RETRY_AFTER).then(|| reason.clone())
  }

  fn remember(reason: String) -> String {
    if let Ok(mut guard) = LAST_FAILURE.lock() {
      *guard = Some((Instant::now(), reason.clone()));
    }
    reason
  }

  fn forget() {
    if let Ok(mut guard) = LAST_FAILURE.lock() {
      *guard = None;
    }
  }

  pub fn probe_ssid() -> Result<Option<String>, String> {
    if let Some(reason) = cached_failure() {
      return Err(reason);
    }
    let out = match std::process::Command::new("netsh")
      .args(["wlan", "show", "interfaces"])
      .creation_flags(CREATE_NO_WINDOW)
      .output()
    {
      Ok(out) => out,
      Err(e) => return Err(remember(format!("cannot run netsh: {e}"))),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    if let Some(ssid) = parse_ssid(&text) {
      forget();
      return Ok(Some(ssid));
    }
    if out.status.success() {
      // The WLAN stack answered: there simply is no wireless network right now
      // (a cable, a switched-off radio, a machine without Wi-Fi).
      return Ok(None);
    }
    // Windows hides the SSID from processes that lack the location permission
    // and says so in a localized message; quote it rather than parse it.
    let first = text
      .lines()
      .map(str::trim)
      .find(|l| !l.is_empty())
      .unwrap_or("")
      .to_string();
    Err(remember(if first.is_empty() {
      format!("netsh exited with {}", out.status)
    } else {
      format!("netsh cannot read the WLAN state ({first})")
    }))
  }
}

#[cfg(windows)]
pub use imp::probe_ssid;

/// Linux: ask NetworkManager, then read the association with `iw`.
///
/// Both are local and privileged-free. `nmcli` covers a desktop install and
/// costs one short-lived process; `iw` covers a machine without NetworkManager
/// (a server with `systemd-networkd`, a minimal container). A machine with no
/// wireless interface at all answers `Ok(None)`, which is what keeps a wired
/// host quiet instead of logging an unexplained "no ssid" on every probe.
#[cfg(target_os = "linux")]
mod linux {
  use std::process::Command;

  /// `nmcli -t` escapes the field separator and the escape character itself.
  fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
      if c != '\\' {
        out.push(c);
        continue;
      }
      match chars.next() {
        Some(':') => out.push(':'),
        Some('\\') => out.push('\\'),
        // An escape we do not know: keep both characters rather than guessing.
        Some(other) => {
          out.push('\\');
          out.push(other);
        }
        None => out.push('\\'),
      }
    }
    out
  }

  /// The SSID of the active Wi-Fi link in `nmcli -t -f ACTIVE,SSID dev wifi`.
  ///
  /// `--rescan no` keeps a probe that runs every 15 s from starting a scan: the
  /// access point the machine is associated with is always in the cache.
  pub(super) fn ssid_from_nmcli(text: &str) -> Option<String> {
    for line in text.lines() {
      let Some((active, ssid)) = line.split_once(':') else {
        continue;
      };
      if active.trim() != "yes" {
        continue;
      }
      let ssid = unescape(ssid);
      let ssid = ssid.trim();
      // A hidden network is listed as `--` instead of a name.
      if !ssid.is_empty() && ssid != "--" {
        return Some(ssid.to_string());
      }
    }
    None
  }

  /// The SSID in `iw dev <iface> link`, which prints `SSID: name` when the
  /// interface is associated and `Not connected.` when it is not.
  pub(super) fn ssid_from_iw(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
      let name = line.trim().strip_prefix("SSID:")?.trim();
      (!name.is_empty()).then(|| name.to_string())
    })
  }

  /// Is this interface wireless? The same test `srun::link_kind` uses.
  fn is_wireless(iface: &str) -> bool {
    let sys = std::path::Path::new("/sys/class/net").join(iface);
    sys.join("wireless").exists() || sys.join("phy80211").exists()
  }

  fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
      .args(args)
      .output()
      .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
      return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let first = stderr
      .lines()
      .map(str::trim)
      .find(|line| !line.is_empty())
      .unwrap_or("");
    Err(if first.is_empty() {
      format!("{program} exited with {}", out.status)
    } else {
      format!("{program}: {first}")
    })
  }

  pub fn probe_ssid() -> Result<Option<String>, String> {
    match run(
      "nmcli",
      &[
        "--terse",
        "--fields",
        "ACTIVE,SSID",
        "device",
        "wifi",
        "list",
        "--rescan",
        "no",
      ],
    ) {
      Ok(text) => Ok(ssid_from_nmcli(&text)),
      Err(nm) => {
        // No NetworkManager (or no `nmcli` on PATH): read the association
        // straight from the driver instead.
        let Some(iface) = crate::srun::link_iface() else {
          // No route either, so there is nothing to name and nothing to report.
          return Ok(None);
        };
        if !is_wireless(&iface) {
          return Ok(None);
        }
        match run("iw", &["dev", &iface, "link"]) {
          Ok(text) => Ok(ssid_from_iw(&text)),
          Err(iw) => Err(format!("{nm}; {iw}")),
        }
      }
    }
  }
}

#[cfg(target_os = "linux")]
pub use linux::probe_ssid;

#[cfg(not(any(windows, target_os = "linux")))]
pub fn probe_ssid() -> Result<Option<String>, String> {
  Ok(None)
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
    // A lossily decoded OEM-coded error message must not yield a bogus SSID.
    assert_eq!(parse_ssid("缃戠粶澹版槑闇€瑕佷綅缃潈闄? SSID : \n"), None);
  }

  #[test]
  fn enterprise_ssids_are_recognized() {
    // The real SSID is hyphenated; the space form is accepted too.
    assert!(is_no_portal_ssid("Tsinghua-Secure"));
    assert!(is_no_portal_ssid("Tsinghua Secure"));
    assert!(is_no_portal_ssid("  tsinghua_secure "));
    assert!(is_no_portal_ssid("TSINGHUA-SECURE"));
    // Windows appends " 2" when the profile name is already taken, so the same
    // network can be called this on a machine that re-joined it.
    assert!(is_no_portal_ssid("Tsinghua-Secure 2"));
    assert!(is_no_portal_ssid("Tsinghua-Secure 12"));
    assert!(!is_no_portal_ssid("Tsinghua"));
    // A version suffix is part of the name, not a duplicate marker.
    assert!(!is_no_portal_ssid("Tsinghua-Secure-5G"));
    assert!(!is_no_portal_ssid("Tsinghua-IPv4 2"));
    assert!(!is_no_portal_ssid(""));
  }

  #[test]
  fn only_a_matching_ssid_becomes_a_shortcut() {
    // The filter is what turns a read SSID into the 802.1X verdict, so an
    // unrelated network must stay `None` instead of disabling the portal.
    let other = Some("Guest".to_string()).filter(|s| is_no_portal_ssid(s));
    assert_eq!(other, None);
    let secure = Some("Tsinghua-Secure".to_string()).filter(|s| is_no_portal_ssid(s));
    assert_eq!(secure.as_deref(), Some("Tsinghua-Secure"));
  }

  #[test]
  fn campus_ssids_keep_the_portal() {
    // The campus network renames its SSIDs; every one of them still uses the
    // portal, so the probe is allowed to treat a login as meaningful there.
    for ssid in [
      "Tsinghua",
      "Tsinghua-IPv4",
      "Tsinghua-IPv4 2",
      "tsinghua_ipv6",
      "  TSINGHUA  ",
    ] {
      assert!(is_campus_ssid(ssid), "{ssid}");
    }
    // 802.1X wins over the name: that network has no portal to log in to.
    assert!(!is_campus_ssid("Tsinghua-Secure"));
    assert!(!is_campus_ssid("Tsinghua-Secure 2"));
    // Somebody else's network is not a campus one, whatever it looks like.
    for ssid in ["Guest", "eduroam", "THU-Guest", ""] {
      assert!(!is_campus_ssid(ssid), "{ssid}");
    }
  }

  /// `nmcli -t` rows, including the escaping a real SSID can carry.
  #[cfg(target_os = "linux")]
  #[test]
  fn ssid_is_read_from_nmcli() {
    use super::linux::ssid_from_nmcli;

    let sample = "no:eduroam\nyes:Tsinghua-Secure\nno:THU-Guest\n";
    assert_eq!(ssid_from_nmcli(sample).as_deref(), Some("Tsinghua-Secure"));
    // Nothing is associated: every row says `no`.
    assert_eq!(ssid_from_nmcli("no:eduroam\n"), None);
    // No wireless hardware at all: nmcli prints nothing and still succeeds.
    assert_eq!(ssid_from_nmcli(""), None);
    // A hidden network has no name to classify.
    assert_eq!(ssid_from_nmcli("yes:--\n"), None);
    // `-t` escapes the separator, so a name with a colon survives intact.
    assert_eq!(
      ssid_from_nmcli("yes:THU\\:Guest").as_deref(),
      Some("THU:Guest")
    );
    // Windows-style duplicate suffix is still the same network.
    assert!(is_campus_ssid(
      &ssid_from_nmcli("yes:Tsinghua-IPv4 2").unwrap()
    ));
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn ssid_is_read_from_iw() {
    use super::linux::ssid_from_iw;

    let associated =
      "Connected to 00:11:22:33:44:55 (on wlan0)\n\tSSID: Tsinghua-Secure\n\t\n\tfreq: 5180\n";
    assert_eq!(ssid_from_iw(associated).as_deref(), Some("Tsinghua-Secure"));
    assert_eq!(ssid_from_iw("Not connected.\n"), None);
    assert_eq!(ssid_from_iw(""), None);
  }
}
