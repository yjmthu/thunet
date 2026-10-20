//! UI / log strings in Chinese and English.
//!
//! `Config::lang` stores `"auto" | "zh" | "en"`; `"auto"` resolves through the
//! OS locale, so the app follows the system language by default.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
  Zh,
  En,
}

impl Lang {
  pub fn from_setting(s: &str) -> Self {
    match s.trim().to_ascii_lowercase().as_str() {
      "zh" | "zh-cn" | "zh-hans" => Lang::Zh,
      "en" | "en-us" | "en-gb" => Lang::En,
      _ => Lang::system(),
    }
  }

  /// Follow the OS language; anything not Chinese falls back to English.
  pub fn system() -> Self {
    let loc = sys_locale::get_locale().unwrap_or_default();
    let loc = loc.to_ascii_lowercase();
    if loc.starts_with("zh") {
      Lang::Zh
    } else {
      Lang::En
    }
  }

  pub fn texts(self) -> &'static Texts {
    match self {
      Lang::Zh => &ZH,
      Lang::En => &EN,
    }
  }
}

pub struct Texts {
  // window chrome
  pub connect: &'static str,
  pub settings: &'static str,
  pub back: &'static str,
  pub disconnect: &'static str,
  pub refresh: &'static str,
  pub copy: &'static str,
  pub autostart: &'static str,
  pub start_minimized: &'static str,
  pub auto_connect: &'static str,
  pub password: &'static str,
  pub remember_password: &'static str,
  pub remember_password_hint: &'static str,
  pub save: &'static str,
  pub saved: &'static str,
  pub copied: &'static str,
  pub copy_line: &'static str,
  pub copy_all: &'static str,
  pub need_credentials: &'static str,
  pub network_unavailable: &'static str,
  pub no_login_needed: &'static str,
  pub not_campus_network: &'static str,
  pub secure_wifi: &'static str,
  pub language: &'static str,
  pub lang_auto: &'static str,
  pub lang_zh: &'static str,
  pub lang_en: &'static str,
  // status
  pub online: &'static str,
  pub authenticating: &'static str,
  pub offline: &'static str,
  pub account: &'static str,
  pub ipv4: &'static str,
  pub ipv6: &'static str,
  pub session_traffic: &'static str,
  // messages
  pub login_failed: &'static str,
  pub connect_ok: &'static str,
  pub connect_failed: &'static str,
  pub config_path: &'static str,
  pub refresh_failed: &'static str,
  pub disconnect_failed: &'static str,
  pub autostart_failed: &'static str,
  pub already_online: &'static str,
  pub not_connected: &'static str,
  pub status_refreshed: &'static str,
  pub detected_offline: &'static str,
  pub disconnected: &'static str,
  // tray
  pub show_window: &'static str,
  pub desktop_shortcut: &'static str,
  pub done: &'static str,
  pub quit: &'static str,
  // session
  pub already_online_session: &'static str,
  pub login_ok: &'static str,
  pub login_ok_verified: &'static str,
  pub request_failed: &'static str,
  pub login_rejected: &'static str,
  pub ac_id_hint: &'static str,
}

static ZH: Texts = Texts {
  connect: "连接",
  settings: "设置",
  back: "← 返回",
  disconnect: "断开",
  refresh: "刷新",
  copy: "复制",
  autostart: "开机自启",
  start_minimized: "启动时最小化到托盘",
  auto_connect: "启动时自动连接",
  password: "密码",
  remember_password: "记住密码",
  remember_password_hint: "保存在 Windows 凭据管理器（thunet/auth4.tsinghua.edu.cn），不会写入配置文件",
  save: "保存",
  saved: "已保存",
  copied: "已复制",
  copy_line: "复制本行",
  copy_all: "复制全部",
  need_credentials: "请先在下方输入账号和密码",
  network_unavailable: "当前没有网络连接（网线未插或 Wi-Fi 未连接），暂不尝试登录",
  no_login_needed: "当前网络可直接访问外网，无需登录",
  not_campus_network: "当前网络不是清华校园网登录环境，跳过登录",
  secure_wifi: "已连接 {}（系统级 802.1X 认证），无需门户登录",
  language: "语言",
  lang_auto: "跟随系统",
  lang_zh: "中文",
  lang_en: "English",
  online: "在线",
  authenticating: "认证中",
  offline: "离线",
  account: "账号",
  ipv4: "IPv4",
  ipv6: "IPv6",
  session_traffic: "本次流量",
  login_failed: "登录失败",
  connect_ok: "连接成功",
  connect_failed: "连接失败",
  config_path: "配置文件",
  refresh_failed: "刷新失败",
  disconnect_failed: "断开失败",
  autostart_failed: "自启设置失败",
  already_online: "已经在线，无需重复连接",
  not_connected: "当前未连接",
  status_refreshed: "状态已刷新",
  detected_offline: "检测到已离线",
  disconnected: "已断开",
  show_window: "显示窗口",
  desktop_shortcut: "创建桌面快捷方式",
  done: "完成",
  quit: "退出",
  already_online_session: "已在线",
  login_ok: "登录成功",
  login_ok_verified: "登录成功（服务器返回 {}，已验证在线）",
  request_failed: "请求认证服务器 {} 失败：{}",
  login_rejected: "登录被拒绝（ac_id={}）：{}",
  ac_id_hint: "提示：换网络后 ac_id 可能变化，可用 --ac-id 手动指定",
};

static EN: Texts = Texts {
  connect: "Connect",
  settings: "Settings",
  back: "← Back",
  disconnect: "Disconnect",
  refresh: "Refresh",
  copy: "Copy",
  autostart: "Start with system",
  start_minimized: "Start minimised to tray",
  auto_connect: "Connect on startup",
  password: "Password",
  remember_password: "Remember password",
  remember_password_hint: "Stored in Windows Credential Manager (thunet/auth4.tsinghua.edu.cn), never in the config file",
  save: "Save",
  saved: "Saved",
  copied: "Copied",
  copy_line: "Copy line",
  copy_all: "Copy all",
  need_credentials: "Enter your account and password below",
  network_unavailable: "No network connection (cable unplugged / Wi-Fi down), not logging in",
  no_login_needed: "This network already reaches the internet; no login needed",
  not_campus_network: "This does not look like the Tsinghua campus login network; skipping",
  secure_wifi: "Connected to {} (OS-level 802.1X); no portal login needed",
  language: "Language",
  lang_auto: "System",
  lang_zh: "中文",
  lang_en: "English",
  online: "Online",
  authenticating: "Authenticating",
  offline: "Offline",
  account: "Account",
  ipv4: "IPv4",
  ipv6: "IPv6",
  session_traffic: "Session traffic",
  login_failed: "Login failed",
  connect_ok: "Connected",
  connect_failed: "Connection failed",
  config_path: "Config file",
  refresh_failed: "Refresh failed",
  disconnect_failed: "Disconnect failed",
  autostart_failed: "Failed to change autostart",
  already_online: "Already online, nothing to do",
  not_connected: "Not connected",
  status_refreshed: "Status refreshed",
  detected_offline: "Session ended, now offline",
  disconnected: "Disconnected",
  show_window: "Show window",
  desktop_shortcut: "Create desktop shortcut",
  done: "Done",
  quit: "Quit",
  already_online_session: "already online",
  login_ok: "login ok",
  login_ok_verified: "login ok (server reported {}, verified online)",
  request_failed: "request to {} failed: {}",
  login_rejected: "login rejected (ac_id={}): {}",
  ac_id_hint: "Hint: ac_id can change with the network; pass --ac-id to override",
};
