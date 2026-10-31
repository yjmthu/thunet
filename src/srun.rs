//! SRun3K portal client: challenge → login/logout → status.
//!
//! Endpoints and parameter names follow the campus portal's public HTTP
//! interface (SRun3K):
//!   /cgi-bin/get_challenge, /cgi-bin/srun_portal, /srun_portal_pc,
//!   /cgi-bin/rad_user_info, callback= JSONP wrapper, {MD5}/{SRBX1} prefixes.

use std::io::ErrorKind;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::crypto;

pub const CALLBACK: &str = "C_a_l_l_b_a_c_k";

const UA: &str = "SRun3K Client_1.0.13462.80424";

/// What the machine is currently attached to, decided from external probes
/// only (campus pages answer even when there is nothing to log in to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEnv {
  /// Nothing answered: no cable, no Wi-Fi, no route, DNS broken.
  Unreachable,
  /// The internet answered directly, so there is nothing to authenticate.
  DirectInternet,
  /// A captive portal intercepted us and it is the campus one.
  Campus { ac_id: String },
  /// An interception page that does not look like the campus portal.
  OtherPortal,
}

/// What kept a request from reaching the portal, coarse enough for the window
/// to word it in its own language.
///
/// The operating system's own text (`WSAHOST_NOT_FOUND` and friends) is
/// localized by Windows, not by this app, so it can only ever be a log detail:
/// showing it in the banner is what makes a Chinese sentence appear in an
/// English window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetCause {
  /// The host name did not resolve.
  Resolve,
  /// Nothing accepted the connection.
  Refused,
  /// The request ran out of time.
  Timeout,
  /// There is no route to the host at all.
  Unreachable,
  /// The other side closed the connection mid-answer: a middlebox cutting TLS,
  /// a gateway resetting, a server that hung up. The request did reach
  /// something, so this is not "no network".
  Lost,
  /// Anything else: a portal answer we cannot use, TLS, a protocol error.
  Other,
}

/// A failed portal request: the cause for the user, the detail for the log.
#[derive(Debug, Clone)]
pub struct NetError {
  pub cause: NetCause,
  /// Wording for the log pane, including whatever the OS said.
  pub detail: String,
}

impl NetError {
  /// A failure that is not about reaching the network (a bad portal answer).
  pub fn msg(detail: impl Into<String>) -> Self {
    Self {
      cause: NetCause::Other,
      detail: detail.into(),
    }
  }

  /// The same cause, with context prepended for the log.
  pub fn context(self, prefix: impl std::fmt::Display) -> Self {
    Self {
      cause: self.cause,
      detail: format!("{prefix}: {}", self.detail),
    }
  }

  /// Classify a transport error. The OS text stays in `detail`: it is the only
  /// place the platform's own wording survives, and the log is English.
  fn transport(e: ureq::Error) -> Self {
    let cause = match &e {
      ureq::Error::HostNotFound => NetCause::Resolve,
      ureq::Error::Timeout(_) => NetCause::Timeout,
      ureq::Error::ConnectionFailed => NetCause::Unreachable,
      ureq::Error::Io(io) => match io.kind() {
        ErrorKind::TimedOut => NetCause::Timeout,
        ErrorKind::ConnectionRefused => NetCause::Refused,
        ErrorKind::AddrNotAvailable => NetCause::Unreachable,
        // A connection the other side cut. rustls reports a TLS stream that
        // ended without `close_notify` as `InvalidData` (its own wording, with
        // a docs link) or `UnexpectedEof`, and a reset gateway lands on the
        // same kinds - none of that is worth a banner, so it is one cause.
        ErrorKind::UnexpectedEof
        | ErrorKind::InvalidData
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::BrokenPipe => NetCause::Lost,
        // Windows reports a DNS failure as an "uncategorized" io error, so the
        // Winsock codes have to be read directly.
        _ => match io.raw_os_error() {
          Some(11001 | 11002 | 11003 | 11004) => NetCause::Resolve,
          Some(10060) => NetCause::Timeout,
          Some(10061) => NetCause::Refused,
          // WSAECONNABORTED / WSAECONNRESET.
          Some(10053 | 10054) => NetCause::Lost,
          Some(10049 | 10050 | 10051 | 10065) => NetCause::Unreachable,
          // Android (bionic) hands `getaddrinfo` failures over as an
          // uncategorized error with no errno, so the wording is the only
          // signal left. Without this a blocked background network - which the
          // system reports exactly this way - looked like a rejected login.
          _ if looks_unresolved(&io.to_string()) => NetCause::Resolve,
          _ => NetCause::Other,
        },
      },
      _ => NetCause::Other,
    };
    Self {
      cause,
      detail: e.to_string(),
    }
  }
}

impl std::fmt::Display for NetError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.detail)
  }
}

/// Does this OS error text mean the name could not be resolved?
///
/// Only Android and macOS need it: Linux reports `EAI_AGAIN`/`EAI_NONAME` as
/// plain `io::Error`s too, and bionic gives them no errno at all, so the message
/// is all there is. It is worth the substring test because a blocked background
/// network on a phone arrives exactly this way.
fn looks_unresolved(text: &str) -> bool {
  let text = text.to_ascii_lowercase();
  [
    "lookup address information",
    "no address associated",
    "name or service not known",
    "temporary failure in name resolution",
    "nodename nor servname",
  ]
  .iter()
  .any(|needle| text.contains(needle))
}

#[derive(Debug)]
pub struct Client {
  host: String,
  https: bool,
  agent: ureq::Agent,
  /// The login round-trip is relayed by the portal to the access controller,
  /// and right after a session was ended somewhere else (the web portal,
  /// another client) that round-trip can take many seconds. Sharing the short
  /// status timeout made those attempts fail as "connection timed out" while
  /// the portal was in fact still working on them.
  login_agent: ureq::Agent,
}

/// How long a login (challenge + `srun_portal`) may take, at least.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// What the campus portal answered about this client.
///
/// The answer itself is the evidence: only a link that reaches the campus
/// portal can produce one, and that is the one property every campus path
/// shares - a cable, the campus Wi-Fi, a router or VPN in between, an emulator
/// whose traffic leaves through a campus host - none of which has to carry a
/// campus SSID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalView {
  /// Somebody is logged in on the address the portal sees: the answer's
  /// `error` is `ok`. It says nothing about the link itself - a portal that
  /// answers `not_online` still proves the link reaches the campus network.
  pub logged_in: bool,
  /// The address the portal sees, when its answer names one: this machine's,
  /// or a router's/VPN's in between. The portal names one even when nobody is
  /// logged in (`rad_user_info` reports `client_ip`/`online_ip` either way), so
  /// this is diagnostic rather than a sign of a session.
  pub address: Option<String>,
}

