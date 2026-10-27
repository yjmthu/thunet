//! Embeds the application icon and version info into the Windows executable.
//! Without this the shell (Explorer, taskbar, Start Menu) shows a generic exe
//! icon even though the tray and window draw their own.

fn main() {
  println!("cargo:rerun-if-changed=assets/thunet.ico");
  // The stamp printed in Settings must match the binary around it, so refresh
  // it whenever a rebuild can happen; otherwise cargo reuses the cached epoch.
  println!("cargo:rerun-if-changed=src");
  println!("cargo:rerun-if-changed=Cargo.toml");
  let epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  println!("cargo:rustc-env=THUNET_BUILD_EPOCH={epoch}");
  // Cargo sets `PROFILE` for build scripts (`debug` for `cargo build`, `release`
  // for `--release`). Stamping it makes it obvious which binary is on screen.
  let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
  println!("cargo:rustc-env=THUNET_PROFILE={profile}");

  // The icon belongs to Windows binaries, and a build script is compiled for
  // the *host*: `cfg(windows)` would also fire when a Windows machine
  // cross-compiles for Android, where winresource refuses the target
  // ("Can only compile resource file when target_env is \"gnu\" or \"msvc\"").
  let target = std::env::var("TARGET").unwrap_or_default();
  if target.starts_with("windows") {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/thunet.ico");
    res.set("ProductName", "THUnet");
    // Task Manager labels a process with this string, and both halves of the
    // application are the same image - so it must read as the product, not as
    // one of its halves. The service is still told apart by its SCM display
    // name (`THUnet Background Service`), which Task Manager shows nested under
    // the process.
    res.set("FileDescription", "THUnet Campus Network App");
    res.set("LegalCopyright", "");
    res.compile().expect("compile Windows resources");
  }
}
