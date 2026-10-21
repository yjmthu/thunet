//! Autostart registration.
//!
//! Two things are registered: a **machine-wide service** that owns the network
//! session (`thunet core --machine`), and a **per-user tray** entry. The core
//! needs no display, so it can start before anybody logs in; the tray starts
//! with the user's desktop.
//!
//! Both platforms use the same mental model: the core is `thunet.service` on
//! Linux and the `thunet` service on Windows, while the frontend is the XDG
//! autostart file / a per-user Task Scheduler logon task
//! (`thunet-tray-<user>`).
//!
//! Service registration, the start type and start/stop need administrator
//! rights on both platforms; the tray entry is always per user and never needs
//! elevation.

use std::path::PathBuf;

/// How much of the app starts with the system.
///
/// There are two entries (tray and background core). The tray never starts a
/// core by itself, so "tray only" is not offered: it would put an icon on
/// screen whose only message is "service not running".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// Nothing starts automatically.
  Off,
  /// Only `thunet core`: the session is kept alive without a tray icon.
  Core,
  /// Core and tray.
  All,
}

/// What `thunet service <action>` does. The names follow systemd, the Windows
/// side maps them onto Task Scheduler commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceAction {
  /// Register the unit / task without starting anything.
  Install,
  /// Remove the registration: the service, plus the tray logon task on Windows.
  Uninstall,
  /// Start automatically from now on.
  Enable,
  /// Stop starting automatically (a running core keeps running).
  Disable,
  Start,
  Stop,
  Restart,
  /// Print what the platform's service manager says.
  Status,
}

#[derive(Clone)]
pub struct Autostart {
  exe: PathBuf,
  minimized: bool,
}

impl Autostart {
  /// `None` when the running executable path cannot be determined.
  pub fn new(minimized: bool) -> Option<Self> {
    Some(Self {
      exe: crate::exe_path()?,
      minimized,
    })
  }

  fn args(&self) -> &'static [&'static str] {
    if self.minimized {
      &["--minimized"]
    } else {
      &[]
    }
  }

  /// Which entries are registered right now.
  ///
  /// Derived from the OS, never cached in `config.json`: deleting a task or a
  /// unit by hand is reflected immediately, and there is no second source of
  /// truth to drift.
  pub fn mode(&self) -> Mode {
    // What starts at boot decides, not what is merely registered. A tray entry
    // on its own (a hand-made task, or one left over from an installation that
    // was removed) must not make the page claim "service + tray" while nothing
    // starts the core.
    if !imp::core_starts_at_boot() {
      return Mode::Off;
    }
    if imp::tray_registered(self) {
      Mode::All
    } else {
      Mode::Core
    }
  }

  pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
    // The core half goes first: on Windows it may raise a UAC prompt, and
    // cancelling that must leave the settings exactly as they were.
    // Enabling or disabling the tray alone never needs elevation once the
    // machine service is already in the requested state.
    match mode {
      Mode::Off => {
        imp::disable_core()?;
        imp::disable_tray(self)
      }
      Mode::Core => {
        if !imp::core_starts_at_boot() {
          imp::enable_core(self)?;
        }
        imp::disable_tray(self)
      }
      Mode::All => {
        if !imp::core_starts_at_boot() {
          imp::enable_core(self)?;
        }
        imp::enable_tray(self)
      }
    }
  }

  /// Rewrite the tray entry after its command line changed (`--minimized`).
  ///
  /// The core entry never carries that flag, so this deliberately leaves it
  /// alone — on Windows touching it would mean a UAC prompt for a checkbox.
  pub fn refresh_tray(&self) -> Result<(), String> {
    imp::refresh_tray(self)
  }

  /// Remove a tray entry that no boot-starting core backs.
  ///
  /// None of the three modes can produce that state: a lone tray entry is
  /// either hand-made or left over from an older installation, and it would
  /// only put an icon on screen that reports "service not running". The app
  /// cleans it up instead of showing a mode it cannot offer. `Ok(true)` means
  /// an entry was removed.
  pub fn drop_stray_tray(&self) -> Result<bool, String> {
    if imp::core_starts_at_boot() || !imp::tray_registered(self) {
      return Ok(false);
    }
    imp::disable_tray(self)?;
    Ok(true)
  }

  /// Install the service entry the CLI drives, if it is missing, so
  /// `thunet service start|stop|status` also works on a portable install.
  ///
  /// `Ok(true)` means an entry was written. Windows has nothing to prepare —
  /// registering the service *is* the install — and reports `false`.
  pub fn prepare_service(&self) -> Result<bool, String> {
    imp::prepare_service(self)
  }

  /// Bring the core back after an upgrade, but only when it is configured to
  /// start at boot.
  ///
  /// The installer stops the service before replacing the binary; this is what
  /// starts the new build again. A service whose autostart is off (start type
  /// `demand`) is left alone, so upgrading cannot resurrect a session the user
  /// turned off.
  pub fn resume_after_update() -> Result<(), String> {
    imp::resume_after_update()
  }

  /// `thunet service <action>`: run it and return the line to print.
  pub fn service(action: ServiceAction, minimized: bool) -> Result<String, String> {
    let me = Autostart::new(minimized).ok_or("cannot determine the executable path")?;
    imp::service(action, &me)
  }
}

/// Perform one service operation that needs administrator rights.
///
/// This is what the elevated copy of the executable runs
/// (`thunet service-admin <action>`); it must not ask for elevation again, or
/// the prompt would never end.
pub fn service_admin(action: ServiceAction) -> Result<(), String> {
  let me = Autostart::new(false).ok_or("cannot determine the executable path")?;
  imp::service_direct(action, &me).map(|_| ())
}

