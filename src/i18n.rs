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
  /// Shown instead of the autostart row in portable mode, which has no
  /// service to start at boot.
  pub portable_autostart: &'static str,
  pub start_minimized: &'static str,
  /// Sign in by itself right after the core starts.
  pub auto_connect: &'static str,
  /// Keep the session up after it was established, until the user disconnects.
  pub keep_online: &'static str,
  pub keep_online_hint: &'static str,
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
  /// The account is being switched: the old session is logged out and the new
  /// pair is tried, which drops the connection for a moment.
  pub switching_account: &'static str,
  /// The new pair worked and is now the stored one.
  pub switched_account: &'static str,
  /// The new pair did not work; the previous account signed back in and the
  /// stored credential was left untouched.
  pub switch_failed: &'static str,
  /// The portal still lists this address as online, so nothing was switched.
  pub switch_pending: &'static str,
  /// Confirmation shown when a switch has no stored credential to fall back on.
  pub switch_warning_title: &'static str,
  pub switch_warning_body: &'static str,
  pub switch_continue: &'static str,
  pub cancel: &'static str,
  /// Amber band: auto connect is on, but there is nothing to connect with.
  pub auto_connect_without_credentials: &'static str,
  /// The same gap while a leftover session is still online: it looks healthy,
  /// but nothing can sign in again once that session ends.
  pub auto_connect_without_credentials_online: &'static str,
  /// Hover on a disabled *Apply*: the form is incomplete, or nothing changed.
  pub password_required: &'static str,
  pub nothing_to_apply: &'static str,
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
  /// Why a portal request failed, one line per [`crate::srun::NetCause`]. The
  /// banner uses these instead of the OS's own text, which Windows localizes
  /// on its own terms.
  pub net_resolve: &'static str,
  pub net_refused: &'static str,
  pub net_timeout: &'static str,
  pub net_unreachable: &'static str,
  /// The other side closed the connection mid-answer.
  pub net_lost: &'static str,
  pub net_other: &'static str,
  /// Shown while a login could still happen: the 802.1X network is the reason it
  /// cannot.
  pub secure_wifi: &'static str,
  /// Shown while the session is already up on that network, so there is nothing
  /// left for the user to do.
  pub secure_wifi_online: &'static str,
  /// A portal verb was asked for on a network whose session belongs to the OS.
  pub disconnect_unavailable: &'static str,
  pub credentials_unavailable: &'static str,
  pub language: &'static str,
  pub log_level: &'static str,
  pub theme: &'static str,
  pub theme_auto: &'static str,
  pub theme_light: &'static str,
  pub theme_dark: &'static str,
  pub lang_auto: &'static str,
  pub lang_zh: &'static str,
  pub lang_en: &'static str,
  // status
  pub online: &'static str,
  pub authenticating: &'static str,
  pub offline: &'static str,
  pub account: &'static str,
  /// Label of the row that names the link the session rides on (cable, Wi-Fi).
  pub link: &'static str,
  pub link_ethernet: &'static str,
  pub link_wlan: &'static str,
  pub link_none: &'static str,
  pub link_other: &'static str,
  /// Tooltip of the heartbeat ring beside the state label.
  pub heartbeat_hint: &'static str,
  pub ipv4: &'static str,
  pub ipv6: &'static str,
  pub session_traffic: &'static str,
  /// Heading of the list that clicking the device badge shows, with its
  /// trailing separator: the count is written straight after it, and Chinese
  /// wants a full-width colon there.
  pub devices_title: &'static str,
  /// Status-page row describing the background service process.
  pub service: &'static str,
  /// The same row in portable mode, where the core is a thread of the window
  /// rather than a service: there is nothing to start, stop or restart.
  pub core_row: &'static str,
  pub core_in_window: &'static str,
  pub service_running: &'static str,
  pub service_gone: &'static str,
  /// Red banner on the status page while no core is answering.
  pub service_down: &'static str,
  /// Red banner while a core answers with another protocol version: it runs,
  /// but this build cannot talk to it until someone restarts it. `{}` is the
  /// protocol the core reported.
  pub service_stale: &'static str,
  /// Button on that row: stop the service while it runs, start it otherwise.
  pub service_start: &'static str,
  pub service_stop: &'static str,
  /// Same button while the core speaks another protocol: a start is a no-op
  /// for a service that already runs, so it has to be restarted.
  pub service_restart: &'static str,
  /// Toasts for that button. They fire when the request actually succeeded, or
  /// when it provably did not; none of them uses a colon.
  pub service_started: &'static str,
  pub service_stopped: &'static str,
  pub service_start_failed: &'static str,
  pub service_stop_failed: &'static str,
  /// Apply was pressed while the machine service was down; a start is running.
  pub service_starting: &'static str,
  // messages
  pub login_failed: &'static str,
  pub connect_ok: &'static str,
  pub connect_failed: &'static str,
  /// The portal rejected the account/password pair (see
  /// `srun::is_credentials_error`). It is the red banner, worded when the
  /// banner is painted so a language switch rewrites it; the raw JSON answer
  /// goes to the log instead.
  pub invalid_credentials: &'static str,
  /// The portal could not reach an access controller for the `ac_id` in use.
  pub ac_unreachable: &'static str,
  /// State label: the link reaches the internet without a portal session.
  pub online_without_portal: &'static str,
  pub refresh_failed: &'static str,
  pub disconnect_failed: &'static str,
  pub autostart_failed: &'static str,
  /// The autostart change needs administrator rights and the UAC prompt was
  /// dismissed; nothing was changed.
  pub admin_cancelled: &'static str,
  /// Administrator rights could not be obtained at all (no polkit agent, no
  /// graphical session, or the dialog was dismissed - `pkexec` reports both the
  /// same way), so the action did not run.
  pub admin_unavailable: &'static str,
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