impl Client {
  pub fn new(host: impl Into<String>, https: bool, timeout: Duration) -> Self {
    let agent = |t: Duration| -> ureq::Agent {
      ureq::Agent::config_builder()
        .timeout_global(Some(t))
        .build()
        .into()
    };
    Self {
      host: host.into(),
      https,
      agent: agent(timeout),
      login_agent: agent(timeout.max(LOGIN_TIMEOUT)),
    }
  }

  fn scheme(&self) -> &'static str {
    if self.https { "https" } else { "http" }
  }

  fn get(&self, url: &str) -> Result<String, NetError> {
    self.get_with(&self.agent, url)
  }

  /// The same request with the login budget, for the two calls the portal
  /// forwards to the access controller.
  fn login_get(&self, url: &str) -> Result<String, NetError> {
    self.get_with(&self.login_agent, url)
  }

  fn get_with(&self, agent: &ureq::Agent, url: &str) -> Result<String, NetError> {
    agent
      .get(url)
      .header("User-Agent", UA)
      .call()
      .map_err(NetError::transport)?
      .body_mut()
      .read_to_string()
      .map_err(NetError::transport)
  }

  /// Cheap reachability probe with a short timeout so the worker thread is not
  /// blocked for long when the machine has no network at all. It runs on a timer
  /// even when nobody asked to log in (it decides whether *Connect* is usable),
  /// so the timeout is deliberately shorter than a normal request.
  pub fn net_env(&self) -> NetEnv {
    const PROBES: [&str; 3] = [
      "http://connect.rom.miui.com/generate_204",
      "http://www.msftconnecttest.com/connecttest.txt",
      "http://www.baidu.com/",
    ];
    let agent: ureq::Agent = ureq::Agent::config_builder()
      .timeout_global(Some(Duration::from_secs(2)))
      .build()
      .into();
    for url in PROBES {
      match agent.get(url).header("User-Agent", UA).call() {
        Ok(mut resp) => {
          let body = resp.body_mut().read_to_string().unwrap_or_default();
          if let Some(ac_id) = scan_ac_id(&body) {
            return NetEnv::Campus { ac_id };
          }
          return if looks_like_portal(&body) {
            NetEnv::OtherPortal
          } else {
            NetEnv::DirectInternet
          };
        }
        // A status-code error still means a server answered: the network works.
        Err(ureq::Error::StatusCode(_)) => return NetEnv::DirectInternet,
        Err(_) => continue,
      }
    }
    NetEnv::Unreachable
  }

  fn url(&self, path: &str, params: &[(&str, &str)]) -> String {
    let mut s = format!("{}://{}{}?", self.scheme(), self.host, path);
    for (k, v) in params {
      s.push_str(&format!("{}={}&", enc(k), enc(v)));
    }
    s.push_str("callback=");
    s.push_str(CALLBACK);
    s
  }

  /// Strip the JSONP wrapper and parse.
  fn jsonp(body: &str) -> Result<Value, NetError> {
    let t = body.trim();
    let inner = t
      .strip_prefix(CALLBACK)
      .and_then(|r| r.strip_prefix('('))
      .and_then(|r| r.strip_suffix(')'))
      .unwrap_or(t);
    serde_json::from_str(inner).map_err(|e| NetError::msg(format!("bad JSON: {e}: {inner}")))
  }

  pub fn challenge(&self, username: &str, ip: &str) -> Result<String, NetError> {
    let url = self.url(
      "/cgi-bin/get_challenge",
      &[("username", username), ("ip", ip), ("double_stack", "1")],
    );
    // Naming the step keeps a timeout in the log diagnosable: the portal
    // answers some of its endpoints while another one is stuck, and "the
    // request failed" alone cannot tell them apart.
    let body = self
      .login_get(&url)
      .map_err(|e| e.context("get_challenge"))?;
    let v = Self::jsonp(&body)?;
    if v.get("res").and_then(Value::as_str) != Some("ok") {
      return Err(NetError::msg(format!("challenge rejected: {v}")));
    }
    v.get("challenge")
      .and_then(Value::as_str)
      .map(str::to_owned)
      .ok_or_else(|| NetError::msg(format!("no challenge field: {v}")))
  }

  /// Detect the AC id the campus gateway assigned to this client.
  ///
  /// While unauthenticated the gateway intercepts external HTTP requests and
  /// answers with an HTML stub pointing at `<auth4>/index_<ac_id>.html`, so
  /// that page name is the authoritative source. `auth4/` and
  /// `login.tsinghua.edu.cn/` only serve the default `index_1.html` page,
  /// which is why using `ac_id=1` makes the portal answer
  /// `E2808 Control policy not found`.
  /// The `bool` is `true` when the id came from a captive interception page
  /// (authoritative) and `false` when it came from a default campus page.
  pub fn detect_ac_id(&self) -> Result<(String, bool), NetError> {
    // External targets can only answer through the gateway, so a hit here
    // means we were intercepted and the id is the real AC.
    const PROBES: [&str; 3] = [
      "http://connect.rom.miui.com/generate_204",
      "http://www.baidu.com/",
      "http://www.msftconnecttest.com/connecttest.txt",
    ];
    for url in PROBES {
      if let Ok(body) = self.get(url) {
        if let Some(id) = scan_ac_id(&body) {
          return Ok((id, true));
        }
      }
    }
    // Fallback: campus pages always answer, but while the client is online
    // they hardcode the default AC (1), which is not the real one.
    for url in [
      "http://login.tsinghua.edu.cn/",
      "http://login.tsinghua.edu.cn/index_1.html",
    ] {
      if let Ok(body) = self.get(url) {
        if let Some(id) = scan_ac_id(&body) {
          return Ok((id, false));
        }
      }
    }
    Err(NetError::msg("ac_id not found (portal not intercepting?)"))
  }

  /// True when `/cgi-bin/rad_user_info` reports an active session.
  pub fn is_online(&self, ac_id: &str) -> bool {
    self
      .status(ac_id)
      .ok()
      .and_then(|v| v.get("online").and_then(|b| b.as_bool()))
      .unwrap_or(false)
  }

  /// Poll `rad_user_info` until the session shows up.
  ///
  /// THU's accounting row appears ~20 s after an accepted login, so a single
  /// immediate check is not enough.
  pub fn wait_online(&self, ac_id: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
      if self.is_online(ac_id) {
        return true;
      }
      if start.elapsed() >= timeout {
        return false;
      }
      std::thread::sleep(Duration::from_secs(1));
    }
  }

  pub fn status(&self, ac_id: &str) -> Result<Value, NetError> {
    let page = self.get(&format!(
      "{}://{}/srun_portal_pc?ac_id={}",
      self.scheme(),
      self.host,
      enc(ac_id)
    ))?;
    let ip = scrape_ip(&page).ok_or_else(|| NetError::msg("public ip not found on portal page"))?;
    let url = self.url("/cgi-bin/rad_user_info", &[("ip", &ip)]);
    let mut v = Self::jsonp(&self.get(&url)?)?;
    let online = v.get("error").and_then(Value::as_str) == Some("ok");
    if let Some(obj) = v.as_object_mut() {
      // The portal nests the per-device list in a JSON *string*; hand it on as
      // the object it describes. See [`expanded_device_detail`].
      if let Some(devices) = obj
        .get("online_device_detail")
        .and_then(expanded_device_detail)
      {
        obj.insert("online_device_detail".into(), devices);
      }
      obj.insert("ip".into(), json!(ip));
      obj.insert("online".into(), json!(online));
    }
    Ok(v)
  }

  pub fn login_logout(
    &self,
    username: &str,
    password: Option<&str>,
    ip: &str,
    ac_id: &str,
    logout: bool,
  ) -> Result<Value, NetError> {
    let token = self.challenge(username, ip)?;
    let params = build_params(
      username,
      password,
      &token,
      ip,
      ac_id,
      logout,
      Some(portal_os_name()),
      portal_device_class(),
    );
    let refs: Vec<(&str, &str)> = params
      .iter()
      .map(|(k, v)| (k.as_str(), v.as_str()))
      .collect();
    let url = self.url("/cgi-bin/srun_portal", &refs);
    let step = if logout {
      "srun_portal logout"
    } else {
      "srun_portal login"
    };
    let body = self.login_get(&url).map_err(|e| e.context(step))?;
    Self::jsonp(&body)
  }

  /// Ask the campus portal about this client. `None` when it does not answer,
  /// or answers with something that is not its own JSON.
  ///
  /// This is the request that tells a campus path apart from a home connection
  /// without looking at the Wi-Fi name: a portal answer means the campus network
  /// is reachable, whether a session exists or not, and whatever NAT, VPN or
  /// router sits in between.
  pub fn portal_view(&self) -> Option<PortalView> {
    let url = self.url("/cgi-bin/rad_user_info", &[]);
    let body = self.get(&url).ok()?;
    Self::portal_view_of(&body)
  }

  /// Parse one `rad_user_info` answer.
  ///
  /// The portal names the outcome in `error`; there is no separate online flag
  /// on the wire (`status()` derives one, this does not need it).
  fn portal_view_of(body: &str) -> Option<PortalView> {
    let v = Self::jsonp(body).ok()?;
    let error = v.get("error").and_then(Value::as_str)?;
    // The portal speaks two shapes: the older answers carry `client_ip`, the
    // current one reports the address as `online_ip` (and has no `client_ip`
    // at all).
    let address = v
      .get("client_ip")
      .or_else(|| v.get("online_ip"))
      .and_then(Value::as_str)
      .map(str::trim)
      .filter(|ip| !ip.is_empty())
      .map(str::to_owned);
    Some(PortalView {
      logged_in: error == "ok",
      address,
    })
  }

  /// Account name and address of the live session on `ip` (empty = this host).
  ///
  /// `logout` signs the request with the account that owns the session, which
  /// need not be the one saved in the config (shared machine, or a session
  /// started by another client). `get_challenge` also rejects an empty
  /// `username` with `missing_required_parameters_error`, so this is how a
  /// client without stored credentials finds out who to log out.
  pub fn online_identity(&self, ip: &str) -> Option<(String, String)> {
    let url = self.url("/cgi-bin/rad_user_info", &[("ip", ip)]);
    let body = self.get(&url).ok()?;
    let v = Self::jsonp(&body).ok()?;
    if v.get("error").and_then(Value::as_str) != Some("ok") {
      return None;
    }
    let user = v.get("user_name").and_then(Value::as_str)?;
    if user.is_empty() {
      return None;
    }
    let addr = v
      .get("online_ip")
      .and_then(Value::as_str)
      .unwrap_or(ip)
      .to_string();
    Some((user.to_string(), addr))
  }
}

