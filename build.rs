//! Embeds the application icon and version info into the Windows executable.
//! Without this the shell (Explorer, taskbar, Start Menu) shows a generic exe
//! icon even though the tray and window draw their own.

fn main() {
  println!("cargo:rerun-if-changed=assets/thunet.ico");

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
