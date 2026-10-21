//! UI / log strings in Chinese and English.
//!
//! `Config::lang` stores `"auto" | "zh" | "en"`; `"auto"` resolves through the
//! OS locale, so the app follows the system language by default.

use serde::{Deserialize, Serialize};

/// The resolved language. It travels over the IPC link so the core can word the
/// messages it generates itself (skip reasons, toasts) in the same language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
  /// The three autostart modes: none / background only / background + tray.
  pub autostart_off: &'static str,
  pub autostart_core: &'static str,
  pub autostart_all: &'static str,
  pub start_minimized: &'static str,
  pub auto_connect: &'static str,
  pub password: &'static str,
  pub remember_password: &'static str,
  /// Where the password ends up; the wording is the same on every platform.
  pub remember_password_hint: &'static str,
  pub apply: &'static str,
  /// Shown next to *Apply* while the row differs from what is stored.
  pub unsaved: &'static str,
  pub revert: &'static str,
  pub revert_hint: &'static str,
  /// Applying with the checkbox on writes the credential ...
  pub password_saved: &'static str,
  /// ... and with it off deletes the stored one.
  pub password_cleared: &'static str,
  /// "Remember" is on but the password field is empty: nothing is stored.
  pub password_missing: &'static str,
  /// Placeholder for an empty field while the core holds a saved password: an
  /// empty field means "keep it", not "delete it".
  pub password_keep: &'static str,
  /// Connect was pressed while no account/password is available at all.
  pub no_credentials: &'static str,
  /// Prefix of the compile-time stamp next to the version.
  pub built: &'static str,
  /// Settings switches confirm the state they just moved to as
  /// "<label>: On" / "<label>: Off".
  pub on: &'static str,
  pub off: &'static str,
  pub copied: &'static str,
  pub copy_line: &'static str,
  pub copy_all: &'static str,
  pub network_unavailable: &'static str,
  pub no_login_needed: &'static str,
  pub not_campus_network: &'static str,
  pub secure_wifi: &'static str,
  pub language: &'static str,
  pub log_level: &'static str,
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
  /// Status-page row describing the background service process.
  pub service: &'static str,
  pub service_running: &'static str,
  pub service_gone: &'static str,
  /// Red banner on the status page while no core is answering.
  pub service_down: &'static str,
  /// Button on that row: stop the service while it runs, start it otherwise.
  pub service_start: &'static str,
  pub service_stop: &'static str,
  /// Toasts for that button. They fire when the request actually succeeded, or
  /// when it provably did not; none of them uses a colon.
  pub service_started: &'static str,
  pub service_stopped: &'static str,
  pub service_start_failed: &'static str,
  pub service_stop_failed: &'static str,
  /// Apply was pressed while the machine service was down; a start is running.
  pub service_starting: &'static str,
  /// The core could not write the sealed credential (permissions, disk).
  pub cannot_save_credential: &'static str,
  // messages
  pub connect_ok: &'static str,
  pub connect_failed: &'static str,
  /// The portal rejected the account/password pair (see
  /// `srun::is_credentials_error`). The raw JSON goes to the log instead.
  pub invalid_credentials: &'static str,
  pub refresh_failed: &'static str,
  pub disconnect_failed: &'static str,
  pub autostart_failed: &'static str,
  /// The autostart change needs administrator rights and the UAC prompt was
  /// dismissed; nothing was changed.
  pub admin_cancelled: &'static str,
  pub already_online: &'static str,
  pub not_connected: &'static str,
  pub status_refreshed: &'static str,
  pub disconnected: &'static str,
  // tray
  pub show_window: &'static str,
  pub hide_window: &'static str,
  pub desktop_shortcut: &'static str,
  pub done: &'static str,
  pub quit: &'static str,
}