impl Texts {
  /// Words for a failed portal request, so the banner follows the app's
  /// language instead of whatever language the operating system prints.
  pub fn net_cause(&self, cause: crate::srun::NetCause) -> &'static str {
    use crate::srun::NetCause;
    match cause {
      NetCause::Resolve => self.net_resolve,
      NetCause::Refused => self.net_refused,
      NetCause::Timeout => self.net_timeout,
      NetCause::Unreachable => self.net_unreachable,
      NetCause::Lost => self.net_lost,
      NetCause::Other => self.net_other,
    }
  }
}

impl crate::proto::LinkKind {
  /// Words for the link row: which interface carries the session.
  pub fn text(&self, t: &Texts) -> &'static str {
    use crate::proto::LinkKind;
    match self {
      LinkKind::None => t.link_none,
      LinkKind::Ethernet => t.link_ethernet,
      LinkKind::Wlan => t.link_wlan,
      LinkKind::Other => t.link_other,
    }
  }
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
  autostart_core: "仅服务",
  autostart_all: "服务和托盘",
  portable_autostart: "便携模式下没有开机自启：后台核心随本会话启动，退出即结束",
  start_minimized: "启动时最小化到托盘",
  auto_connect: "启动时自动连接",
  keep_online: "保持连接",
  keep_online_hint: "掉线后自动重连，直到你手动断开（启动时自动连接只负责启动一次）",
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
  switching_account: "正在切换账号，网络会中断片刻…",
  switched_account: "账号已切换",
  switch_failed: "新凭据未通过，已用原账号重新登录",
  switch_pending: "校园网仍显示本机在线，稍后再试",
  switch_warning_title: "没有可回退的密码",
  switch_warning_body: "本机只有在校园网接受新密码后才会重新联网。如果新密码有误，它会掉线，而且本机没有旧密码可以重连——需要到现场或换一条线路才能恢复。确定继续吗？",
  switch_continue: "仍要切换",
  cancel: "取消",
  auto_connect_without_credentials: "启动时自动连接已开启，但没有保存账号或密码",
  auto_connect_without_credentials_online: "当前会话可继续，但没有保存密码——掉线后不会自动重连",
  password_required: "保存密码或更换账号都需要重新输入密码",
  nothing_to_apply: "没有需要应用的修改",
  no_credentials: "没有可用的账号或密码",
  built: "构建于",
  on: "已开启",
  off: "已关闭",
  copied: "已复制",
  copy_line: "复制本行",
  copy_all: "复制全部",
  network_unavailable: "当前没有网络连接（网线未插或 Wi-Fi 未连接），暂不尝试登录",
  no_login_needed: "当前网络不经过校园门户（未检测到拦截），本机不会尝试登录",
  not_campus_network: "当前网络不是清华校园网登录环境，跳过登录",
  net_resolve: "无法解析服务器地址（DNS）",
  net_refused: "服务器拒绝了连接",
  net_timeout: "连接服务器超时",
  net_unreachable: "当前没有可用的网络（网线未插或网卡已禁用）",
  net_lost: "连接被对端中断（服务器或网关提前关闭了连接）",
  net_other: "网络请求失败",
  secure_wifi: "已连接 {}（系统级 802.1X 认证），无需门户登录",
  secure_wifi_online: "已通过 {} 登录（系统级 802.1X 认证），无需操作",
  disconnect_unavailable: "当前网络由系统级 802.1X 认证，没有本程序建立的门户会话可断开",
  credentials_unavailable: "当前网络由系统级 802.1X 认证，门户账号在这里不会被使用",
  language: "语言",
  log_level: "日志",
  theme: "皮肤",
  theme_auto: "自动",
  theme_light: "亮色",
  theme_dark: "暗色",
  lang_auto: "跟随系统",
  lang_zh: "中文",
  lang_en: "English",
  online: "在线",
  online_without_portal: "已联网（未登录）",
  authenticating: "认证中",
  offline: "离线",
  account: "账号",
  link: "网络",
  link_ethernet: "以太网",
  link_wlan: "WLAN",
  link_none: "未连接",
  link_other: "其他",
  heartbeat_hint: "在线每 15 秒、离线每 10 秒查询一次门户状态；圆环每次收到回答后重新开始",
  ipv4: "IPv4",
  ipv6: "IPv6",
  session_traffic: "本次流量",
  devices_title: "校园网在线设备：",
  service: "后台服务",
  core_row: "核心",
  core_in_window: "随本窗口运行",
  service_running: "运行中",
  service_gone: "未运行",
  service_down: "后台服务当前没有运行，暂时无法连接或保持在线，点「启动」按钮即可恢复。",
  service_stale: "后台服务还在运行旧版本（协议 {}），重启后才能恢复连接。",
  service_start: "启动",
  service_stop: "停止",
  service_restart: "重启",
  service_started: "后台服务已启动",
  service_stopped: "后台服务已停止",
  service_start_failed: "后台服务启动失败",
  service_stop_failed: "后台服务停止失败",
  service_starting: "正在启动后台服务，请稍候…",
  login_failed: "登录失败",
  connect_ok: "连接成功",
  connect_failed: "连接失败",
  invalid_credentials: "用户名或密码错误",
  ac_unreachable: "门户无法把登录送到当前链路的接入点（ac_id 不属于这条线路），已停止自动连接；换到正确的线路后点“连接”再试",
  refresh_failed: "刷新失败",
  disconnect_failed: "断开失败",
  autostart_failed: "自启设置失败",
  admin_cancelled: "需要管理员权限，已取消",
  admin_unavailable: "未获得管理员授权（可能已取消，或当前会话无法弹出授权框）；请在终端里执行 `sudo thunet --machine service enable|start|stop`",
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
  autostart_core: "Service only",
  autostart_all: "Service and tray",
  portable_autostart: "Portable mode has no autostart: the core belongs to this session",
  start_minimized: "Start minimised to tray",
  auto_connect: "Connect on startup",
  keep_online: "Stay online",
  keep_online_hint: "Sign back in after the session drops, until you disconnect (auto connect only covers the start)",
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
  switching_account: "Switching the account, the connection drops for a moment…",
  switched_account: "Account switched",
  switch_failed: "The new credentials were refused; signed back in with the previous account",
  switch_pending: "The portal still lists this machine as online; try again in a moment",
  switch_warning_title: "No password to fall back on",
  switch_warning_body: "This machine only gets back online if the portal accepts the new password. If it is wrong, it drops off the network and no stored password can sign it back in — recovering it would need physical access or another path. Continue?",
  switch_continue: "Switch anyway",
  cancel: "Cancel",
  auto_connect_without_credentials: "Connect on startup is on, but no account or password is saved",
  auto_connect_without_credentials_online: "This session keeps running, but no password is saved — it will not reconnect on its own",
  password_required: "Saving a password or changing the account needs the password typed in",
  nothing_to_apply: "Nothing to apply",
  no_credentials: "No account or password saved",
  built: "Built at",
  on: "On",
  off: "Off",
  copied: "Copied",
  copy_line: "Copy line",
  copy_all: "Copy all",
  network_unavailable: "No network connection (cable unplugged / Wi-Fi down), not logging in",
  no_login_needed: "This link does not go through the campus portal (nothing intercepts HTTP); no login is attempted",
  not_campus_network: "This does not look like the Tsinghua campus login network; skipping",
  net_resolve: "Cannot resolve the server address (DNS)",
  net_refused: "The server refused the connection",
  net_timeout: "The connection to the server timed out",
  net_unreachable: "No network route to the server (cable unplugged or adapter disabled)",
  net_lost: "The connection was closed by the other side (server or gateway hung up)",
  net_other: "The network request failed",
  secure_wifi: "Connected to {} (OS-level 802.1X); no portal login needed",
  secure_wifi_online: "Signed in through {} (OS-level 802.1X); nothing to do",
  disconnect_unavailable: "This network authenticates at the OS level; this app has no portal session here to end",
  credentials_unavailable: "This network authenticates at the OS level; the portal account is not used here",
  language: "Language",
  log_level: "Log",
  theme: "Skin",
  theme_auto: "Auto",
  theme_light: "Light",
  theme_dark: "Dark",
  lang_auto: "System",
  lang_zh: "中文",
  lang_en: "English",
  online: "Online",
  online_without_portal: "Online (unauthenticated)",
  authenticating: "Authenticating",
  offline: "Offline",
  account: "Account",
  link: "Link",
  link_ethernet: "Ethernet",
  link_wlan: "WLAN",
  link_none: "No link",
  link_other: "Other",
  heartbeat_hint: "Portal status is polled every 15 s online and 10 s offline; the ring restarts with each answer",
  ipv4: "IPv4",
  ipv6: "IPv6",
  session_traffic: "Session traffic",
  devices_title: "Online devices: ",
  service: "Service",
  core_row: "Core",
  core_in_window: "runs in this window",
  service_running: "running",
  service_gone: "not running",
  service_down: "The background service is not running, so you cannot sign in or stay online. Use the Start button to bring it back.",
  service_stale: "The background service still runs an older build (protocol {}); restart it to reconnect.",
  service_start: "Start",
  service_stop: "Stop",
  service_restart: "Restart",
  service_started: "Service started",
  service_stopped: "Service stopped",
  service_start_failed: "Service failed to start",
  service_stop_failed: "Service failed to stop",
  service_starting: "Starting the background service…",
  login_failed: "Login failed",
  connect_ok: "Connected",
  connect_failed: "Connection failed",
  invalid_credentials: "Wrong username or password",
  ac_unreachable: "The portal could not reach the access controller for this link's ac_id; automatic attempts stopped — press Connect to try again",
  refresh_failed: "Refresh failed",
  disconnect_failed: "Disconnect failed",
  autostart_failed: "Failed to change autostart",
  admin_cancelled: "Administrator rights are required; cancelled",
  admin_unavailable: "Administrator authorization was not granted (it may have been dismissed, or this session cannot show a prompt); from a terminal run `sudo thunet --machine service enable|start|stop`",
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