/// Delete the machine state directory after a real uninstall.
///
/// The MSI runs this only when it removes the product (`REMOVE="ALL"` without
/// `UPGRADINGPRODUCTCODE`), never for an upgrade, a repair or a reinstall: the
/// sealed credential and `key.bin` are machine state, so they must not outlive
/// the package. The per-user directory (window preferences, no secret) is left
/// alone.
pub fn purge_machine_state() -> Result<(), String> {
  imp::purge_machine_state()
}

/// Is the core registered with the OS as something that starts on its own?
///
/// A registered core belongs to systemd / Task Scheduler, so the tray must ask
/// the manager to start it instead of spawning a second, unmanaged copy.
pub fn core_managed() -> bool {
  imp::core_registered()
}

#[cfg(windows)]
mod imp {
  use std::os::windows::process::CommandExt as _;
  use std::path::Path;

  use windows_sys::Win32::Foundation::GetLastError;
  use windows_sys::Win32::System::Services::{
    ChangeServiceConfigW, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatusEx,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO,
    SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONTROL_STOP,
    SERVICE_DEMAND_START, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
  };

  use super::{Autostart, ServiceAction};

  /// Service Control Manager key of the background core. The same name as the
  /// Linux unit (`thunet.service`), so the mental model is identical on both
  /// platforms: `thunet` *is* the service, `thunet-tray-<user>` is the frontend.
  const CORE_SERVICE: &str = "thunet";
  /// Virtual service account. It has no profile and no interactive logon; the
  /// machine state directory is the only place it can write.
  const SERVICE_ACCOUNT: &str = r"NT SERVICE\thunet";

  /// Base service DACL. Interactive users may only query status; starting,
  /// stopping and reconfiguring stay administrator-only. The service SID is
  /// appended at install time.
  const SERVICE_SDDL: &str = concat!(
    "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)",
    "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)",
    "(A;;CCLCSWLOCRRC;;;IU)",
    "(A;;CCLCSWLOCRRC;;;SU)"
  );

  /// The tray task is per user: `thunet-tray-<USERNAME>`. A fixed name would
  /// let one user's `/Create /F` overwrite another user's entry.

  /// Never flash a console window: this binary uses the GUI subsystem.
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;

  const SERVICE_ERROR_NORMAL: u32 = 0x0000_0001;
  /// Standard access right `DELETE`, needed by `DeleteService`.
  const DELETE: u32 = 0x0001_0000;
  /// `ERROR_SERVICE_ALREADY_RUNNING` / `ERROR_SERVICE_NOT_ACTIVE`.
  const ALREADY_RUNNING: u32 = 1056;
  const NOT_ACTIVE: u32 = 1062;

