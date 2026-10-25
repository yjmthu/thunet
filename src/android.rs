//! Android entry points.
//!
//! The activity is a plain `NativeActivity`; Android calls `android_main` on a
//! thread of its own, and everything below reuses the desktop's portable mode:
//! the core is a thread of this process and both halves talk over a filesystem
//! socket in the app's private directory.
//!
//! The same process can instead start life from the keep-alive service or from
//! a wake-up broadcast, so [`prepare`] is the one place that points the process
//! at the app directory; the JNI functions at the bottom are what those entry
//! points call.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::platform::android::activity::AndroidApp;

/// The panic hook is installed once per process, whichever entry point ran
/// first; a second hook would write every panic twice.
static PANIC_HOOK: AtomicBool = AtomicBool::new(false);

/// One login attempt at a time: the wake receiver, the 15-minute worker and the
/// service's network callback can all fire at once, and the portal sees every
/// attempt made for one account.
static LOGIN: Mutex<()> = Mutex::new(());

/// What the autofill service filled in, taken by the window on its next frame.
/// `None` on a side means "the service left that field alone".
static AUTOFILL: Mutex<Option<(Option<String>, Option<String>)>> = Mutex::new(None);

/// The egui context, remembered so a JNI callback can wake the event loop
/// instead of leaving the window asleep until the next input event.
static UI_CONTEXT: std::sync::OnceLock<eframe::egui::Context> = std::sync::OnceLock::new();

/// The window can take a back step: a menu or modal is open, or the settings
/// page is showing. Written by the window each pass, read by the Java thread.
static CAN_GO_BACK: AtomicBool = AtomicBool::new(false);

/// Java asked for one back step; the window consumes it on its next pass.
static BACK_REQUESTED: AtomicBool = AtomicBool::new(false);

/// IME events from the Activity's `InputConnection`: composition, commits and
/// deletions the soft keyboard sends. Written from the Java UI thread, drained
/// by the window on its next pass and handed to egui as `Event::Ime`.
static IME_EVENTS: Mutex<Vec<eframe::egui::ImeEvent>> = Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub extern "C" fn android_main(app: AndroidApp) {
  let dir = app
    .internal_data_path()
    .expect("Android internal data directory");
  prepare(&dir.to_string_lossy());
  // Before the first frame: every copy button and the log pane's long press
  // reach Java through this handle.
  crate::android_jni::install(&app);

  let cfg = crate::config::Config::load();
  // A second `android_main` in one process cannot build another event loop
  // (winit allows one). Saying so beats a splash screen that never goes away.
  if let Err(err) = crate::ui::run(cfg, false, app) {
    crate::diag::line(&format!("android: ui exited: {err}"));
  }
}

/// Point this process at the app's private directory.
///
/// Every Android entry point calls it before touching configuration,
/// credentials or the control socket: the Activity, the foreground service and
/// the wake receiver can each be the first thing Android starts in a process.
fn prepare(dir: &str) {
  // SAFETY: this runs before any thread that reads the variable is spawned, and
  // the value never changes for the life of the process.
  unsafe { std::env::set_var("THUNET_STATE_DIR", dir) };
  let path = Path::new(dir);
  crate::diag::set_file(path.join("core.log"));
  install_panic_log(path.to_path_buf());
}

/// With no console, a panic would vanish; keep it on disk instead.
fn install_panic_log(dir: PathBuf) {
  if PANIC_HOOK.swap(true, Ordering::Relaxed) {
    return;
  }
  use std::io::Write as _;

  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    if let Ok(mut file) = std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(dir.join("crash.log"))
    {
      let _ = writeln!(
        file,
        "[{}] {info}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
      );
      let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
    }
    previous(info);
  }));
}

/// `Native.ensureOnline(dir)`: one login attempt without starting the UI.
///
/// Returns 0 when the session is (or became) online, 1 when nobody could sign
/// in, 2 when the argument could not be read.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_ensureOnline(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  files_dir: jni::objects::JString,
) -> jni::sys::jint {
  let Some(dir) = jstring(&mut env, &files_dir) else {
    return 2;
  };
  prepare(&dir);
  ensure_online()
}

/// `Native.startCore(dir)`: host the core thread in this process.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_startCore(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  files_dir: jni::objects::JString,
) -> jni::sys::jint {
  let Some(dir) = jstring(&mut env, &files_dir) else {
    return 2;
  };
  prepare(&dir);
  match crate::ipc::host_core() {
    Ok(()) => 0,
    Err(err) => {
      crate::diag::line(&format!("service: core not started: {err}"));
      1
    }
  }
}

/// `Native.disconnect(dir)`: ask the running core to log out.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_disconnect(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  files_dir: jni::objects::JString,
) -> jni::sys::jint {
  let Some(dir) = jstring(&mut env, &files_dir) else {
    return 2;
  };
  prepare(&dir);
  match crate::ipc::open() {
    Some(link) => {
      link.send(crate::proto::Cmd::Disconnect);
      0
    }
    None => 1,
  }
}