static ZH: Texts = Texts {
  connect: "连接",
  settings: "设置",
  back: "← 返回",
  disconnect: "断开",
  refresh: "刷新",
  copy: "复制",
  autostart: "开机自启",
  autostart_off: "关闭",
  autostart_core: "服务",
  autostart_all: "服务 + 托盘",
  start_minimized: "启动时最小化到托盘",
  auto_connect: "启动时自动连接",
  password: "密码",
  remember_password: "记住密码",
  remember_password_hint: "加密后保存到配置目录下的 credential.bin（密钥与本机绑定，换机器需重新输入），不会写入 config.json",
  apply: "应用",
  unsaved: "未保存",
  revert: "还原",
  revert_hint: "恢复为已保存的账号和密码",
  password_saved: "密码已保存",
  password_cleared: "密码已清除",
  password_missing: "未填写密码",
  password_keep: "已保存，留空则不修改",
  no_credentials: "没有可用的账号或密码",
  built: "构建于",
  on: "已开启",
  off: "已关闭",
  copied: "已复制",
  copy_line: "复制本行",
  copy_all: "复制全部",
  network_unavailable: "当前没有网络连接（网线未插或 Wi-Fi 未连接），暂不尝试登录",
  no_login_needed: "当前网络可直接访问外网，无需登录",
  not_campus_network: "当前网络不是清华校园网登录环境，跳过登录",
  secure_wifi: "已连接 {}（系统级 802.1X 认证），无需门户登录",
  language: "语言",
  log_level: "日志级别",
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
  service: "后台服务",
  service_running: "运行中",
  service_gone: "未运行",
  service_down: "后台服务当前没有运行，暂时无法连接或保持在线，点「启动」按钮即可恢复。",
  service_start: "启动",
  service_stop: "停止",
  service_started: "后台服务已启动",
  service_stopped: "后台服务已停止",
  service_start_failed: "后台服务启动失败",
  service_stop_failed: "后台服务停止失败",
  service_starting: "正在启动后台服务，请稍候…",
  cannot_save_credential: "无法保存凭据",
  connect_ok: "连接成功",
  connect_failed: "连接失败",
  invalid_credentials: "用户名或密码错误",
  refresh_failed: "刷新失败",
  disconnect_failed: "断开失败",
  autostart_failed: "自启设置失败",
  admin_cancelled: "需要管理员权限，已取消",
  already_online: "已经在线，无需重复连接",
  not_connected: "当前未连接",
  status_refreshed: "状态已刷新",
  disconnected: "已断开",
  show_window: "显示窗口",
  hide_window: "隐藏窗口",
  desktop_shortcut: "创建桌面快捷方式",
  done: "完成",
  quit: "退出",
};

static EN: Texts = Texts {
  connect: "Connect",
  settings: "Settings",
  back: "← Back",
  disconnect: "Disconnect",
  refresh: "Refresh",
  copy: "Copy",
  autostart: "Start with system",
  autostart_off: "Off",
  autostart_core: "Service",
  autostart_all: "Service + tray",
  start_minimized: "Start minimised to tray",
  auto_connect: "Connect on startup",
  password: "Password",
  remember_password: "Remember password",
  remember_password_hint: "Encrypted with a key bound to this machine into credential.bin in the config directory (moving it elsewhere needs the password again), never in config.json",
  apply: "Apply",
  unsaved: "Unsaved",
  revert: "Revert",
  revert_hint: "Restore the stored account and password",
  password_saved: "Password saved",
  password_cleared: "Password cleared",
  password_missing: "No password entered",
  password_keep: "Saved — leave blank to keep it",
  no_credentials: "No account or password saved",
  built: "Built at",
  on: "On",
  off: "Off",
  copied: "Copied",
  copy_line: "Copy line",
  copy_all: "Copy all",
  network_unavailable: "No network connection (cable unplugged / Wi-Fi down), not logging in",
  no_login_needed: "This network already reaches the internet; no login needed",
  not_campus_network: "This does not look like the Tsinghua campus login network; skipping",
  secure_wifi: "Connected to {} (OS-level 802.1X); no portal login needed",
  language: "Language",
  log_level: "Log level",
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
  service: "Service",
  service_running: "running",
  service_gone: "not running",
  service_down: "The background service is not running, so you cannot sign in or stay online. Use the Start button to bring it back.",
  service_start: "Start",
  service_stop: "Stop",
  service_started: "Service started",
  service_stopped: "Service stopped",
  service_start_failed: "Service failed to start",
  service_stop_failed: "Service failed to stop",
  service_starting: "Starting the background service…",
  cannot_save_credential: "Cannot save the credential",
  connect_ok: "Connected",
  connect_failed: "Connection failed",
  invalid_credentials: "Wrong username or password",
  refresh_failed: "Refresh failed",
  disconnect_failed: "Disconnect failed",
  autostart_failed: "Failed to change autostart",
  admin_cancelled: "Administrator rights are required; cancelled",
  already_online: "Already online, nothing to do",
  not_connected: "Not connected",
  status_refreshed: "Status refreshed",
  disconnected: "Disconnected",
  show_window: "Show window",
  hide_window: "Hide window",
  desktop_shortcut: "Create desktop shortcut",
  done: "Done",
  quit: "Quit",
};
