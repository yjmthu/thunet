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
  crate::diag::line("service: service_main entered");
  if let Err(e) = body() {
    crate::diag::line(&format!("service: {e}"));
    std::process::exit(1);
  }
}

/// A clean stop status, used when the SCM asks the process to exit.
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
  crate::diag::line("service: control handler registered");

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
  crate::diag::line("service: status running");

  // There is no stderr in a service, and the interesting part of a boot-time
  // failure happens before anybody can look at it.
  crate::diag::set_file(crate::config::dir().join("core.log"));
  crate::diag::line(&format!(
    "service: state dir {}, pid {}",
    crate::config::dir().display(),
    std::process::id()
  ));

  // `serve` never returns while it is serving: it owns the accept loop and the
  // session worker. If it returns at all, this instance is useless, and a
  // service that stays `RUNNING` without serving anybody can only be repaired
  // by a second manual restart. Leaving the process alive would keep the SCM
  // (and the window) convinced that a core is listening, so fail out loud.
  std::thread::spawn(|| {
    crate::diag::line("service: starting core serve loop");
    let cfg = crate::config::Config::load_machine();
    match crate::ipc::serve(cfg, true) {
      Ok(crate::ipc::Serve::Started) => {}
      Ok(crate::ipc::Serve::AlreadyRunning) => {
        crate::diag::line("service: another core owns the control pipe; stopping this instance");
      }
      Err(e) => crate::diag::line(&format!("service: {e}")),
    }
    crate::diag::line("service: no core is being served, exiting with code 1");
    std::process::exit(1);
  });

  // The SCM only asks us to stop; the session loop has no unwinding to do.
  let _ = stop_rx.recv();
  crate::diag::line("service: stop requested");
  let _ = status.set_service_status(stopped());
  std::process::exit(0);
}