/// Portal codes meaning "this account/password pair is wrong".
///
/// Retrying cannot fix them, and the JSON that carries them is far too long for
/// a toast, so the UI shows a one-line message and keeps the detail in the log.
/// E2901 is what THU answers for a bad password (`ldap_first_entry error`);
/// E2531/E2532/E2553 are the generic SRun3K variants.
pub fn is_credentials_error(code: &str) -> bool {
  matches!(code, "E2531" | "E2532" | "E2553" | "E2901")
}

/// Portal answers meaning "this login cannot be handed to the access
/// controller that `ac_id` names".
///
/// `E2808 Control policy not found` is a policy for an AC this client is not on,
/// and `INFO failed, BAS respond timeout` is that AC not answering at all. On
/// THU both happen when the id belongs to another link: the campus hands out a
/// different one per access network, and switching between them (cable, Wi-Fi,
/// 802.1X) leaves a stale value behind. Retrying with the same id cannot work.
pub fn is_ac_unreachable(code: &str, message: &str) -> bool {
  if code == "E2808" {
    return true;
  }
  message.to_ascii_lowercase().contains("bas respond timeout")
}

/// Percent-encode for a query string (RFC 3986 unreserved kept).
fn enc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for b in s.bytes() {
    match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
      _ => out.push_str(&format!("%{b:02X}")),
    }
  }
  out
}

/// What the portal expects in its own `os_name` field, derived from the system
/// this build runs on rather than from a setting: the value describes the
/// machine, so a user-editable copy could only ever be wrong.
pub fn portal_os_name() -> &'static str {
  match std::env::consts::OS {
    "linux" => "Linux",
    "windows" => "Windows",
    "macos" => "MacOS",
    "android" => "AndroidOS",
    "ios" => "iOS",
    "freebsd" => "FreeBSD",
    other => other,
  }
}

/// What the portal records as `class_name` for this session.
///
/// The field is the machine's **chassis type**, so it is named the way the
/// firmware names it ("Mini PC", "Laptop", "Tablet"). `None` means the platform
/// does not expose one (macOS has no SMBIOS enclosure data at all) or the
/// firmware answered "other" / "unknown" with nothing to fall back on: saying
/// nothing is better than inventing "Desktop", because the portal derives its
/// own class from the system we declare.
pub fn portal_device_class() -> Option<&'static str> {
  device_class()
}

/// SMBIOS chassis type to the name the firmware uses for it.
///
/// `battery` is evidence, not a default: a machine with a battery is portable
/// even when the firmware stays silent. Without that evidence the answer is
/// `None` - a desktop is never assumed from a missing answer.
fn class_from_chassis(chassis: Option<u32>, battery: bool) -> Option<&'static str> {
  match chassis {
    Some(3) => Some("Desktop"),
    Some(4) => Some("Low Profile Desktop"),
    Some(5) => Some("Pizza Box"),
    Some(6) => Some("Mini Tower"),
    Some(7) => Some("Tower"),
    Some(8) => Some("Portable"),
    Some(9) => Some("Laptop"),
    Some(10) => Some("Notebook"),
    Some(11) => Some("Hand Held"),
    Some(12) => Some("Docking Station"),
    Some(13) => Some("All in One"),
    Some(14) => Some("Sub Notebook"),
    Some(15) => Some("Space-saving"),
    Some(16) => Some("Lunch Box"),
    Some(17) => Some("Main Server Chassis"),
    Some(23) => Some("Rack Mount Chassis"),
    Some(24) => Some("Sealed-case PC"),
    Some(30) => Some("Tablet"),
    Some(31) => Some("Convertible"),
    Some(32) => Some("Detachable"),
    Some(35) => Some("Mini PC"),
    Some(36) => Some("Stick PC"),
    _ if battery => Some("Laptop"),
    _ => None,
  }
}

