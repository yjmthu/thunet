//! System clipboard.
//!
//! egui-winit deliberately leaves the platform clipboard alone on Android —
//! `egui_winit::clipboard::Clipboard` falls back to a string that only exists
//! inside the process there — so a `CopyText` command that reaches the platform
//! handler pastes nowhere. [`flush`] takes those commands out of the output
//! before eframe handles them and hands each text to the Activity over JNI (see
//! `src/android_jni.rs`). That one path covers every copy in the app: the log
//! menu, a label selection (`Event::Copy`), and [`copy`] itself.
//!
//! The in-process clipboard is therefore not used on Android: the app has no
//! inline text field there (the credential form is a native dialog), so nothing
//! pastes back into egui.

/// Put `text` on the clipboard.
///
/// The text leaves through egui's output queue rather than straight to JNI, so
/// widget copies and the explicit calls here take the same route.
pub fn copy(ctx: &eframe::egui::Context, text: &str) {
  ctx.copy_text(text.to_owned());
}

/// Send the copies egui produced this pass to the Android clipboard.
///
/// Called once at the end of `App::ui`, after every widget has run and before
/// eframe consumes the output. Draining (instead of peeking) leaves egui-winit
/// nothing to write, so a copy is never stored twice.
#[cfg(target_os = "android")]
pub fn flush(ctx: &eframe::egui::Context) {
  let texts = ctx.output_mut(|output| {
    let mut texts = Vec::new();
    output.commands.retain(|command| {
      if let eframe::egui::OutputCommand::CopyText(text) = command {
        texts.push(text.clone());
        false
      } else {
        true
      }
    });
    texts
  });
  for text in texts {
    if let Err(err) = crate::android_jni::call_str("copyText", &text) {
      crate::diag::line(&format!("clipboard: {err}"));
    }
  }
}
