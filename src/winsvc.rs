//! `thunet core --service`: the core as a Windows service.
//!
//! A logon task cannot run before somebody logs on; the Service Control Manager
//! can, because it starts the process at boot with its own (LocalSystem) token.
//! That is what lets the portal session be up by the time the login screen
//! appears. The process itself is unchanged: it binds the control pipe and runs
//! the same session loop as `thunet core`.
//!
//! Nothing else in the program uses this path. `sc create` (done by the
//! elevated helper, see `autostart.rs`) registers the executable with the
//! `core --service` arguments; the SCM starts it, reports its state, and asks
//! it to stop through the handler below.

use std::ffi::OsString;
use std::time::Duration;

use windows_service::service::{
  ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

/// SCM key name. Same name as the Linux unit and the old task, so the mental
/// model is unchanged: `thunet` *is* the service.
pub const SERVICE_NAME: &str = "thunet";

define_windows_service!(ffi_service_main, service_main);

/// Hand control to the SCM. Returns an error when the process was not started
/// by it (running `thunet core --service` by hand), which is the right outcome.
pub fn run() -> Result<(), String> {
  service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    .map_err(|e| format!("cannot become a service: {e}"))
}

fn service_main(_args: Vec<OsString>) {
  if let Err(e) = body() {
    crate::diag::line(&format!("service: {e}"));
    std::process::exit(1);
  }
}

/// The SCM's status handle, kept so a shutdown that starts in the session
/// worker (`Cmd::Shutdown`) can still report a clean stop.
static STATUS: std::sync::OnceLock<StatusHandle> = std::sync::OnceLock::new();

struct StatusHandle(windows_service::service_control_handler::ServiceStatusHandle);

// SAFETY: `ServiceStatusHandle` wraps a plain Win32 `SERVICE_STATUS_HANDLE`
// (a raw pointer, hence neither `Send` nor `Sync` by itself). It is only used
// through `set_service_status`, which is a thread-safe call on a handle that is
// valid for the life of the process.
unsafe impl Send for StatusHandle {}
unsafe impl Sync for StatusHandle {}

fn stopped() -> ServiceStatus {
  ServiceStatus {
    service_type: ServiceType::OWN_PROCESS,
    current_state: ServiceState::Stopped,
    controls_accepted: ServiceControlAccept::empty(),
    exit_code: ServiceExitCode::Win32(0),
    checkpoint: 0,
    wait_hint: Duration::default(),
    process_id: None,
  }
}

/// Report a clean stop before the process ends; a no-op outside a service.
pub fn report_stopped() {
  if let Some(handle) = STATUS.get() {
    let _ = handle.0.set_service_status(stopped());
  }
}

fn body() -> Result<(), String> {
  let (stop_tx, stop_rx) = std::sync::mpsc::channel();
  let handler = move |control| match control {
    ServiceControl::Stop | ServiceControl::Shutdown => {
      let _ = stop_tx.send(());
      ServiceControlHandlerResult::NoError
    }
    ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
    _ => ServiceControlHandlerResult::NotImplemented,
  };
  let status =
    service_control_handler::register(SERVICE_NAME, handler).map_err(|e| e.to_string())?;

  let running = ServiceStatus {
    service_type: ServiceType::OWN_PROCESS,
    current_state: ServiceState::Running,
    controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    exit_code: ServiceExitCode::Win32(0),
    checkpoint: 0,
    wait_hint: Duration::default(),
    process_id: Some(std::process::id()),
  };
  status
    .set_service_status(running)
    .map_err(|e| e.to_string())?;
  let _ = STATUS.set(StatusHandle(status));

  // There is no stderr in a service, and the interesting part of a boot-time
  // failure happens before anybody can look at it.
  crate::diag::set_file(crate::config::dir().join("core.log"));

  // `serve` never returns: it owns the accept loop and the session worker.
  std::thread::spawn(|| {
    let cfg = crate::config::Config::load();
    match crate::ipc::serve(cfg) {
      Ok(crate::ipc::Serve::Started) => {}
      Ok(crate::ipc::Serve::AlreadyRunning) => {
        crate::diag::line("core: another core already owns the control pipe");
      }
      Err(e) => crate::diag::line(&format!("core: {e}")),
    }
  });

  // The SCM only asks us to stop; the session loop has no unwinding to do.
  let _ = stop_rx.recv();
  let _ = status.set_service_status(stopped());
  std::process::exit(0);
}