#[cfg(target_os = "linux")]
fn device_class() -> Option<&'static str> {
  let chassis = std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
    .ok()
    .and_then(|raw| raw.trim().parse().ok());
  let battery = std::fs::read_dir("/sys/class/power_supply")
    .map(|entries| {
      entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().starts_with("BAT")
          || std::fs::read_to_string(entry.path().join("type"))
            .is_ok_and(|kind| kind.trim() == "Battery")
      })
    })
    .unwrap_or(false);
  class_from_chassis(chassis, battery)
}

#[cfg(target_os = "windows")]
fn device_class() -> Option<&'static str> {
  use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
  // `GetSystemFirmwareTable` gives the raw SMBIOS table, the same source the
  // Linux sysfs file comes from; the battery is the fallback for firmware that
  // reports nothing usable. 128 means "no system battery".
  let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
  let battery = unsafe { GetSystemPowerStatus(&mut status) } != 0 && status.BatteryFlag != 128;
  class_from_chassis(smbios_chassis(), battery)
}

/// The System Enclosure (type 3) chassis byte from the firmware's SMBIOS table.
#[cfg(target_os = "windows")]
fn smbios_chassis() -> Option<u32> {
  use windows_sys::Win32::System::SystemInformation::GetSystemFirmwareTable;
  const RSMB: u32 = u32::from_be_bytes(*b"RSMB");

  // A null buffer asks for the size only.
  let size = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
  if size < 8 {
    return None;
  }
  let mut buffer = vec![0u8; size as usize];
  let got =
    unsafe { GetSystemFirmwareTable(RSMB, 0, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
  if got == 0 || got > size {
    return None;
  }
  buffer.truncate(got as usize);
  // `RawSMBIOSData`: 4 bytes of version, then a DWORD length, then the table.
  let length = u32::from_le_bytes(buffer.get(4..8)?.try_into().ok()?) as usize;
  let end = 8usize.checked_add(length)?.min(buffer.len());
  chassis_from_smbios(buffer.get(8..end)?)
}

/// Walk an SMBIOS structure table and return the chassis type of the System
/// Enclosure structure (type 3), which is the same 1-36 enum as the Linux file.
///
/// `table` starts at the first structure, i.e. after the 8-byte header.
#[cfg(any(target_os = "windows", test))]
fn chassis_from_smbios(table: &[u8]) -> Option<u32> {
  let mut i = 0;
  while i + 4 <= table.len() {
    let kind = table[i];
    let len = table[i + 1] as usize;
    if len < 4 || i + len > table.len() {
      return None;
    }
    if kind == 3 && len > 5 {
      // Byte 5: chassis type in bits 0-6, lock bit in bit 7.
      return Some(u32::from(table[i + 5] & 0x7F));
    }
    // The formatted area is followed by the string set, which ends at a double
    // NUL; the next structure starts right after it.
    let mut j = i + len;
    while j + 1 < table.len() && !(table[j] == 0 && table[j + 1] == 0) {
      j += 1;
    }
    i = j + 2;
  }
  None
}

#[cfg(target_os = "macos")]
fn device_class() -> Option<&'static str> {
  // macOS has no SMBIOS enclosure data, but it does not need one: every Mac
  // laptop has a battery, so no battery means a desktop body, and the model
  // identifier says which desktop.
  if mac_has_battery() {
    return Some("Laptop");
  }
  Some(mac_class_from_model(&mac_model().unwrap_or_default()))
}

/// Model identifier of this Mac (`hw.model`, e.g. `Macmini9,1`).
#[cfg(target_os = "macos")]
fn mac_model() -> Option<String> {
  unsafe extern "C" {
    fn sysctlbyname(
      name: *const core::ffi::c_char,
      oldp: *mut core::ffi::c_void,
      oldlenp: *mut usize,
      newp: *mut core::ffi::c_void,
      newlen: usize,
    ) -> core::ffi::c_int;
  }

  let name = c"hw.model";
  let mut size = 0usize;
  // SAFETY: `sysctlbyname` with a null buffer only reports the length.
  unsafe {
    if sysctlbyname(
      name.as_ptr(),
      std::ptr::null_mut(),
      &mut size,
      std::ptr::null_mut(),
      0,
    ) != 0
      || size == 0
    {
      return None;
    }
  }
  let mut buffer = vec![0u8; size];
  // SAFETY: the buffer is `size` bytes, which is what the first call asked for.
  unsafe {
    if sysctlbyname(
      name.as_ptr(),
      buffer.as_mut_ptr().cast(),
      &mut size,
      std::ptr::null_mut(),
      0,
    ) != 0
    {
      return None;
    }
  }
  buffer.truncate(size.saturating_sub(1));
  String::from_utf8(buffer).ok()
}

/// Whether this Mac has a battery at all.
#[cfg(target_os = "macos")]
fn mac_has_battery() -> bool {
  std::process::Command::new("/usr/bin/pmset")
    .args(["-g", "batt"])
    .output()
    .map(|out| String::from_utf8_lossy(&out.stdout).contains("InternalBattery"))
    .unwrap_or(false)
}

/// What a Mac body is called, from `hw.model`, when it has no battery. The
/// names follow the SMBIOS ones used for the other platforms.
#[cfg(any(target_os = "macos", test))]
fn mac_class_from_model(model: &str) -> &'static str {
  let m = model.to_ascii_lowercase();
  if m.contains("macbook") {
    "Laptop"
  } else if m.contains("macmini") {
    "Mini PC"
  } else if m.contains("imac") {
    "All in One"
  } else if m.contains("macpro") {
    "Tower"
  } else if m.contains("xserve") {
    "Rack Mount Chassis"
  } else {
    // Mac Studio, or a virtual machine: a desktop body without a more specific
    // name.
    "Desktop"
  }
}

#[cfg(target_os = "android")]
fn device_class() -> Option<&'static str> {
  // Android has no SMBIOS chassis to name, so this is the portal's own category
  // for phones and tablets — the same class the official client is recorded
  // under. Sending nothing (the old behaviour) left the device list's type
  // column empty, because the portal stores `name` as the class.
  Some("Smartphones/PDAs/Tablets")
}