fn ensure_online() -> i32 {
  let _guard = LOGIN
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner());
  // A core that is already running is the one allowed to talk to the portal:
  // asking it costs one message instead of a second, competing session.
  if let Some(link) = crate::ipc::open() {
    link.send(crate::proto::Cmd::Connect);
    crate::diag::line("wake: asked the running core to connect");
    return 0;
  }
  let cfg = crate::config::Config::load();
  if cfg.username.trim().is_empty() || cfg.password.is_empty() {
    // Written before any request: a cold-started process may be reclaimed
    // before a slow answer arrives, and the next attempt has to be traceable.
    crate::diag::line("wake: no saved credential, nothing to do");
    return 1;
  }
  let mut session = crate::session::Session::new(cfg);
  match session.ensure_online() {
    Ok(msg) => {
      crate::diag::line(&format!("wake: {msg}"));
      0
    }
    Err(err) => {
      crate::diag::line(&format!("wake: failed: {err}"));
      1
    }
  }
}

fn jstring(env: &mut jni::JNIEnv, value: &jni::objects::JString) -> Option<String> {
  let value = env.get_string(value).ok()?;
  Some(value.to_string_lossy().into_owned())
}

/// `Native.autofillResult(user, pass)`: the service filled the virtual form.
///
/// Either argument is null when the service did not touch that field, which is
/// why the pair is kept as two options instead of two strings.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_autofillResult(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  user: jni::objects::JObject,
  pass: jni::objects::JObject,
) {
  let user = optional_string(&mut env, user);
  let pass = optional_string(&mut env, pass);
  if user.is_none() && pass.is_none() {
    return;
  }
  if let Ok(mut slot) = AUTOFILL.lock() {
    *slot = Some((user, pass));
  }
  if let Some(ctx) = UI_CONTEXT.get() {
    ctx.request_repaint();
  }
}

fn optional_string(env: &mut jni::JNIEnv, value: jni::objects::JObject) -> Option<String> {
  if value.is_null() {
    return None;
  }
  let text = jni::objects::JString::from(value);
  env
    .get_string(&text)
    .ok()
    .map(|text| text.to_string_lossy().into_owned())
}

/// `Native.backPressed()`: true when the window can take a back step.
///
/// False is what lets Java leave the task in the background; true means the
/// window closes a menu or a modal, or goes back to the status page, on its
/// next pass.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_backPressed(
  _env: jni::JNIEnv,
  _class: jni::objects::JClass,
) -> jni::sys::jboolean {
  if !CAN_GO_BACK.load(Ordering::Relaxed) {
    return jni::sys::JNI_FALSE;
  }
  BACK_REQUESTED.store(true, Ordering::Relaxed);
  if let Some(ctx) = UI_CONTEXT.get() {
    ctx.request_repaint();
  }
  jni::sys::JNI_TRUE
}

/// Publish whether the window can take a back step; called from `logic`.
pub fn set_can_go_back(can: bool) {
  CAN_GO_BACK.store(can, Ordering::Relaxed);
}

/// Take the back step Java asked for, if any.
pub fn take_back() -> bool {
  BACK_REQUESTED.swap(false, Ordering::Relaxed)
}

/// `Native.imeCommit(text)`: the IME settled on this text.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_imeCommit(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  text: jni::objects::JString,
) {
  if let Some(text) = jstring(&mut env, &text) {
    push_ime(eframe::egui::ImeEvent::Commit(text));
  }
}

/// `Native.imePreedit(text)`: the IME is composing this text; empty ends it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_imePreedit(
  mut env: jni::JNIEnv,
  _class: jni::objects::JClass,
  text: jni::objects::JString,
) {
  if let Some(text) = jstring(&mut env, &text) {
    push_ime(eframe::egui::ImeEvent::Preedit {
      text,
      active_range_chars: None,
    });
  }
}

/// `Native.imeDelete(before, after)`: the IME deleted around the cursor.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_yjmthu_thunet_Native_imeDelete(
  _env: jni::JNIEnv,
  _class: jni::objects::JClass,
  before: jni::sys::jint,
  after: jni::sys::jint,
) {
  push_ime(eframe::egui::ImeEvent::DeleteSurrounding {
    before_chars: before.max(0) as usize,
    after_chars: after.max(0) as usize,
  });
}

fn push_ime(event: eframe::egui::ImeEvent) {
  if let Ok(mut queue) = IME_EVENTS.lock() {
    queue.push(event);
  }
  if let Some(ctx) = UI_CONTEXT.get() {
    ctx.request_repaint();
  }
}

/// Take the IME events that arrived since the last pass.
pub fn take_ime_events() -> Vec<eframe::egui::ImeEvent> {
  IME_EVENTS
    .lock()
    .map(|mut queue| std::mem::take(&mut *queue))
    .unwrap_or_default()
}

/// Remember the context so the JNI callbacks can wake the loop. Called once,
/// from the first `logic` pass.
pub fn remember_context(ctx: &eframe::egui::Context) {
  let _ = UI_CONTEXT.set(ctx.clone());
}

/// Take what the autofill service filled in, if anything arrived since the last
/// frame.
pub fn take_autofill_result() -> Option<(Option<String>, Option<String>)> {
  AUTOFILL.lock().ok().and_then(|mut slot| slot.take())
}
