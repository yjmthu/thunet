//! The JNI bridge to `ThunetActivity`.
//!
//! `android_main` is the only place that receives the `AndroidApp` handle, so
//! the VM and the Activity are captured there once and reused by everything
//! that needs to call Java later: the clipboard and the keep-alive switch.
//! Calls are made on the thread that paints or ticks, which is the Android main
//! thread, so no extra attachment dance is needed.

use std::sync::OnceLock;

use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{GlobalRef, JObject, JValue};
use winit::platform::android::activity::AndroidApp;

static VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: OnceLock<GlobalRef> = OnceLock::new();

/// Remember the VM and the Activity. Called once, from `android_main`.
pub fn install(app: &AndroidApp) {
  // SAFETY: both pointers are owned by android-activity and stay valid for the
  // life of the process; `from_raw` only stores them.
  let vm = match unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) } {
    Ok(vm) => vm,
    Err(_) => return,
  };
  let activity = {
    let env = match vm.attach_current_thread() {
      Ok(env) => env,
      Err(_) => return,
    };
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
    match env.new_global_ref(&activity) {
      Ok(activity) => activity,
      Err(_) => return,
    }
  };
  let _ = VM.set(vm);
  let _ = ACTIVITY.set(activity);
}

/// Call `ThunetActivity.<method>(String)`.
pub fn call_str(method: &str, text: &str) -> Result<(), String> {
  with_env(|env, activity| {
    let value = JObject::from(env.new_string(text)?);
    env.call_method(
      activity,
      method,
      "(Ljava/lang/String;)V",
      &[JValue::Object(&value)],
    )?;
    Ok(())
  })
}

/// Call `ThunetActivity.<method>(boolean)`.
pub fn call_bool(method: &str, value: bool) -> Result<(), String> {
  with_env(|env, activity| {
    env.call_method(activity, method, "(Z)V", &[JValue::Bool(value as u8)])?;
    Ok(())
  })
}

/// Tell the Activity which credential field has the focus, where it sits (in
/// physical pixels) and what it holds. This is what opens the autofill
/// dropdown for a field that only exists on the egui surface.
pub fn autofill_enter(id: i32, x: i32, y: i32, w: i32, h: i32, value: &str) -> Result<(), String> {
  with_env(|env, activity| {
    let value = JObject::from(env.new_string(value)?);
    env.call_method(
      activity,
      "autofillEnter",
      "(IIIIILjava/lang/String;)V",
      &[
        JValue::Int(id),
        JValue::Int(x),
        JValue::Int(y),
        JValue::Int(w),
        JValue::Int(h),
        JValue::Object(&value),
      ],
    )?;
    Ok(())
  })
}

/// The text of the focused field changed; the save prompt depends on this.
pub fn autofill_value(id: i32, value: &str) -> Result<(), String> {
  with_env(|env, activity| {
    let value = JObject::from(env.new_string(value)?);
    env.call_method(
      activity,
      "autofillValue",
      "(ILjava/lang/String;)V",
      &[JValue::Int(id), JValue::Object(&value)],
    )?;
    Ok(())
  })
}

/// The field moved on screen.
///
/// Unlike [`autofill_enter`], this must not start a new fill request: the
/// dropdown's own appearance moves the layout, and re-entering on every move
/// makes the service show its UI again and again.
pub fn autofill_bounds(id: i32, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
  with_env(|env, activity| {
    env.call_method(
      activity,
      "autofillBounds",
      "(IIIII)V",
      &[
        JValue::Int(id),
        JValue::Int(x),
        JValue::Int(y),
        JValue::Int(w),
        JValue::Int(h),
      ],
    )?;
    Ok(())
  })
}

/// No credential field has the focus any more.
pub fn autofill_leave() -> Result<(), String> {
  with_env(|env, activity| {
    env.call_method(activity, "autofillLeave", "()V", &[])?;
    Ok(())
  })
}

/// The form was submitted; let the service offer to save the pair.
pub fn autofill_save() -> Result<(), String> {
  with_env(|env, activity| {
    env.call_method(activity, "autofillSave", "()V", &[])?;
    Ok(())
  })
}

/// The system clipboard text, for the credential fields' long-press paste.
pub fn clipboard_text() -> Option<String> {
  with_env(|env, activity| {
    let value = env
      .call_method(activity, "clipboardText", "()Ljava/lang/String;", &[])?
      .l()?;
    if value.is_null() {
      return Ok(None);
    }
    let text = jni::objects::JString::from(value);
    let text = env.get_string(&text)?;
    Ok(Some(text.to_string_lossy().into_owned()))
  })
  .ok()
  .flatten()
}

/// A short long-press tick, for the log pane's menu.
pub fn haptic() -> Result<(), String> {
  with_env(|env, activity| {
    env.call_method(activity, "haptic", "()V", &[])?;
    Ok(())
  })
}

fn with_env<T>(
  f: impl FnOnce(&mut JNIEnv, &GlobalRef) -> jni::errors::Result<T>,
) -> Result<T, String> {
  let vm = VM.get().ok_or("android: no activity handle")?;
  let activity = ACTIVITY.get().ok_or("android: no activity handle")?;
  let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
  f(&mut env, activity).map_err(|e| e.to_string())
}