#[cfg(not(any(
  target_os = "linux",
  target_os = "windows",
  target_os = "macos",
  target_os = "android"
)))]
fn device_class() -> Option<&'static str> {
  None
}

/// Build the full login/logout parameter set (order is irrelevant).
pub fn build_params(
  username: &str,
  password: Option<&str>,
  token: &str,
  ip: &str,
  ac_id: &str,
  logout: bool,
  os: Option<&str>,
  device: Option<&str>,
) -> Vec<(String, String)> {
  let hmd5 = password.map(crypto::md5_hex).unwrap_or_default();

  let mut info = serde_json::Map::new();
  info.insert("username".into(), json!(username));
  info.insert("ip".into(), json!(ip));
  info.insert("acid".into(), json!(ac_id));
  info.insert("enc_ver".into(), json!("srun_bx1"));
  if !logout {
    info.insert("password".into(), json!(password.unwrap_or("")));
  }
  let info_json = serde_json::to_string(&info).expect("serializable");
  let info_field = format!(
    "{{SRBX1}}{}",
    crypto::quirk_b64(&crypto::xencode(info_json.as_bytes(), token.as_bytes()))
  );

  let n = "200";
  let ty = "1";
  let chk_src = if logout {
    format!("{token}{username}{token}{ac_id}{token}{ip}{token}{n}{token}{ty}{token}{info_field}")
  } else {
    format!(
      "{token}{username}{token}{hmd5}{token}{ac_id}{token}{ip}{token}{n}{token}{ty}{token}{info_field}"
    )
  };

  let mut p: Vec<(String, String)> = vec![
    (
      "action".into(),
      if logout { "logout" } else { "login" }.into(),
    ),
    ("username".into(), username.into()),
    ("ac_id".into(), ac_id.into()),
    ("n".into(), n.into()),
    ("type".into(), ty.into()),
    ("ip".into(), ip.into()),
    ("double_stack".into(), "1".into()),
    ("info".into(), info_field),
  ];
  if !logout {
    p.push(("password".into(), format!("{{MD5}}{hmd5}")));
    // What the portal records as `os_name` and `class_name` for this session.
    // Neither is part of `chksum`, so a portal that does not know a key simply
    // ignores it. `name` carries a category ("Desktop"), never a machine name:
    // the portal treats that field as the class, and a free-form label there
    // replaced the category it derives from the system.
    if let Some(os) = os.filter(|v| !v.is_empty()) {
      p.push(("os".into(), os.into()));
    }
    if let Some(device) = device.filter(|v| !v.is_empty()) {
      p.push(("name".into(), device.into()));
    }
  }
  p.push(("chksum".into(), crypto::sha1_hex(&chk_src)));
  p
}

/// A captive portal answers with a tiny HTML/JS stub instead of the real page.
fn looks_like_portal(body: &str) -> bool {
  let b = body.trim();
  if b.is_empty() || b.len() > 4096 {
    return false;
  }
  let lower = b.to_ascii_lowercase();
  lower.contains("location.href")
    || lower.contains("location.replace")
    || lower.contains("http-equiv=\"refresh\"")
    || lower.contains("http-equiv='refresh'")
}

/// Identity of the path this machine currently uses to reach the internet.
///
/// Connecting a UDP socket only performs a route lookup - no packet is sent -
/// so this is essentially free. The local address the kernel picks names the
/// interface and the lease behind it, which is exactly what changes when a
/// cable is pulled, a Wi-Fi network is joined or DHCP hands out another
/// address. `None` means there is no route at all: the cable is out, or no
/// address has been assigned yet.
pub fn link_id() -> Option<std::net::IpAddr> {
  let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
  socket.connect("223.5.5.5:53").ok()?;
  socket.local_addr().ok().map(|a| a.ip())
}

/// Cheap "is there a route to the internet at all?" check.
///
/// Used to start a login attempt the moment the network comes up instead of
/// waiting for a poll interval.
pub fn link_up() -> bool {
  link_id().is_some()
}

/// The IPv6 counterpart of [`link_id`].
///
/// The same route-lookup trick, aimed at a public v6 host: the address the
/// kernel would send from is the one this machine holds on the link. `None`
/// when there is no v6 route at all (an IPv4-only network).
pub fn link_id6() -> Option<std::net::Ipv6Addr> {
  let socket = std::net::UdpSocket::bind("[::]:0").ok()?;
  socket.connect("[2400:3200::1]:53").ok()?;
  match socket.local_addr().ok()?.ip() {
    std::net::IpAddr::V6(addr) => Some(addr),
    std::net::IpAddr::V4(_) => None,
  }
}

/// Which kind of link carries that route: a cable, Wi-Fi, or neither.
///
/// The window shows it next to the account, because a laptop can have both a
/// cable and a Wi-Fi association up and the portal session belongs to exactly
/// one of them - a session that appears to come and go is usually two links
/// taking turns.
pub fn link_kind() -> crate::proto::LinkKind {
  link_imp::link_kind()
}

/// The default-route interface name in `/proc/net/route`.
///
/// The table is hex and little-endian; a default route has a zero destination
/// and the `UP` (`0x1`) flag, and the kernel uses the lowest metric among
/// them. Kept free of `cfg` so the parsing itself is testable on any host.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn default_route(table: &str) -> Option<String> {
  let mut best: Option<(u32, String)> = None;
  for line in table.lines().skip(1) {
    let mut cols = line.split_whitespace();
    let Some(iface) = cols.next() else { continue };
    let (Some(destination), Some(_gateway), Some(flags)) = (cols.next(), cols.next(), cols.next())
    else {
      continue;
    };
    let metric = cols.nth(2).and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    let Ok(flags) = u32::from_str_radix(flags, 16) else {
      continue;
    };
    // Destination 0.0.0.0 with RTF_UP: the route that catches everything.
    if destination != "00000000" || flags & 0x1 == 0 {
      continue;
    }
    if best.as_ref().is_none_or(|(m, _)| metric < *m) {
      best = Some((metric, iface.to_string()));
    }
  }
  best.map(|(_, iface)| iface)
}

