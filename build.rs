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

  #[cfg(windows)]
  {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/thunet.ico");
    res.set("ProductName", "THUnet");
    res.set("FileDescription", "THUnet campus network client");
    res.set("LegalCopyright", "");
    res.compile().expect("compile Windows resources");
  }
}
