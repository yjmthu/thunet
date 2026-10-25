// The normal entry point is the tray app, so no console window is created.
// CLI subcommands re-attach to the launching terminal (see the desktop module).
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() {
  thunet::desktop::main();
}

#[cfg(target_os = "android")]
fn main() {}