#[cfg(windows)]
mod link_imp {
  use crate::proto::LinkKind;
  use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    GetBestInterfaceEx, IP_ADAPTER_ADDRESSES_LH,
  };
  use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_UNSPEC, IN_ADDR, IN_ADDR_0, SOCKADDR, SOCKADDR_IN,
  };

  /// `IF_TYPE_ETHERNET_CSMACD` and `IF_TYPE_IEEE80211` from `ipifcons.h`.
  const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
  const IF_TYPE_IEEE80211: u32 = 71;
  /// `ERROR_BUFFER_OVERFLOW`: the list did not fit, `size` now says how much it
  /// needs.
  const ERROR_BUFFER_OVERFLOW: u32 = 111;
  /// A sanity cap, so a broken `size` answer cannot spin here.
  const MAX_LIST_BYTES: u32 = 4 * 1024 * 1024;

  pub fn link_kind() -> LinkKind {
    match best_index().and_then(adapter_info) {
      // A phone sharing its connection over USB is an ordinary Ethernet
      // adapter to Windows; only its driver description says otherwise.
      Some((IF_TYPE_ETHERNET_CSMACD, true)) => LinkKind::Usb,
      Some((IF_TYPE_ETHERNET_CSMACD, false)) => LinkKind::Ethernet,
      Some((IF_TYPE_IEEE80211, _)) => LinkKind::Wlan,
      // A route that exists but belongs to some other adapter (a tunnel, a
      // virtual switch) still is not "no link".
      Some(_) => LinkKind::Other,
      None if crate::srun::link_up() => LinkKind::Other,
      None => LinkKind::None,
    }
  }

  /// The index of the interface the routing table would send to `223.5.5.5`,
  /// which is the same destination `link_id()` asks the kernel about.
  fn best_index() -> Option<u32> {
    let target = SOCKADDR_IN {
      sin_family: AF_INET,
      sin_port: 53u16.to_be(),
      // Network order in memory, so the four octets land in the intended order.
      sin_addr: IN_ADDR {
        S_un: IN_ADDR_0 {
          S_addr: u32::from_ne_bytes([223, 5, 5, 5]),
        },
      },
      sin_zero: [0; 8],
    };
    let mut index = 0u32;
    let rc =
      unsafe { GetBestInterfaceEx(&target as *const SOCKADDR_IN as *const SOCKADDR, &mut index) };
    (rc == 0 && index != 0).then_some(index)
  }

  /// `IfType` of the adapter with that index and whether it is a phone on the
  /// end of a USB cable, or `None` when the list cannot be read (which is a
  /// different case from "not found" only for the log).
  fn adapter_info(index: u32) -> Option<(u32, bool)> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    loop {
      let mut buf = vec![0u8; size as usize];
      let rc = unsafe {
        GetAdaptersAddresses(
          AF_UNSPEC as u32,
          flags,
          std::ptr::null(),
          buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
          &mut size,
        )
      };
      if rc == ERROR_BUFFER_OVERFLOW && size <= MAX_LIST_BYTES {
        continue;
      }
      if rc != 0 {
        return None;
      }
      let mut entry = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
      while !entry.is_null() {
        let adapter = unsafe { &*entry };
        let this = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
        if this == index {
          return Some((adapter.IfType, is_tethered(&description(adapter))));
        }
        entry = adapter.Next;
      }
      return None;
    }
  }

  /// The driver's own name for the adapter, as a NUL-terminated wide string.
  fn description(adapter: &IP_ADAPTER_ADDRESSES_LH) -> String {
    let ptr = adapter.Description;
    if ptr.is_null() {
      return String::new();
    }
    let mut len = 0usize;
    unsafe {
      while *ptr.add(len) != 0 {
        len += 1;
      }
      String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
  }

  /// Names the tethering drivers use: RNDIS/NDIS generally, and the phone
  /// vendors' own Ethernet-over-USB entries. A plain USB Ethernet dongle is
  /// deliberately not matched - it is not a phone.
  fn is_tethered(description: &str) -> bool {
    let name = description.to_ascii_lowercase();
    [
      "ndis",
      "tethering",
      "iphone",
      "android",
      "apple mobile device",
    ]
    .iter()
    .any(|key| name.contains(key))
  }
}

#[cfg(target_os = "linux")]
mod link_imp {
  use crate::proto::LinkKind;

  pub fn link_kind() -> LinkKind {
    let Ok(table) = std::fs::read_to_string("/proc/net/route") else {
      return LinkKind::None;
    };
    let Some(iface) = super::default_route(&table) else {
      return LinkKind::None;
    };
    let sys = std::path::Path::new("/sys/class/net").join(&iface);
    if sys.join("wireless").exists() || sys.join("phy80211").exists() {
      return LinkKind::Wlan;
    }
    match std::fs::read_to_string(sys.join("type")) {
      // ARPHRD_ETHER is what a wired NIC reports; a tunnel reports 65534. A
      // phone sharing its connection over USB shows up as an ordinary
      // Ethernet adapter, and only sysfs knows that the adapter hangs off a
      // USB bus.
      Ok(kind) if kind.trim() == "1" => {
        if usb_backed(&sys) {
          LinkKind::Usb
        } else {
          LinkKind::Ethernet
        }
      }
      Ok(_) => LinkKind::Other,
      Err(_) => LinkKind::Other,
    }
  }

  /// Is this interface attached over USB?
  ///
  /// `device` points at the parent device in sysfs; a USB function's path runs
  /// through a `usbN` hub (`.../usb1/1-2/1-2:1.0`), while an onboard NIC stops
  /// at its PCI function.
  fn usb_backed(sys: &std::path::Path) -> bool {
    std::fs::read_link(sys.join("device"))
      .map(|path| usb_target(&path))
      .unwrap_or(false)
  }

  /// The rule itself, without the file system: a parent under a USB hub.
  fn usb_target(path: &std::path::Path) -> bool {
    path.to_string_lossy().contains("/usb")
  }

  #[cfg(test)]
  mod tests {
    use super::usb_target;
    use std::path::Path;

    #[test]
    fn a_device_path_through_usb_is_tethered() {
      assert!(usb_target(Path::new(
        "../../devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0"
      )));
      assert!(!usb_target(Path::new(
        "../../devices/pci0000:00/0000:00:1f.6"
      )));
    }
  }
}

/// Android: a `NativeActivity` cannot reach `ConnectivityManager` without JNI,
/// so the Activity answers the question and writes the transport behind the
/// default network to `files/link.txt`. Reading a five-byte file is cheaper
/// than a JNI call and keeps the core free of a Java dependency.
#[cfg(target_os = "android")]
mod link_imp {
  use crate::proto::LinkKind;

  pub fn link_kind() -> LinkKind {
    let path = crate::config::dir().join("link.txt");
    match std::fs::read_to_string(path) {
      Ok(text) => match text.trim() {
        "wlan" => LinkKind::Wlan,
        "ethernet" => LinkKind::Ethernet,
        "mobile" => LinkKind::Mobile,
        "usb" => LinkKind::Usb,
        "other" => LinkKind::Other,
        "none" => LinkKind::None,
        // A word this build does not know: fall through to the route check
        // rather than claim there is no link.
        _ => fallback(),
      },
      // Before the first write (the Activity registers its callback while the
      // core is already probing) the route table still answers.
      Err(_) => fallback(),
    }
  }

  fn fallback() -> LinkKind {
    if crate::srun::link_up() {
      LinkKind::Other
    } else {
      LinkKind::None
    }
  }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
mod link_imp {
  use crate::proto::LinkKind;

