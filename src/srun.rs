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

  /// GET an arbitrary URL; used by the keepalive loop to generate traffic.
  pub fn touch(&self, url: &str) -> Result<usize, NetError> {
    Ok(self.get(url)?.len())
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
    let params = build_params(username, password, &token, ip, ac_id, logout);
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

  /// The address the portal sees this request coming from, or `None` when the
  /// portal did not answer with one.
  ///
  /// It is the one fact that says "this machine is on the campus network",
  /// even where the gateway does not intercept HTTP: the portal reports the
  /// caller's own address, which only matches ours while no NAT stands
  /// between the two.
  pub fn portal_address(&self) -> Option<String> {
    let url = self.url("/cgi-bin/rad_user_info", &[]);
    let body = self.get(&url).ok()?;
    let v = Self::jsonp(&body).ok()?;
    v.get("client_ip")
      .and_then(Value::as_str)
      .filter(|ip| !ip.trim().is_empty())
      .map(|ip| ip.trim().to_string())
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

/// Build the full login/logout parameter set (order is irrelevant).
pub fn build_params(
  username: &str,
  password: Option<&str>,
  token: &str,
  ip: &str,
  ac_id: &str,
  logout: bool,
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

/// Which kind of link carries that route: a cable, Wi-Fi, or neither.
///
/// The window shows it next to the account, because a laptop can have both a
/// cable and a Wi-Fi association up and the portal session belongs to exactly
/// one of them - a session that appears to come and go is usually two links
/// taking turns.
pub fn link_kind() -> crate::proto::LinkKind {
  link_imp::link_kind()
}

/// Name of the interface behind the default route (`wlan0`, `enp3s0`).
///
/// The SSID probe needs it for its `iw` fallback, which is per interface;
/// `nmcli` names the active network without one.
#[cfg(target_os = "linux")]
pub fn link_iface() -> Option<String> {
  let table = std::fs::read_to_string("/proc/net/route").ok()?;
  default_route(&table)
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
    match best_index().and_then(if_type) {
      Some(IF_TYPE_ETHERNET_CSMACD) => LinkKind::Ethernet,
      Some(IF_TYPE_IEEE80211) => LinkKind::Wlan,
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

  /// `IfType` of the adapter with that index, or `None` when the list cannot be
  /// read (which is a different case from "not found" only for the log).
  fn if_type(index: u32) -> Option<u32> {
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
          return Some(adapter.IfType);
        }
        entry = adapter.Next;
      }
      return None;
    }
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
      // ARPHRD_ETHER is what a wired NIC reports; a tunnel reports 65534.
      Ok(kind) if kind.trim() == "1" => LinkKind::Ethernet,
      Ok(_) => LinkKind::Other,
      Err(_) => LinkKind::Other,
    }
  }
}

#[cfg(not(any(windows, target_os = "linux")))]
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

#[cfg(test)]
mod tests {
  use super::*;

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
    let p = build_params("u", Some("p"), &"0".repeat(64), "1.2.3.4", "1", false);
    let get = |k: &str| p.iter().find(|(n, _)| n == k).unwrap().1.clone();
    assert_eq!(get("password"), format!("{{MD5}}{}", crypto::md5_hex("p")));
    assert!(get("info").starts_with("{SRBX1}"));
    assert_eq!(get("chksum").len(), 40);
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
}