  fn tray_task_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    format!("thunet-tray-{}", user.replace('\\', "_"))
  }

  pub fn tray_registered(_a: &Autostart) -> bool {
    task_exists(&tray_task_name())
  }

  /// The core is a service: the window asks the SCM to start it instead of
  /// spawning a second, unmanaged copy. This stays true when autostart is off
  /// but the service is still registered (start type `demand`) — the tray must
  /// not fight the SCM over who owns the process.
  pub fn core_registered() -> bool {
    service_open(SERVICE_QUERY_STATUS).is_some()
  }

  /// Whether the SCM starts it at boot. This, not registration, is what the
  /// settings page shows.
  pub fn core_starts_at_boot() -> bool {
    let Some(service) = service_open(SERVICE_QUERY_CONFIG) else {
      return false;
    };
    let start = query_start_type(service);
    unsafe {
      CloseServiceHandle(service);
    }
    start == Some(SERVICE_AUTO_START)
  }

  pub fn enable_tray(a: &Autostart) -> Result<(), String> {
    register(&a.exe, a.args(), &tray_task_name())
  }

  pub fn disable_tray(_a: &Autostart) -> Result<(), String> {
    delete_task(&tray_task_name())
  }

  /// Rewrite the tray task after its command line changed (`--minimized`).
  /// The core entry has no such argument, so it is deliberately left alone.
  pub fn refresh_tray(a: &Autostart) -> Result<(), String> {
    if !task_exists(&tray_task_name()) {
      return Ok(());
    }
    register(&a.exe, a.args(), &tray_task_name())
  }

  /// Register the core as a boot-time service and keep it running.
  ///
  /// Creating or reconfiguring a service needs administrator rights, so an
  /// ordinary caller is replaced by an elevated copy of this executable.
  pub fn enable_core(a: &Autostart) -> Result<(), String> {
    if crate::elevate::is_elevated() {
      enable_direct(a)
    } else {
      elevate("enable")
    }
  }

  pub fn disable_core() -> Result<(), String> {
    if crate::elevate::is_elevated() {
      stop_starting_at_boot()
    } else {
      elevate("disable")
    }
  }

  pub fn prepare_service(_a: &Autostart) -> Result<bool, String> {
    // Nothing to prepare: `service install` (or switching autostart to a
    // service mode) is what registers it.
    Ok(false)
  }

  /// Start the core again after the installer replaced the binary, after
  /// making sure the registration points at the build that was just installed.
  pub fn resume_after_update() -> Result<(), String> {
    if crate::elevate::is_elevated()
      && let Some(me) = Autostart::new(false)
    {
      sync_image_path(&me)?;
      // An upgrade keeps `%ProgramData%\thunet`, so a previous install's DACL
      // on `key.bin` and `credential.bin` survives it and the new core can read
      // `config.json` but not the key. Re-apply the service-only ACL to the
      // whole tree, not just to the directory.
      harden_machine_dir(Some(SERVICE_ACCOUNT))?;
    }
    if !core_starts_at_boot() {
      return Ok(());
    }
    if let Some(service) = service_open(SERVICE_QUERY_STATUS) {
      let state = query_state(service);
      unsafe {
        CloseServiceHandle(service);
      }
      if state == Some(SERVICE_RUNNING) {
        return Ok(());
      }
    }
    start()
  }

  /// Remove `%ProgramData%\thunet` and everything in it. The installer stops
  /// and deletes the service before this runs, so no file is held open.
  pub fn purge_machine_state() -> Result<(), String> {
    let dir = crate::config::machine_dir();
    match std::fs::remove_dir_all(&dir) {
      Ok(()) => Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(format!("{}: {e}", dir.display())),
    }
  }

  /// `thunet service <action>`. The actions that change the registration ask
  /// for administrator rights once; start/stop/status do not need them.
  pub fn service(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    if needs_admin(action) && !crate::elevate::is_elevated() {
      elevate(action_name(action))?;
      return Ok(already_done(action));
    }
    service_direct(action, a)
  }

  /// Start an elevated copy of this executable for one operation.
  ///
  /// The state directory is spelled out rather than recomputed: the elevated
  /// process may run as a different administrator, whose `%APPDATA%` is not
  /// the one the window writes to.
  fn elevate(action: &str) -> Result<(), String> {
    let dir = crate::config::machine_dir();
    crate::elevate::run_as_admin(&[
      "service-admin",
      action,
      "--state-dir",
      &dir.to_string_lossy(),
    ])
  }

  /// The action performed by the elevated copy (`thunet service-admin …`).
  pub fn service_direct(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    match action {
      ServiceAction::Install => {
        install(a)?;
        Ok("core registered as a service (start type demand)".into())
      }
      ServiceAction::Uninstall => {
        delete()?;
        // The tray logon task is the other registration this application
        // creates. The installer calls this action on uninstall, and a task
        // that outlives the package would point at a deleted executable.
        let _ = delete_task(&tray_task_name());
        Ok("core service and tray autostart removed".into())
      }
      ServiceAction::Enable => {
        enable_direct(a)?;
        Ok("core starts with the machine".into())
      }
      ServiceAction::Disable => {
        stop_starting_at_boot()?;
        Ok("core no longer starts with the machine".into())
      }
      ServiceAction::Start => {
        // `Start` is also the first-use path on Windows: a fresh installation
        // has no service yet, and falling back to a user-run core would let it
        // own the machine endpoint while looking like the service.
        if service_open(SERVICE_QUERY_STATUS).is_none() {
          install(a)?;
        }
        start()?;
        Ok("core started".into())
      }
      ServiceAction::Stop => {
        stop()?;
        Ok("core stopped".into())
      }
      ServiceAction::Restart => {
        let _ = stop();
        start()?;
        Ok("core restarted".into())
      }
      ServiceAction::Status => status_text(),
    }
  }

  fn needs_admin(action: ServiceAction) -> bool {
    matches!(
      action,
      ServiceAction::Install
        | ServiceAction::Uninstall
        | ServiceAction::Enable
        | ServiceAction::Disable
        | ServiceAction::Start
        | ServiceAction::Stop
        | ServiceAction::Restart
    )
  }

  /// Must match the clap value names, because the elevated copy is started
  /// with them.
  fn action_name(action: ServiceAction) -> &'static str {
    match action {
      ServiceAction::Install => "install",
      ServiceAction::Uninstall => "uninstall",
      ServiceAction::Enable => "enable",
      ServiceAction::Disable => "disable",
      ServiceAction::Start => "start",
      ServiceAction::Stop => "stop",
      ServiceAction::Restart => "restart",
      ServiceAction::Status => "status",
    }
  }

  /// What to report when the work happened in the elevated copy.
  fn already_done(action: ServiceAction) -> String {
    match action {
      ServiceAction::Install => "core registered as a service (start type demand)",
      ServiceAction::Uninstall => "core service and tray autostart removed",
      ServiceAction::Enable => "core starts with the machine",
      ServiceAction::Disable => "core no longer starts with the machine",
      ServiceAction::Start => "core started",
      ServiceAction::Stop => "core stopped",
      ServiceAction::Restart => "core restarted",
      ServiceAction::Status => "status",
    }
    .into()
  }

  fn enable_direct(a: &Autostart) -> Result<(), String> {
    // The service may already be registered (for example by a first click on
    // *Start*). Enabling autostart only needs the start type; re-running the
    // installer path here would rewrite the DACL and ACLs of a live service
    // for no reason.
    if service_open(SERVICE_QUERY_STATUS).is_none() {
      install(a)?;
    }
    sc(&["config", CORE_SERVICE, "start=", "auto"])?;
    Ok(())
  }

  fn stop_starting_at_boot() -> Result<(), String> {
    if service_open(SERVICE_QUERY_STATUS).is_none() {
      return Ok(());
    }
    sc(&["config", CORE_SERVICE, "start=", "demand"])
  }

  /// Create the service (start type `demand`, i.e. installed but not enabled).
  ///
  /// The binary line carries the state directory: the token the SCM uses has no
  /// profile of its own, so `%APPDATA%` there is the system profile. The
  /// settings and the sealed credential stay where the user's window writes
  /// them.
  fn install(a: &Autostart) -> Result<(), String> {
    ensure_machine_dir()?;
    // First close the directory to ordinary users. The service SID does not
    // exist yet on a clean install; it is added after creation. If that second
    // step fails, the service simply cannot read the secret (fail closed).
    harden_machine_dir(None)?;
    let name = wide(CORE_SERVICE);
    let display = wide("THUnet background service");
    let binary = wide(&binary_path(a));
    let account = wide(SERVICE_ACCOUNT);

    if service_open(SERVICE_QUERY_STATUS).is_none() {
      let manager =
        open_manager(SC_MANAGER_CREATE_SERVICE).ok_or("cannot open the service manager")?;
      // SAFETY: every string outlives the call; the returned handle is closed
      // below. The virtual account has no password and no profile.
      let service = unsafe {
        CreateServiceW(
          manager,
          name.as_ptr(),
          display.as_ptr(),
          SERVICE_ALL_ACCESS,
          SERVICE_WIN32_OWN_PROCESS,
          SERVICE_DEMAND_START,
          SERVICE_ERROR_NORMAL,
          binary.as_ptr(),
          std::ptr::null(),
          std::ptr::null_mut(),
          std::ptr::null(),
          account.as_ptr(),
          std::ptr::null(),
        )
      };
      let err = unsafe { GetLastError() };
      unsafe {
        CloseServiceHandle(manager);
      };
      if service.is_null() {
        return Err(format!(
          "cannot create the {CORE_SERVICE} service (error {err})"
        ));
      }
      unsafe {
        CloseServiceHandle(service);
      };
    } else {
      // The service may already exist but point at an older image; rewrite the
      // command line, not only on first creation.
      sc(&["config", CORE_SERVICE, "binPath=", &binary_path(a)])?;
    }

    // The service SID is created with the service; grant it the service DACL,
    // then lock the state directory to that SID and Administrators.
    let _ = sc(&["sidtype", CORE_SERVICE, "unrestricted"]);
    let _ = sc(&["sdset", CORE_SERVICE, &service_sddl()]);
    harden_machine_dir(Some(SERVICE_ACCOUNT))?;

    // Best effort: restart after a crash, and describe the service.
    let _ = sc(&[
      "failure",
      CORE_SERVICE,
      "reset=",
      "86400",
      "actions=",
      "restart/5000/restart/5000/restart/5000",
    ]);
    let _ = sc(&[
      "description",
      CORE_SERVICE,
      "Keeps the THUNet portal session online, starting with the machine.",
    ]);
    Ok(())
  }

  /// `%ProgramData%\thunet`, but never with `%ProgramData%`'s inherited
  /// `Users: read` ACE.
  fn ensure_machine_dir() -> Result<(), String> {
    let dir = crate::config::machine_dir();
    // `%ProgramData%` lets Users create folders; a user could have planted a
    // junction before the installer ran. Refuse to follow one.
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
      use std::os::windows::fs::MetadataExt as _;
      const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
      if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
          "{} is a reparse point; refusing to use it for machine state",
          dir.display()
        ));
      }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(())
  }

  fn harden_machine_dir(service: Option<&str>) -> Result<(), String> {
    let dir = crate::config::machine_dir();
    let service_ace = service.map(|name| format!("{name}:(OI)(CI)F"));
    let mut args = vec![
      "/inheritance:r",
      "/grant:r",
      "*S-1-5-18:(OI)(CI)F",
      "*S-1-5-32-544:(OI)(CI)F",
    ];
    if let Some(ace) = &service_ace {
      args.push(ace);
    }
    args.push("/T");
    let apply = || {
      std::process::Command::new(system32("icacls.exe"))
        .arg(&dir)
        .args(&args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("icacls: {e}"))
    };
    let out = apply()?;
    if out.status.success() {
      return Ok(());
    }

    // A previous install (or a service that ran as another account) may have
    // left files this administrator cannot open for WRITE_DAC. `icacls` then
    // stops at the first such file. Take ownership of the tree and retry; the
    // log is disposable, so drop it first if it is the stubborn one.
    let _ = std::fs::remove_file(dir.join("core.log"));
    let _ = std::process::Command::new(system32("takeown.exe"))
      .arg("/F")
      .arg(&dir)
      .arg("/R")
      .arg("/A")
      .arg("/D")
      .arg("Y")
      .creation_flags(CREATE_NO_WINDOW)
      .output();
    let retry = apply()?;
    if retry.status.success() {
      Ok(())
    } else {
      Err(format!(
        "cannot secure {}: {}",
        dir.display(),
        decode(&retry.stderr).trim()
      ))
    }
  }

  fn service_sddl() -> String {
    match service_sid() {
      Some(sid) => format!("{SERVICE_SDDL}(A;;GA;;;{sid})"),
      None => SERVICE_SDDL.to_string(),
    }
  }

  fn service_sid() -> Option<String> {
    let text = decode(&sc_output(&["showsid", CORE_SERVICE]).ok()?.stdout);
    text
      .split(|c: char| c.is_whitespace() || c == '=' || c == '[' || c == ']')
      .find(|t| t.starts_with("S-1-5-80-"))
      .map(str::to_string)
  }

  fn delete() -> Result<(), String> {
    if service_open(SERVICE_QUERY_STATUS).is_none() {
      return Ok(());
    }
    let _ = stop();
    let service = service_open(DELETE).ok_or("cannot open the service for deletion")?;
    // SAFETY: the handle comes from OpenServiceW and is closed below.
    let ok = unsafe { DeleteService(service) };
    let err = unsafe { GetLastError() };
    unsafe {
      CloseServiceHandle(service);
    }
    if ok != 0 {
      Ok(())
    } else {
      Err(format!("cannot delete the service (error {err})"))
    }
  }

  fn start() -> Result<(), String> {
    let service = service_open(SERVICE_START).ok_or("the core service is not installed")?;
    // SAFETY: the handle is valid and closed below.
    let ok = unsafe { StartServiceW(service, 0, std::ptr::null()) };
    let err = unsafe { GetLastError() };
    unsafe {
      CloseServiceHandle(service);
    }
    if ok != 0 || err == ALREADY_RUNNING {
      return Ok(());
    }
    Err(format!("cannot start the core service (error {err})"))
  }

  fn stop() -> Result<(), String> {
    let service = service_open(SERVICE_STOP | SERVICE_QUERY_STATUS)
      .ok_or("the core service is not installed")?;
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid, and the status outlives the call.
    let ok = unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
    let err = unsafe { GetLastError() };
    if ok == 0 && err != NOT_ACTIVE {
      unsafe {
        CloseServiceHandle(service);
      }
      return Err(format!("cannot stop the core service (error {err})"));
    }
    // Wait for the process to be gone: an installer calls this right before it
    // replaces the binary, and a stop that is only *requested* would leave the
    // exe in use. Five seconds is generous for a process that just exits.
    let mut state = None;
    for _ in 0..50 {
      state = query_state(service);
      if state == Some(SERVICE_STOPPED) {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(100));
    }
    unsafe {
      CloseServiceHandle(service);
    }
    if state == Some(SERVICE_STOPPED) {
      Ok(())
    } else {
      Err("the core service did not stop within five seconds".into())
    }
  }

  fn status_text() -> Result<String, String> {
    let Some(service) = service_open(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) else {
      return Ok(format!("{CORE_SERVICE}: not installed"));
    };
    let state = query_state(service).unwrap_or(0);
    let start = query_start_type(service).unwrap_or(0);
    unsafe {
      CloseServiceHandle(service);
    }
    let state = match state {
      SERVICE_RUNNING => "running",
      SERVICE_STOPPED => "stopped",
      SERVICE_START_PENDING => "starting",
      SERVICE_STOP_PENDING => "stopping",
      _ => "unknown",
    };
    let start = match start {
      SERVICE_AUTO_START => "auto",
      SERVICE_DEMAND_START => "demand",
      _ => "disabled",
    };
    Ok(format!(
      "name               : {CORE_SERVICE}\
       \nstate              : {state}\
       \nstart_type         : {start}"
    ))
  }

  fn query_state(service: SC_HANDLE) -> Option<u32> {
    let mut status: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
    let mut needed = 0u32;
    // SAFETY: the buffer is exactly as large as the size passed, and lives
    // until the call returns.
    let ok = unsafe {
      QueryServiceStatusEx(
        service,
        SC_STATUS_PROCESS_INFO,
        &mut status as *mut _ as *mut u8,
        std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
        &mut needed,
      )
    };
    (ok != 0).then_some(status.dwCurrentState)
  }

  fn query_start_type(service: SC_HANDLE) -> Option<u32> {
    let mut needed = 0u32;
    // The first call only reports how much room the answer needs.
    // SAFETY: a null buffer with size 0 is the documented size query.
    unsafe {
      QueryServiceConfigW(service, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
      return None;
    }
    let mut buffer = vec![0u32; (needed as usize).div_ceil(4)];
    // SAFETY: the buffer is aligned and at least `needed` bytes long.
    let ok = unsafe {
      QueryServiceConfigW(
        service,
        buffer.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW,
        needed,
        &mut needed,
      )
    };
    if ok == 0 {
      return None;
    }
    // SAFETY: a successful call filled the buffer with a QUERY_SERVICE_CONFIGW.
    let config = unsafe { &*(buffer.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    Some(config.dwStartType)
  }

  /// The image path the SCM stores for the core. It spells out the state
  /// directory, because the token the service runs under has no profile of its
  /// own.
  fn binary_path(a: &Autostart) -> String {
    format!(
      "\"{}\" core --service --state-dir \"{}\"",
      a.exe.display(),
      crate::config::machine_dir().display()
    )
  }

  /// Point an existing registration at the executable that is installed now.
  ///
  /// A major upgrade — and a reinstall of the package, which
  /// `AllowSameVersionUpgrades` turns into one — deliberately keeps the
  /// service so that autostart survives, and that also keeps the image path
  /// the previous package wrote. When the new package puts the executable
  /// somewhere else, the SCM would keep launching the old one (or fail), so
  /// the installer's `service-resume` hook rewrites the path first. No-op when
  /// the path already matches or nothing is registered.
  fn sync_image_path(a: &Autostart) -> Result<(), String> {
    let Some(service) = service_open(SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG) else {
      return Ok(());
    };
    let current = query_binary_path(service);
    let wanted = binary_path(a);
    if current.as_deref() == Some(wanted.as_str()) {
      unsafe {
        CloseServiceHandle(service);
      }
      return Ok(());
    }
    let binary = wide(&wanted);
    // SAFETY: the handle is valid, the string outlives the call, and every
    // other field is left as it was by SERVICE_NO_CHANGE.
    let ok = unsafe {
      ChangeServiceConfigW(
        service,
        SERVICE_NO_CHANGE,
        SERVICE_NO_CHANGE,
        SERVICE_NO_CHANGE,
        binary.as_ptr(),
        std::ptr::null(),
        std::ptr::null_mut(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
      )
    };
    let err = unsafe { GetLastError() };
    unsafe {
      CloseServiceHandle(service);
    }
    if ok != 0 {
      Ok(())
    } else {
      Err(format!(
        "cannot point the core service at {} (error {err})",
        a.exe.display()
      ))
    }
  }

  fn query_binary_path(service: SC_HANDLE) -> Option<String> {
    let mut needed = 0u32;
    // The first call only reports how much room the answer needs.
    // SAFETY: a null buffer with size 0 is the documented size query.
    unsafe {
      QueryServiceConfigW(service, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
      return None;
    }
    let mut buffer = vec![0u32; (needed as usize).div_ceil(4)];
    // SAFETY: the buffer is aligned and at least `needed` bytes long.
    let ok = unsafe {
      QueryServiceConfigW(
        service,
        buffer.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW,
        needed,
        &mut needed,
      )
    };
    if ok == 0 {
      return None;
    }
    // SAFETY: a successful call filled the buffer, and `lpBinaryPathName`
    // points inside it at a NUL-terminated string.
    let config = unsafe { &*(buffer.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    let start = config.lpBinaryPathName;
    if start.is_null() {
      return None;
    }
    unsafe {
      let mut len = 0usize;
      while *start.add(len) != 0 {
        len += 1;
      }
      Some(String::from_utf16_lossy(std::slice::from_raw_parts(
        start, len,
      )))
    }
  }

  fn open_manager(access: u32) -> Option<SC_HANDLE> {
    // SAFETY: plain Win32 call; the handle is closed by the caller.
    let handle = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    (!handle.is_null()).then_some(handle)
  }

  fn service_open(access: u32) -> Option<SC_HANDLE> {
    let manager = open_manager(SC_MANAGER_CONNECT)?;
    let name = wide(CORE_SERVICE);
    // SAFETY: the manager handle is valid and the name outlives the call.
    let service = unsafe { OpenServiceW(manager, name.as_ptr(), access) };
    unsafe {
      CloseServiceHandle(manager);
    }
    (!service.is_null()).then_some(service)
  }

  fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
  }

  /// `sc.exe`, used for the operations whose Win32 form takes a dozen
  /// arguments (DACL, failure actions, description, start type).
  fn sc(args: &[&str]) -> Result<(), String> {
    let out = sc_output(args)?;
    if out.status.success() {
      return Ok(());
    }
    let err = decode(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
      format!("sc exited with {:?}", out.status.code())
    } else {
      err
    })
  }

  fn sc_output(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new(system32("sc.exe"))
      .args(args)
      .creation_flags(CREATE_NO_WINDOW)
      .output()
      .map_err(|e| format!("sc: {e}"))
  }

  fn delete_task(name: &str) -> Result<(), String> {
    if !task_exists(name) {
      return Ok(());
    }
    expect_success(&["/Delete", "/TN", name, "/F"])
  }

  /// `schtasks /XML` and `sc` print UTF-16 with a BOM on some consoles and
  /// plain (OEM/UTF-8) on others; accept either.
  fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
      let units: Vec<u16> = bytes[2..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
      String::from_utf16_lossy(&units)
    } else {
      String::from_utf8_lossy(bytes).into_owned()
    }
  }

  fn register(exe: &Path, args: &[&str], name: &str) -> Result<(), String> {
    let user = current_user()?;
    let xml = task_xml(exe, args, &user);
    let path = std::env::temp_dir().join(format!("thunet-autostart-{}.xml", std::process::id()));
    write_utf16(&path, &xml)?;
    let result = expect_success(&[
      "/Create",
      "/TN",
      name,
      "/XML",
      &path.to_string_lossy(),
      "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    result
  }

  fn task_exists(name: &str) -> bool {
    run(&["/Query", "/TN", name])
      .map(|out| out.status.success())
      .unwrap_or(false)
  }

  /// `Err` only when `schtasks.exe` cannot be started at all; a non-zero exit
  /// status is returned as `Ok` so callers can interpret it.
  fn run(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new(system32("schtasks.exe"))
      .args(args)
      .creation_flags(CREATE_NO_WINDOW)
      .output()
      .map_err(|e| format!("schtasks: {e}"))
  }

  fn expect_success(args: &[&str]) -> Result<(), String> {
    let out = run(args)?;
    if out.status.success() {
      return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
      format!("schtasks exited with {:?}", out.status.code())
    } else {
      err
    })
  }

  /// Full path so a hijacked `PATH` cannot redirect the call.
  fn system32(exe: &str) -> std::ffi::OsString {
    std::env::var_os("SystemRoot")
      .map(|root| Path::new(&root).join("System32").join(exe).into_os_string())
      .unwrap_or_else(|| exe.into())
  }

  /// The task runs as its owner, so the account must be spelled out; the
  /// domain is the machine name for local accounts and the tenant for AAD ones.
  fn current_user() -> Result<String, String> {
    let name = std::env::var("USERNAME").map_err(|_| "USERNAME is not set".to_string())?;
    Ok(match std::env::var("USERDOMAIN") {
      Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
      _ => name,
    })
  }

  /// `schtasks /Create /XML` wants a Unicode file with a BOM.
  fn write_utf16(path: &Path, text: &str) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in text.encode_utf16() {
      bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
  }

  /// A logon task with no delay: same trigger the other login-time clients
  /// use, so the app is scheduled alongside them rather than after the shell.
  fn task_xml(exe: &Path, args: &[&str], user: &str) -> String {
    let args = args.join(" ");
    let args = if args.is_empty() {
      String::new()
    } else {
      format!("      <Arguments>{}</Arguments>\n", esc(&args))
    };
    format!(
      concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n",
        "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n",
        "  <RegistrationInfo>\n",
        "    <Description>THUnet autostart</Description>\n",
        "  </RegistrationInfo>\n",
        "  <Triggers>\n",
        "    <LogonTrigger>\n",
        "      <Enabled>true</Enabled>\n",
        "      <UserId>{user}</UserId>\n",
        "    </LogonTrigger>\n",
        "  </Triggers>\n",
        "  <Principals>\n",
        "    <Principal id=\"Author\">\n",
        "      <UserId>{user}</UserId>\n",
        "      <LogonType>InteractiveToken</LogonType>\n",
        "      <RunLevel>LeastPrivilege</RunLevel>\n",
        "    </Principal>\n",
        "  </Principals>\n",
        "  <Settings>\n",
        "    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n",
        "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n",
        "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n",
        "    <AllowHardTerminate>true</AllowHardTerminate>\n",
        "    <StartWhenAvailable>false</StartWhenAvailable>\n",
        "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n",
        "    <IdleSettings>\n",
        "      <StopOnIdleEnd>false</StopOnIdleEnd>\n",
        "      <RestartOnIdle>false</RestartOnIdle>\n",
        "    </IdleSettings>\n",
        "    <AllowStartOnDemand>true</AllowStartOnDemand>\n",
        "    <Enabled>true</Enabled>\n",
        "    <Hidden>false</Hidden>\n",
        "    <RunOnlyIfIdle>false</RunOnlyIfIdle>\n",
        "    <WakeToRun>false</WakeToRun>\n",
        "    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n",
        "    <Priority>7</Priority>\n",
        "  </Settings>\n",
        "  <Actions Context=\"Author\">\n",
        "    <Exec>\n",
        "      <Command>{exe}</Command>\n",
        "{args}",
        "    </Exec>\n",
        "  </Actions>\n",
        "</Task>\n"
      ),
      user = esc(user),
      exe = esc(&exe.to_string_lossy()),
      args = args,
    )
  }

  fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
      .replace('<', "&lt;")
      .replace('>', "&gt;")
  }

  #[cfg(test)]
  mod tests {
    use super::{esc, task_xml};
    use std::path::Path;

    #[test]
    fn xml_carries_the_command_line() {
      let xml = task_xml(
        Path::new(r"C:\Tools\thunet.exe"),
        &["--minimized"],
        "PC\\me",
      );
      assert!(xml.contains("<Command>C:\\Tools\\thunet.exe</Command>"));
      assert!(xml.contains("<Arguments>--minimized</Arguments>"));
      assert!(xml.contains("<UserId>PC\\me</UserId>"));
    }

    #[test]
    fn xml_omits_empty_arguments() {
      let xml = task_xml(Path::new(r"C:\Tools\thunet.exe"), &[], "PC\\me");
      assert!(!xml.contains("<Arguments>"));
    }

    #[test]
    fn xml_escapes_markup() {
      assert_eq!(esc("a&b<c>d"), "a&amp;b&lt;c&gt;d");
      let xml = task_xml(Path::new(r"C:\a&b\thunet.exe"), &[], "PC\\me");
      assert!(xml.contains(r"<Command>C:\a&amp;b\thunet.exe</Command>"));
    }
  }
}

#[cfg(not(windows))]
mod imp {
  use std::path::PathBuf;

  use auto_launch::{AutoLaunch, AutoLaunchBuilder};

  use super::{Autostart, ServiceAction};

  /// Name of the XDG desktop entry (`auto-launch` writes `thunet.desktop`).
  const APP_NAME: &str = "thunet";

  /// The background core as a systemd **system** unit. It needs no display, so
  /// it can start with the machine, before anybody logs in, and it is shared by
  /// every account.
  ///
  /// Named `thunet.service` so `systemctl start|stop|status thunet` works
  /// without the suffix.
  const CORE_UNIT: &str = "thunet.service";

  fn launcher(a: &Autostart) -> Result<AutoLaunch, String> {
    AutoLaunchBuilder::new()
      .set_app_name(APP_NAME)
      .set_app_path(&a.exe.to_string_lossy())
      .set_args(a.args())
      .build()
      .map_err(|e| e.to_string())
  }

  pub fn tray_registered(a: &Autostart) -> bool {
    launcher(a)
      .and_then(|l| l.is_enabled().map_err(|e| e.to_string()))
      .unwrap_or(false)
  }

  pub fn core_registered() -> bool {
    unit_present()
  }

  /// Whether the system manager starts it at boot.
  pub fn core_starts_at_boot() -> bool {
    unit_enabled()
  }

  /// The tray keeps using the XDG autostart entry: every desktop honours it,
  /// including the ones that never pull `graphical-session.target` (i3, sway).
  pub fn enable_tray(a: &Autostart) -> Result<(), String> {
    launcher(a)?.enable().map_err(|e| e.to_string())
  }

  pub fn disable_tray(a: &Autostart) -> Result<(), String> {
    launcher(a)?.disable().map_err(|e| e.to_string())
  }

  /// Write the unit (if needed) and put it in the boot path. Both steps need
  /// administrator rights; `Autostart::service` elevates before calling this.
  pub fn enable_core(a: &Autostart) -> Result<(), String> {
    if !unit_present() {
      install_unit(a)?;
    }
    enable_unit()
  }

  /// Stop starting the core automatically. A running core keeps running: the
  /// switch is about autostart, not about killing the session.
  pub fn disable_core() -> Result<(), String> {
    disable_unit()
  }

  /// The tray entry is the only one `--minimized` affects.
  pub fn refresh_tray(a: &Autostart) -> Result<(), String> {
    if tray_registered(a) {
      enable_tray(a)
    } else {
      Ok(())
    }
  }

  /// Nothing to resume by hand: systemd applies `Restart=` and the enabled
  /// unit starts at the next login.
  pub fn resume_after_update() -> Result<(), String> {
    Ok(())
  }

  /// The `.deb` removes `/var/lib/thunet` from `postrm` on a real uninstall, so
  /// this entry point has nothing to do here.
  pub fn purge_machine_state() -> Result<(), String> {
    Ok(())
  }

  /// The package ships the unit. A portable install uses
  /// `thunet service install`, which needs administrator rights.
  pub fn prepare_service(_a: &Autostart) -> Result<bool, String> {
    Ok(false)
  }

  pub fn service(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    if needs_admin(action) && !crate::elevate::is_elevated() {
      elevate(action_name(action))?;
      return Ok(already_done(action));
    }
    service_direct(action, a)
  }

  fn needs_admin(action: ServiceAction) -> bool {
    !matches!(action, ServiceAction::Status)
  }

  fn action_name(action: ServiceAction) -> &'static str {
    match action {
      ServiceAction::Install => "install",
      ServiceAction::Uninstall => "uninstall",
      ServiceAction::Enable => "enable",
      ServiceAction::Disable => "disable",
      ServiceAction::Start => "start",
      ServiceAction::Stop => "stop",
      ServiceAction::Restart => "restart",
      ServiceAction::Status => "status",
    }
  }

  fn already_done(action: ServiceAction) -> String {
    match action {
      ServiceAction::Install => format!("installed {CORE_UNIT} (not enabled)"),
      ServiceAction::Uninstall => format!("removed {CORE_UNIT}"),
      ServiceAction::Enable => "autostart enabled for the core".into(),
      ServiceAction::Disable => "autostart disabled for the core".into(),
      ServiceAction::Start => "core started".into(),
      ServiceAction::Stop => "core stopped".into(),
      ServiceAction::Restart => "core restarted".into(),
      ServiceAction::Status => "status".into(),
    }
  }

  fn elevate(action: &str) -> Result<(), String> {
    let dir = crate::config::machine_dir();
    crate::elevate::run_as_admin(&[
      "service-admin",
      action,
      "--state-dir",
      &dir.to_string_lossy(),
    ])
  }

  pub fn service_direct(action: ServiceAction, a: &Autostart) -> Result<String, String> {
    match action {
      ServiceAction::Install => {
        install_unit(a)?;
        Ok(format!("installed {CORE_UNIT} (not enabled)"))
      }
      ServiceAction::Uninstall => {
        remove_unit()?;
        Ok(format!("removed {CORE_UNIT}"))
      }
      ServiceAction::Enable => {
        enable_core(a)?;
        Ok("autostart enabled for the core".into())
      }
      ServiceAction::Disable => {
        disable_core()?;
        Ok("autostart disabled for the core".into())
      }
      ServiceAction::Start => {
        systemctl(&["start", CORE_UNIT])?;
        Ok("core started".into())
      }
      ServiceAction::Stop => {
        systemctl(&["stop", CORE_UNIT])?;
        Ok("core stopped".into())
      }
      ServiceAction::Restart => {
        systemctl(&["restart", CORE_UNIT])?;
        Ok("core restarted".into())
      }
      ServiceAction::Status => Ok(systemctl_output(&["status", "--no-pager", CORE_UNIT])),
    }
  }

  #[cfg(target_os = "linux")]
  fn unit_path() -> Option<PathBuf> {
    Some(PathBuf::from("/etc/systemd/system").join(CORE_UNIT))
  }

  #[cfg(target_os = "linux")]
  fn package_unit_path() -> PathBuf {
    PathBuf::from("/usr/lib/systemd/system").join(CORE_UNIT)
  }

  #[cfg(target_os = "linux")]
  fn unit_text(a: &Autostart) -> String {
    format!(
      "[Unit]\n\
       Description=THUnet campus network client (background core)\n\
       After=network-online.target\n\
       Wants=network-online.target\n\
       \n\
       [Service]\n\
       Type=simple\n\
       DynamicUser=yes\n\
       StateDirectory=thunet\n\
       StateDirectoryMode=0700\n\
       UMask=0077\n\
       ExecStart=\"{}\" core --machine\n\
       Restart=always\n\
       RestartSec=3\n\
       NoNewPrivileges=true\n\
       PrivateTmp=true\n\
       ProtectHome=true\n\
       ProtectSystem=strict\n\
       RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n\
       CapabilityBoundingSet=\n\
       LimitCORE=0\n\
       \n\
       [Install]\n\
       WantedBy=multi-user.target\n",
      a.exe.display()
    )
  }

  #[cfg(target_os = "linux")]
  fn unit_present() -> bool {
    unit_path().is_some_and(|p| p.exists()) || package_unit_path().exists()
  }

  #[cfg(target_os = "linux")]
  fn install_unit(a: &Autostart) -> Result<(), String> {
    let path = unit_path().ok_or("cannot locate /etc/systemd/system")?;
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = unit_text(a);
    // Rewriting an unchanged unit would trigger a reload for nothing.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
      std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
      let _ = systemctl(&["daemon-reload"]);
    }
    Ok(())
  }

  #[cfg(target_os = "linux")]
  fn unit_enabled() -> bool {
    systemctl(&["is-enabled", "--quiet", CORE_UNIT]).is_ok()
  }

  #[cfg(target_os = "linux")]
  fn enable_unit() -> Result<(), String> {
    // Deliberately not `--now`: this switch is about what happens at the next
    // boot, exactly like the Windows service start type.
    systemctl(&["enable", CORE_UNIT])
  }

  #[cfg(target_os = "linux")]
  fn disable_unit() -> Result<(), String> {
    systemctl(&["disable", CORE_UNIT])
  }

  #[cfg(target_os = "linux")]
  fn remove_unit() -> Result<(), String> {
    let _ = systemctl(&["disable", "--now", CORE_UNIT]);
    if let Some(path) = unit_path() {
      let _ = std::fs::remove_file(path);
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
  }

  #[cfg(target_os = "linux")]
  fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
      .args(args)
      .output()
      .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
      Ok(())
    } else {
      Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
  }

  /// Whatever `systemctl` printed, for `thunet service status`. An inactive
  /// unit exits non-zero, which is information rather than an error here.
  #[cfg(target_os = "linux")]
  fn systemctl_output(args: &[&str]) -> String {
    match std::process::Command::new("systemctl").args(args).output() {
      Ok(out) => {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        text.trim_end().to_string()
      }
      Err(e) => format!("systemctl: {e}"),
    }
  }

  #[cfg(not(target_os = "linux"))]
  fn unit_present() -> bool {
    false
  }

  #[cfg(not(target_os = "linux"))]
  fn systemctl(_args: &[&str]) -> Result<(), String> {
    Err("this platform has no system service manager".into())
  }

  #[cfg(not(target_os = "linux"))]
  fn systemctl_output(_args: &[&str]) -> String {
    "this platform has no system service manager".into()
  }

  /// macOS: there is no service manager for the core (a LaunchAgent has no
  /// display-less equivalent), so it is started on demand — the status page's
  /// *Start* button or `thunet core` / `thunet service start`.
  #[cfg(not(target_os = "linux"))]
  fn install_unit(_a: &Autostart) -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn unit_enabled() -> bool {
    // The LaunchAgent for the tray is what starts the core, so "core managed"
    // is not a separate thing here.
    false
  }

  #[cfg(not(target_os = "linux"))]
  fn enable_unit() -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn disable_unit() -> Result<(), String> {
    Ok(())
  }

  #[cfg(not(target_os = "linux"))]
  fn remove_unit() -> Result<(), String> {
    Ok(())
  }
}
