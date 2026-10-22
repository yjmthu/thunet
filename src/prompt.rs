//! Terminal prompts for the CLI verbs that need a secret.
//!
//! Only reached when the command was started from a terminal: the binary uses
//! the GUI subsystem, so it attaches to the launching console first (see
//! `main::attach_parent_console`). A start from Explorer has no stdin to ask
//! on, and those verbs keep their old behaviour instead of waiting for an
//! answer nobody can type.
//!
//! Ctrl+C is part of the contract: a prompt must be cancellable, and the
//! terminal must not be left with echo switched off.

use std::io::{IsTerminal as _, Write};

/// Whether there is a terminal to ask a question on.
pub fn available() -> bool {
  std::io::stdin().is_terminal()
}

/// Make Ctrl+C end the questions cleanly, whatever they are showing at the
/// time.
///
/// Installed before the first question rather than when echo is switched off:
/// a cancel has to work at the account prompt too, and it must leave the
/// terminal exactly as it found it.
pub fn arm_cancel() {
  #[cfg(windows)]
  console::install();
  #[cfg(unix)]
  tty::install();
}

/// Ask for one line, showing `prompt` first. Surrounding whitespace is dropped:
/// it is never part of an account name.
pub fn line(prompt: &str) -> Result<String, String> {
  print!("{prompt}");
  std::io::stdout().flush().map_err(|e| e.to_string())?;
  let mut answer = String::new();
  std::io::stdin()
    .read_line(&mut answer)
    .map_err(|e| format!("cannot read the answer: {e}"))?;
  Ok(answer.trim().to_string())
}

/// Ask for a secret: the same read, but the terminal does not echo it.
///
/// Only the line terminator is stripped - a password may end in spaces, and
/// trimming them would change it.
pub fn secret(prompt: &str) -> Result<String, String> {
  print!("{prompt}");
  std::io::stdout().flush().map_err(|e| e.to_string())?;
  let echo = Echo::off()?;
  let mut answer = String::new();
  let read = std::io::stdin().read_line(&mut answer);
  drop(echo);
  // The user pressed Enter, but the terminal never showed it.
  println!();
  read.map_err(|e| format!("cannot read the password: {e}"))?;
  Ok(answer.trim_end_matches(['\r', '\n']).to_string())
}

/// Echo turned off for as long as this value lives, plus a Ctrl+C hook that
/// puts it back before the process ends.
///
/// Without the hook a cancelled prompt leaves the console without echo, so
/// everything typed into that window afterwards is invisible - and on Windows
/// a Ctrl+C does nothing at all, because a GUI-subsystem process has no default
/// console handler to end it.
#[cfg(windows)]
struct Echo(Option<u32>);

#[cfg(windows)]
mod console {
  use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

  use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
    SetConsoleCtrlHandler, SetConsoleMode,
  };

  /// The mode in force before echo was switched off; `0` means "echo was never
  /// disabled", which is also a value `GetConsoleMode` never reports.
  static SAVED: AtomicU32 = AtomicU32::new(0);
  static INSTALLED: AtomicBool = AtomicBool::new(false);

  /// Turn echo off and report the previous mode, or `None` when stdin is not a
  /// console (a pipe or a file never echoes anyway).
  pub fn quiet() -> Option<CONSOLE_MODE> {
    // SAFETY: plain console calls on the process's own stdin handle.
    unsafe {
      let handle = GetStdHandle(STD_INPUT_HANDLE);
      let mut mode: CONSOLE_MODE = 0;
      if GetConsoleMode(handle, &mut mode) == 0 {
        return None;
      }
      if SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) == 0 {
        return None;
      }
      SAVED.store(mode, Ordering::SeqCst);
      install();
      Some(mode)
    }
  }

  /// Put the saved mode back, if echo is still off.
  pub fn restore() {
    let mode = SAVED.swap(0, Ordering::SeqCst);
    if mode != 0 {
      // SAFETY: restoring the mode that was read from this same handle.
      unsafe { SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode) };
    }
  }

  pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
      return;
    }
    // SAFETY: the handler only touches the console and this process.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
  }

  /// Ctrl+C: cancel the prompt, leaving the terminal usable.
  ///
  /// Returning `FALSE` would hand the event to a default handler this process
  /// does not have, so it is handled here and the exit is explicit.
  unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
    restore();
    println!();
    println!("cancelled");
    std::process::exit(130);
  }
}

#[cfg(windows)]
impl Echo {
  fn off() -> Result<Self, String> {
    Ok(Self(console::quiet()))
  }
}

#[cfg(windows)]
impl Drop for Echo {
  fn drop(&mut self) {
    if self.0.is_some() {
      console::restore();
    }
  }
}

/// The same, for a POSIX terminal.
#[cfg(unix)]
struct Echo(Option<libc::termios>);

#[cfg(unix)]
mod tty {
  use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

  /// The mode to restore, boxed and handed to the signal handler. Leaked on
  /// purpose: freeing from a handler is not worth the risk, and it costs one
  /// allocation per password prompt.
  static SAVED: AtomicPtr<libc::termios> = AtomicPtr::new(std::ptr::null_mut());
  static INSTALLED: AtomicBool = AtomicBool::new(false);

  /// Turn echo off, returning the mode to restore, or `None` when stdin is not
  /// a terminal.
  pub fn quiet() -> Option<libc::termios> {
    // SAFETY: both calls take fd 0 and a buffer this frame owns.
    unsafe {
      let mut term: libc::termios = std::mem::zeroed();
      if libc::tcgetattr(0, &mut term) != 0 {
        return None;
      }
      let mut without_echo = term;
      without_echo.c_lflag &= !libc::ECHO;
      if libc::tcsetattr(0, libc::TCSAFLUSH, &without_echo) != 0 {
        return None;
      }
      SAVED.store(Box::into_raw(Box::new(term)), Ordering::SeqCst);
      install();
      Some(term)
    }
  }

  pub fn restore() {
    let saved = SAVED.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !saved.is_null() {
      // SAFETY: the pointer came from `Box::into_raw` in `quiet`, and the
      // buffer is still alive because it is deliberately never freed.
      unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, saved) };
    }
  }

  pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
      return;
    }
    // SAFETY: `on_sigint` is a plain C function, and `SA_RESETHAND` restores
    // the default disposition before it runs.
    unsafe {
      let mut action: libc::sigaction = std::mem::zeroed();
      action.sa_sigaction = on_sigint as usize;
      action.sa_flags = libc::SA_RESETHAND;
      libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
  }

  /// Ctrl+C: put the terminal back, then let the default action end the process.
  extern "C" fn on_sigint(_signal: libc::c_int) {
    restore();
    // SAFETY: the disposition is the default one again, so raising SIGINT from
    // its own handler simply terminates the process.
    unsafe {
      libc::raise(libc::SIGINT);
    }
  }
}

#[cfg(unix)]
impl Echo {
  fn off() -> Result<Self, String> {
    Ok(Self(tty::quiet()))
  }
}

#[cfg(unix)]
impl Drop for Echo {
  fn drop(&mut self) {
    if self.0.is_some() {
      tty::restore();
    }
  }
}
