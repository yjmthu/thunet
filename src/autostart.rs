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
  // The elevated helper has no console, and an installer looks at its exit
  // code alone: a failure has to leave a reason in the log the service writes.
  // The machine directory is used rather than the user one because the MSI
  // runs this as SYSTEM (`Impersonate="no"`), whose `%APPDATA%` is the system
  // profile — a log nobody can find.
  #[cfg(windows)]
  crate::diag::set_file(crate::config::machine_dir().join("core.log"));
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
  // A real uninstall deletes the service before this runs (`RemoveCoreService`
  // is sequenced ahead of `PurgeMachineState`, and both are conditioned on
  // `REMOVE="ALL"`). If the service is still registered, this call is an
  // upgrade or a mistake: the sealed credential and the key matter more than a
  // tidy directory, so refuse instead of deleting them.
  if core_managed() {
    crate::diag::line("service-purge: refused, the core service is still registered");
    return Ok(());
  }
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
  use std::time::{Duration, Instant};

  use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
  use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
    QueryServiceConfigW, QueryServiceStatusEx, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_STOP,
    SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_SID_INFO,
    SERVICE_SID_TYPE_UNRESTRICTED, SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_WIN32_OWN_PROCESS, SetServiceObjectSecurity, StartServiceW,
  };
  use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

  use super::{Autostart, ServiceAction};

  /// Service Control Manager key of the background core. The same name as the
  /// Linux unit (`thunet.service`), so the mental model is identical on both
  /// platforms: `thunet` *is* the service, `thunet-tray-<user>` is the frontend.
  const CORE_SERVICE: &str = "thunet";
  /// Shown by the Services snap-in and nested under the process row in Task
  /// Manager. The image's own description cannot differ per half - both halves
  /// are the same exe - so this is what names the service.
  const SERVICE_DISPLAY: &str = "THUnet Background Service";
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
  /// Standard access right `SYNCHRONIZE` (wait for the process to end).
  const SYNCHRONIZE: u32 = 0x0010_0000;
  /// `ERROR_SERVICE_ALREADY_RUNNING` / `ERROR_SERVICE_NOT_ACTIVE`.
  const ALREADY_RUNNING: u32 = 1056;
  const NOT_ACTIVE: u32 = 1062;
  /// How long a start waits for `RUNNING` and a stop for the process to end.
  const START_WAIT: Duration = Duration::from_secs(10);
  const STOP_WAIT: Duration = Duration::from_secs(15);
  /// Poll interval while waiting on the SCM.
  const POLL: Duration = Duration::from_millis(100);

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
        // Succeeds when no service is registered: the installer calls this on
        // every install, including the first one, where there is nothing to
        // stop. Only a registered service that refuses to stop is an error, and
        // the package turns that into an aborted install.
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
    set_start_type(SERVICE_AUTO_START)
  }

  fn stop_starting_at_boot() -> Result<(), String> {
    if service_open(SERVICE_QUERY_STATUS).is_none() {
      return Ok(());
    }
    set_start_type(SERVICE_DEMAND_START)
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

    // Create or open the registration, then configure it through the handle.
    // Nothing here shells out to `sc.exe`: a console program started by this
    // GUI process inherits no console, so Windows gives it one, and the user
    // sees a black window flash on every click.
    let wanted = binary_path(a);
    let service = match service_open(SERVICE_ALL_ACCESS) {
      Some(existing) => {
        // The service may already exist but point at an older image; rewrite
        // the command line, not only on first creation.
        set_binary_path(existing, &wanted)?;
        existing
      }
      None => create_service(&wanted)?,
    };

    // The service SID is created with the service; grant it the service DACL,
    // then lock the state directory to that SID and Administrators.
    if let Err(e) = set_sid_type(service) {
      crate::diag::line(&format!("service: cannot unrestrict the service SID: {e}"));
    }
    if let Err(e) = set_service_dacl(service, &service_sddl()) {
      crate::diag::line(&format!("service: cannot set the service DACL: {e}"));
    }
    unsafe {
      CloseServiceHandle(service);
    }
    harden_machine_dir(Some(SERVICE_ACCOUNT))?;

    // Best effort: restart after a crash, and describe the service.
    if let Ok(service) = open_for_config() {
      if let Err(e) = set_failure_actions(service) {
        crate::diag::line(&format!("service: cannot set failure actions: {e}"));
      }
      if let Err(e) = set_description(
        service,
        "Keeps the THUNet portal session online, starting with the machine.",
      ) {
        crate::diag::line(&format!("service: cannot set the description: {e}"));
      }
      unsafe {
        CloseServiceHandle(service);
      }
    }
    Ok(())
  }

  /// Create the service with the start type `demand` (installed, not enabled).
  ///
  /// The binary line carries the state directory: the token the SCM uses has no
  /// profile of its own, so `%APPDATA%` there is the system profile. The
  /// settings and the sealed credential stay where the user's window writes
  /// them. The virtual account has no password and no profile.
  fn create_service(binary: &str) -> Result<SC_HANDLE, String> {
    let manager =
      open_manager(SC_MANAGER_CREATE_SERVICE).ok_or("cannot open the service manager")?;
    let name = wide(CORE_SERVICE);
    let display = wide(SERVICE_DISPLAY);
    let binary = wide(binary);
    let account = wide(SERVICE_ACCOUNT);
    // SAFETY: every string outlives the call; the returned handle is closed
    // below and by the caller.
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
    Ok(service)
  }

  /// A handle allowed to change the configuration, the DACL and the optional
  /// settings of the registered service.
  ///
  /// `SERVICE_ALL_ACCESS` rather than just `SERVICE_CHANGE_CONFIG`:
  /// `ChangeServiceConfig2` answers `ERROR_ACCESS_DENIED` for the failure
  /// actions with only the change right, while the handle `CreateServiceW`
  /// returns (which always has full access) accepts them.
  fn open_for_config() -> Result<SC_HANDLE, String> {
    service_open(SERVICE_ALL_ACCESS)
      .ok_or_else(|| format!("cannot open the {CORE_SERVICE} service for changes"))
  }

  /// `sc config start= <type>` without `sc.exe`.
  fn set_start_type(start: u32) -> Result<(), String> {
    let service = open_for_config()?;
    // SAFETY: the handle is valid; a null binary path leaves the stored one.
    let ok = unsafe {
      ChangeServiceConfigW(
        service,
        SERVICE_NO_CHANGE,
        start,
        SERVICE_NO_CHANGE,
        std::ptr::null(),
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
      Err(format!("cannot change the start type (error {err})"))
    }
  }

  /// `sc config binPath=` without `sc.exe`.
  fn set_binary_path(service: SC_HANDLE, path: &str) -> Result<(), String> {
    let binary = wide(path);
    // The display name is rewritten too: an existing registration keeps what it
    // was created with, so a renamed product would otherwise keep the old
    // wording on every upgraded machine.
    let display = wide(SERVICE_DISPLAY);
    // SAFETY: the handle is valid and the strings outlive the call; every
    // other field keeps its stored value (SERVICE_NO_CHANGE).
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
        display.as_ptr(),
      )
    };
    let err = unsafe { GetLastError() };
    if ok != 0 {
      Ok(())
    } else {
      Err(format!("cannot set the service image path (error {err})"))
    }
  }

  /// `sc sidtype … unrestricted`: the virtual account gets its own SID instead
  /// of the process-wide service SID.
  fn set_sid_type(service: SC_HANDLE) -> Result<(), String> {
    let info = SERVICE_SID_INFO {
      dwServiceSidType: SERVICE_SID_TYPE_UNRESTRICTED,
    };
    change_config2(
      service,
      SERVICE_CONFIG_SERVICE_SID_INFO,
      &info as *const _ as *const std::ffi::c_void,
    )
  }

  /// Restart the core three times, five seconds apart, and reset the count
  /// after a day (`sc failure … restart/5000/…`).
  fn set_failure_actions(service: SC_HANDLE) -> Result<(), String> {
    let mut actions = [SC_ACTION {
      Type: SC_ACTION_RESTART,
      Delay: 5_000,
    }; 3];
    let info = SERVICE_FAILURE_ACTIONSW {
      dwResetPeriod: 86_400,
      lpRebootMsg: std::ptr::null_mut(),
      lpCommand: std::ptr::null_mut(),
      cActions: actions.len() as u32,
      lpsaActions: actions.as_mut_ptr(),
    };
    change_config2(
      service,
      SERVICE_CONFIG_FAILURE_ACTIONS,
      &info as *const _ as *const std::ffi::c_void,
    )
  }

  fn set_description(service: SC_HANDLE, text: &str) -> Result<(), String> {
    let mut text = wide(text);
    let info = SERVICE_DESCRIPTIONW {
      lpDescription: text.as_mut_ptr(),
    };
    change_config2(
      service,
      SERVICE_CONFIG_DESCRIPTION,
      &info as *const _ as *const std::ffi::c_void,
    )
  }

  fn change_config2(
    service: SC_HANDLE,
    level: u32,
    info: *const std::ffi::c_void,
  ) -> Result<(), String> {
    // SAFETY: the handle is valid and `info` points at the structure the level
    // asks for, which outlives the call.
    let ok = unsafe { ChangeServiceConfig2W(service, level, info) };
    let err = unsafe { GetLastError() };
    if ok != 0 {
      Ok(())
    } else {
      Err(format!("ChangeServiceConfig2 failed (error {err})"))
    }
  }

  /// `sc sdset`: apply the SDDL DACL to the service object itself.
  fn set_service_dacl(service: SC_HANDLE, sddl: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
      ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let text = wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the SDDL string outlives the call; the descriptor is released
    // below.
    let ok = unsafe {
      ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text.as_ptr(),
        SDDL_REVISION_1,
        &mut descriptor,
        std::ptr::null_mut(),
      )
    };
    if ok == 0 || descriptor.is_null() {
      let err = unsafe { GetLastError() };
      return Err(format!("cannot parse the service DACL (error {err})"));
    }
    // SAFETY: the handle and the descriptor are valid; the descriptor is freed
    // afterwards.
    let set = unsafe { SetServiceObjectSecurity(service, DACL_SECURITY_INFORMATION, descriptor) };
    let err = unsafe { GetLastError() };
    unsafe {
      LocalFree(descriptor as _);
    }
    if set != 0 {
      Ok(())
    } else {
      Err(format!("cannot set the service DACL (error {err})"))
    }
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

  /// Give the machine state directory, and everything already in it, an
  /// explicit DACL for SYSTEM, administrators and the service account.
  ///
  /// Two passes on purpose. The first *replaces* the directory's DACL with
  /// inheritable grants; the second only *adds* plain grants to the tree. A
  /// replaced DACL on a **file** is what once left `key.bin` and
  /// `credential.bin` with no entries at all — inheritance flags are
  /// meaningless on a file, so icacls dropped the grants and the resulting
  /// empty DACL denied every account, SYSTEM included. The service then could
  /// not open the very credential it had sealed.
  fn harden_machine_dir(service: Option<&str>) -> Result<(), String> {
    let dir = crate::config::machine_dir();
    let mut dir_args = vec![
      "/inheritance:r".to_string(),
      "/grant:r".to_string(),
      "*S-1-5-18:(OI)(CI)F".to_string(),
      "*S-1-5-32-544:(OI)(CI)F".to_string(),
    ];
    let mut tree_args = vec![
      "/grant".to_string(),
      "*S-1-5-18:F".to_string(),
      "*S-1-5-32-544:F".to_string(),
    ];
    if let Some(name) = service {
      dir_args.push(format!("{name}:(OI)(CI)F"));
      tree_args.push(format!("{name}:F"));
    }
    // Only the tree pass recurses; the directory pass must not, or its
    // replace operation would reach the files again.
    let mut recursive = tree_args.clone();
    recursive.push("/T".to_string());

    let apply = |args: &[String]| {
      std::process::Command::new(system32("icacls.exe"))
        .arg(&dir)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("icacls: {e}"))
    };
    let ok = |args: &[String]| -> Result<bool, String> { Ok(apply(args)?.status.success()) };
    if ok(&dir_args)? && ok(&recursive)? {
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
    let dir_done = ok(&dir_args)?;
    let tree_done = ok(&recursive)?;
    if dir_done && tree_done {
      return Ok(());
    }
    let out = apply(&recursive)?;
    Err(format!(
      "cannot secure {}: {}",
      dir.display(),
      decode(&out.stderr).trim()
    ))
  }

  fn service_sddl() -> String {
    match service_sid() {
      Some(sid) => format!("{SERVICE_SDDL}(A;;GA;;;{sid})"),
      None => SERVICE_SDDL.to_string(),
    }
  }

  fn service_sid() -> Option<String> {
    // The account exists once the service does; resolving it needs no `sc
    // showsid`, which would be another console program to spawn (and whose
    // output format is localized).
    crate::ident::service_sid_strings().into_iter().next()
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
    let Some(service) = service_open(SERVICE_START | SERVICE_QUERY_STATUS) else {
      return Err("the core service is not installed".into());
    };
    // SAFETY: the handle is valid and closed below.
    let ok = unsafe { StartServiceW(service, 0, std::ptr::null()) };
    let err = unsafe { GetLastError() };
    if ok == 0 && err != ALREADY_RUNNING {
      unsafe {
        CloseServiceHandle(service);
      }
      return Err(format!("cannot start the core service (error {err})"));
    }
    // The SCM starts the process asynchronously: returning the moment the
    // request is accepted would let the caller report "started" for a service
    // that is still coming up (or never does). Wait for RUNNING and leave the
    // rest to the endpoint check the window already does.
    let running = wait_until(START_WAIT, || query_state(service) == Some(SERVICE_RUNNING));
    unsafe {
      CloseServiceHandle(service);
    }
    if running {
      Ok(())
    } else {
      let e = format!(
        "the core service did not reach RUNNING within {} s",
        START_WAIT.as_secs()
      );
      crate::diag::line(&format!("service: {e}"));
      Err(e)
    }
  }

  fn stop() -> Result<(), String> {
    // "Not installed" is not a failure: a first install has nothing to stop,
    // and the MSI turns a failure here into an aborted install. A service that
    // exists but will not stop, on the other hand, must fail loudly — replacing
    // the binary under a running image is exactly how a half-upgraded machine
    // is left behind.
    let Some(service) = service_open(SERVICE_STOP | SERVICE_QUERY_STATUS) else {
      return Ok(());
    };
    // Remember the process before it goes away: the SCM reports STOPPED as soon
    // as the service's control handler returns, and the image is still mapped
    // for a moment after that. An installer that trusts the state alone can
    // replace a file that is still open.
    let pid = query_status(service).map_or(0, |s| s.dwProcessId);
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid, and the status outlives the call.
    let ok = unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) };
    let err = unsafe { GetLastError() };
    if ok == 0 && err != NOT_ACTIVE {
      unsafe {
        CloseServiceHandle(service);
      }
      let e = format!("cannot stop the core service (error {err})");
      crate::diag::line(&format!("service: {e}"));
      return Err(e);
    }
    // Fifteen seconds covers a core caught in the middle of an HTTP request.
    let deadline = Instant::now() + STOP_WAIT;
    let stopped = wait_until_at(deadline, || query_state(service) == Some(SERVICE_STOPPED));
    unsafe {
      CloseServiceHandle(service);
    }
    if !stopped {
      let e = format!(
        "the core service did not stop within {} s",
        STOP_WAIT.as_secs()
      );
      crate::diag::line(&format!("service: {e}"));
      return Err(e);
    }
    // STOPPED is the SCM's bookkeeping; the process is what holds the file.
    if pid != 0 && !wait_process_exit(pid, deadline.saturating_duration_since(Instant::now())) {
      let e = format!("the core process {pid} is still running after the stop");
      crate::diag::line(&format!("service: {e}"));
      return Err(e);
    }
    Ok(())
  }

  /// Poll `check` until it is true or `wait` elapses.
  fn wait_until(wait: Duration, mut check: impl FnMut() -> bool) -> bool {
    wait_until_at(Instant::now() + wait, &mut check)
  }

  /// [`wait_until`] with a deadline that a second wait can share.
  fn wait_until_at(deadline: Instant, mut check: impl FnMut() -> bool) -> bool {
    loop {
      if check() {
        return true;
      }
      if Instant::now() >= deadline {
        return false;
      }
      std::thread::sleep(POLL);
    }
  }

  /// Wait until the process object is gone.
  ///
  /// `OpenProcess` failing is the answer here: it means no process with that
  /// id exists any more (or it can never be opened, which for the core's own
  /// pid would be a different problem).
  fn wait_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
      // SAFETY: plain call; a successful handle is closed right away.
      let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
      if handle.is_null() {
        return true;
      }
      let left = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u32::MAX as u128) as u32;
      // SAFETY: the handle came from OpenProcess and is closed below.
      let rc = unsafe { WaitForSingleObject(handle, left) };
      unsafe {
        CloseHandle(handle);
      };
      if rc == WAIT_OBJECT_0 {
        return true;
      }
      if Instant::now() >= deadline {
        return false;
      }
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
    query_status(service).map(|status| status.dwCurrentState)
  }

  fn query_status(service: SC_HANDLE) -> Option<SERVICE_STATUS_PROCESS> {
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
    ok.ne(&0).then_some(status)
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
    if current.as_deref() != Some(wanted.as_str()) {
      crate::diag::line(&format!("service: image path {current:?} -> {wanted}"));
    }
    // Applied even when the path already matches: the display name travels with
    // this call, and a renamed product has to reach upgraded machines too.
    let result = set_binary_path(service, &wanted);
    unsafe {
      CloseServiceHandle(service);
    }
    result.map_err(|e| format!("{e} (pointing the core service at {})", a.exe.display()))
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