  pub fn link_kind() -> LinkKind {
    if crate::srun::link_up() {
      LinkKind::Other
    } else {
      LinkKind::None
    }
  }
}

fn scan_ac_id(page: &str) -> Option<String> {
  for pat in ["index_", "ac_id="] {
    let mut from = 0;
    while let Some(pos) = page[from..].find(pat) {
      let start = from + pos + pat.len();
      let digits: String = page[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
      if !digits.is_empty() {
        return Some(digits);
      }
      from = from + pos + 1;
    }
  }
  None
}

fn scrape_ip(page: &str) -> Option<String> {
  let b = page.as_bytes();
  let mut i = 0;
  while i + 2 <= b.len() {
    if b[i] == b'i' && b[i + 1] == b'p' && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
      let mut j = i + 2;
      while j < b.len() && b[j].is_ascii_whitespace() {
        j += 1;
      }
      if j < b.len() && b[j] == b':' {
        j += 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
          j += 1;
        }
        if j < b.len() && b[j] == b'"' {
          j += 1;
          let start = j;
          while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
            j += 1;
          }
          if j > start {
            return Some(page[start..j].to_string());
          }
        }
      }
    }
    i += 1;
  }
  None
}

/// The per-device list the portal nests inside a JSON *string*.
///
/// `rad_user_info` answers `"online_device_detail": "{\"<id>\":{…},…}"`, so a
/// reader would otherwise get one long escaped string. This returns the object
/// it describes; anything that is not a JSON object yields `None`, so the
/// caller keeps exactly what the portal sent. The field is informational, and
/// a format change there must not break the rest of the answer.
fn expanded_device_detail(value: &Value) -> Option<Value> {
  value
    .as_str()
    .and_then(|text| serde_json::from_str::<Value>(text).ok())
    .filter(Value::is_object)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_lookup_failure_without_an_errno_is_a_resolve_failure() {
    // What bionic says when the system blocks this app's DNS, and what glibc
    // says for the same thing on a desktop.
    assert!(looks_unresolved(
      "failed to lookup address information: No address associated with hostname"
    ));
    assert!(looks_unresolved("Temporary failure in name resolution"));
    assert!(!looks_unresolved("Connection reset by peer"));
  }

  #[test]
  fn ac_id_scrape() {
    // captive interception page -> real AC id
    assert_eq!(
      scan_ac_id(r#"<script>location.href="http://auth4.tsinghua.edu.cn/index_169.html"</script>"#)
        .as_deref(),
      Some("169")
    );
    // portal landing page -> query parameter form
    assert_eq!(
      scan_ac_id(
        r#"<meta http-equiv="refresh" content="0;url=/srun_portal_pc?ac_id=1&amp;theme=pro">"#
      )
      .as_deref(),
      Some("1")
    );
    assert_eq!(scan_ac_id("no hint here"), None);
  }

  #[test]
  fn portal_stub_detection() {
    // The campus gateway stub and a generic captive portal both look like this.
    assert!(looks_like_portal(
      r#"<script>location.href="http://auth4.tsinghua.edu.cn/index_169.html"</script>"#
    ));
    assert!(looks_like_portal(
      r#"<html><head><meta http-equiv="refresh" content="0;url=/login"></head></html>"#
    ));
    assert!(looks_like_portal(
      "<script>location.replace('http://portal.example/')</script>"
    ));
    // Normal probe answers must NOT be mistaken for portals.
    assert!(!looks_like_portal(""));
    assert!(!looks_like_portal("Microsoft Connect Test"));
    assert!(!looks_like_portal(&"<html>".repeat(2000)));
  }

  #[test]
  fn ip_scrape() {
    // 192.0.2.0/24 is reserved for documentation (RFC 5737).
    assert_eq!(
      scrape_ip("        ip     : \"192.0.2.10\",\n").as_deref(),
      Some("192.0.2.10")
    );
    assert_eq!(scrape_ip("ip : \"1.2.3.4\",").as_deref(), Some("1.2.3.4"));
    assert_eq!(scrape_ip("ipv6 : \"::1\""), None);
  }

  #[test]
  fn chksum_shape() {
    let p = build_params(
      "u",
      Some("p"),
      &"0".repeat(64),
      "1.2.3.4",
      "1",
      false,
      None,
      None,
    );
    let get = |k: &str| p.iter().find(|(n, _)| n == k).unwrap().1.clone();
    assert_eq!(get("password"), format!("{{MD5}}{}", crypto::md5_hex("p")));
    assert!(get("info").starts_with("{SRBX1}"));
    assert_eq!(get("chksum").len(), 40);
  }

  /// The system and the class are declared, never signed, and the class is a
  /// category the portal already knows rather than this machine's name.
  #[test]
  fn the_system_and_the_class_are_declared_but_unsigned() {
    let get =
      |p: &[(String, String)], k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let plain = build_params(
      "u",
      Some("p"),
      &"0".repeat(64),
      "192.0.2.10",
      "169",
      false,
      None,
      None,
    );
    let named = build_params(
      "u",
      Some("p"),
      &"0".repeat(64),
      "192.0.2.10",
      "169",
      false,
      Some("Linux"),
      Some("Desktop"),
    );
    assert_eq!(get(&plain, "os"), None);
    assert_eq!(get(&named, "os"), Some("Linux".into()));
    assert_eq!(get(&named, "name"), Some("Desktop".into()));
    assert_eq!(get(&plain, "chksum"), get(&named, "chksum"));

    let out = build_params(
      "u",
      None,
      &"0".repeat(64),
      "192.0.2.10",
      "169",
      true,
      Some("Linux"),
      Some("Desktop"),
    );
    assert_eq!(get(&out, "os"), None, "a logout declares nothing");
    assert_eq!(get(&out, "name"), None);
  }

  /// The class follows the firmware's own chassis name; a battery is evidence
  /// that the machine is portable, and nothing is claimed without either.
  #[test]
  fn the_device_class_follows_the_chassis() {
    assert_eq!(class_from_chassis(Some(3), false), Some("Desktop"));
    assert_eq!(class_from_chassis(Some(35), false), Some("Mini PC"));
    assert_eq!(class_from_chassis(Some(9), true), Some("Laptop"));
    assert_eq!(class_from_chassis(Some(10), true), Some("Notebook"));
    assert_eq!(class_from_chassis(Some(31), false), Some("Convertible"));
    assert_eq!(class_from_chassis(None, true), Some("Laptop"));
    assert_eq!(
      class_from_chassis(None, false),
      None,
      "no evidence, no claim"
    );
    assert_eq!(
      class_from_chassis(Some(1), false),
      None,
      "firmware says Other"
    );
    assert_eq!(class_from_chassis(Some(99), true), Some("Laptop"));
  }

  /// The SMBIOS walker finds the System Enclosure structure and reads its
  /// chassis byte, and gives up rather than guessing when it is not there.
  #[test]
  fn the_chassis_byte_is_read_from_the_smbios_table() {
    let mut table = vec![0u8, 4, 0, 0, 0, 0]; // a type 0 structure, no strings
    // Type 3, 13 bytes long, chassis type 35 (Mini PC), lock bit clear.
    table.extend([3, 13, 0, 0, 0, 35, 0, 0, 0, 0, 0, 0, 0]);
    table.extend([0, 0]);
    table.extend([127, 4, 0, 0, 0, 0]); // end of table
    assert_eq!(chassis_from_smbios(&table), Some(35));

    let mut locked = table.clone();
    locked[11] = 0x80 | 9; // type 3's byte 5, with the lock bit set
    assert_eq!(chassis_from_smbios(&locked), Some(9));

    assert_eq!(chassis_from_smbios(&[0u8, 4, 0, 0, 0, 0]), None);
    assert_eq!(chassis_from_smbios(&[]), None);
  }

  /// A Mac has no chassis type, but its body is still identifiable: a battery
  /// means a laptop, and the model identifier names the desktop bodies.
  #[test]
  fn a_mac_body_is_named_from_its_model_and_battery() {
    assert_eq!(mac_class_from_model("MacBookPro18,3"), "Laptop");
    assert_eq!(mac_class_from_model("Macmini9,1"), "Mini PC");
    assert_eq!(mac_class_from_model("iMac21,1"), "All in One");
    assert_eq!(mac_class_from_model("MacPro7,1"), "Tower");
    assert_eq!(mac_class_from_model("VirtualMac2,1"), "Desktop");
  }

  #[test]
  fn the_system_name_is_the_portals_own_word() {
    assert_eq!(
      portal_os_name(),
      match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "MacOS",
        "android" => "AndroidOS",
        "ios" => "iOS",
        "freebsd" => "FreeBSD",
        other => other,
      }
    );
  }

  /// The portal names the outcome in `error`; there is no `online` boolean. An
  /// answer means the campus portal was reachable - that is the evidence the
  /// core's campus verdict uses, and it must survive "not online" too.
  #[test]
  fn a_portal_answer_is_read_from_its_error_field() {
    // The current shape: `online_ip`, no `client_ip`.
    let online = Client::portal_view_of(
      r#"C_a_l_l_b_a_c_k({"error":"ok","online_ip":"192.0.2.10","user_name":"alice"})"#,
    );
    assert_eq!(
      online,
      Some(PortalView {
        logged_in: true,
        address: Some("192.0.2.10".into()),
      })
    );
    // The older shape reports the same address as `client_ip`.
    let old = Client::portal_view_of(r#"{"error":"ok","client_ip":"198.51.100.7"}"#);
    assert_eq!(old.unwrap().address.as_deref(), Some("198.51.100.7"));
    // A link without a session still gets a well-formed answer, and it names no
    // address: the answer alone is what marks the link as the campus one.
    let offline = Client::portal_view_of(
      r#"C_a_l_l_b_a_c_k({"error":"not_online","error_msg":"User not online","ecode":1})"#,
    );
    assert_eq!(
      offline,
      Some(PortalView {
        logged_in: false,
        address: None,
      })
    );
    // Anything else is not the campus portal speaking: a captive portal's HTML,
    // a random JSON document, a truncated answer.
    assert_eq!(Client::portal_view_of("<html>login</html>"), None);
    assert_eq!(Client::portal_view_of(r#"{"hello":"world"}"#), None);
    assert_eq!(Client::portal_view_of(""), None);
  }

  #[test]
  fn an_ac_that_cannot_be_reached_is_its_own_refusal() {
    // Verified answers: a wrong policy id, and the AC behind a stale id not
    // answering at all. Neither is a password problem, and neither gets better
    // by sending the same id again.
    assert!(is_ac_unreachable("E2808", "Control policy not found"));
    assert!(is_ac_unreachable("", "INFO failed, BAS respond timeout."));
    assert!(is_ac_unreachable("login_error", "bas respond timeout"));
    assert!(!is_ac_unreachable("E2901", "ldap_bind error"));
    assert!(!is_ac_unreachable("E2620", "E2620: You are already online"));
    assert!(!is_ac_unreachable("", ""));
  }

  #[test]
  fn a_tls_stream_cut_by_the_peer_is_a_lost_connection() {
    // What rustls says when a middlebox ends an HTTPS answer without
    // `close_notify`. It used to reach the banner verbatim, docs link and all.
    let io = std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      "peer closed connection without sending TLS close_notify: \
       https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof",
    );
    let err = NetError::transport(ureq::Error::Io(io));
    assert_eq!(err.cause, NetCause::Lost);
    // The technical sentence is still there for the log pane.
    assert!(err.detail.contains("close_notify"), "{}", err.detail);

    // A reset socket is the same news, whatever the layer that noticed it.
    for kind in [
      std::io::ErrorKind::UnexpectedEof,
      std::io::ErrorKind::ConnectionReset,
      std::io::ErrorKind::ConnectionAborted,
      std::io::ErrorKind::BrokenPipe,
    ] {
      let err = NetError::transport(ureq::Error::Io(std::io::Error::new(kind, "gone")));
      assert_eq!(err.cause, NetCause::Lost, "{kind:?}");
    }
  }

  #[test]
  fn the_default_route_names_the_uplink() {
    // Real shape of `/proc/net/route`: hex and little-endian, one header line.
    let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                 wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                 eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
    // The lowest metric wins, exactly as the kernel chooses.
    assert_eq!(default_route(table).as_deref(), Some("eth0"));

    let wifi_only = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                     wlan0\t00000000\t0105A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";
    assert_eq!(default_route(wifi_only).as_deref(), Some("wlan0"));

    // No default route (only a link-scope one), or no table at all.
    let no_default = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                      eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
    assert_eq!(default_route(no_default), None);
    assert_eq!(default_route("Iface\tDestination\tGateway\n"), None);
    assert_eq!(default_route(""), None);
  }

  /// `rad_user_info` nests the per-device list in a JSON string, so the answer
  /// the rest of the app sees carries it as an object. A field in any other
  /// shape keeps exactly what the portal sent.
  #[test]
  fn the_device_detail_is_expanded_from_its_json_string() {
    // Documentation addresses and a made-up session id.
    let nested = r#"{"1052067690":{"class_name":"","ip":"192.0.2.10","ip6":"2001:db8::1","os_name":"AndroidOS","rad_online_id":"1052067690"}}"#;
    let expanded = expanded_device_detail(&Value::String(nested.into()))
      .expect("a JSON object inside a string is expanded");
    assert_eq!(expanded["1052067690"]["ip"], "192.0.2.10");
    assert_eq!(expanded["1052067690"]["rad_online_id"], "1052067690");

    // Not JSON, JSON that is not an object, and a value that is not a string.
    assert_eq!(expanded_device_detail(&Value::String("n/a".into())), None);
    assert_eq!(expanded_device_detail(&Value::String("[1,2]".into())), None);
    assert_eq!(expanded_device_detail(&json!({ "a": 1 })), None);
  }
}
