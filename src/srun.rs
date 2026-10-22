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
        // Windows reports a DNS failure as an "uncategorized" io error, so the
        // Winsock codes have to be read directly.
        _ => match io.raw_os_error() {
          Some(11001 | 11002 | 11003 | 11004) => NetCause::Resolve,
          Some(10060) => NetCause::Timeout,
          Some(10061) => NetCause::Refused,
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
}

impl Client {
  pub fn new(host: impl Into<String>, https: bool, timeout: Duration) -> Self {
    let config = ureq::Agent::config_builder()
      .timeout_global(Some(timeout))
      .build();
    Self {
      host: host.into(),
      https,
      agent: config.into(),
    }
  }

  fn scheme(&self) -> &'static str {
    if self.https { "https" } else { "http" }
  }

  fn get(&self, url: &str) -> Result<String, NetError> {
    self
      .agent
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
    let v = Self::jsonp(&self.get(&url)?)?;
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
    Self::jsonp(&self.get(&url)?)
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
}
