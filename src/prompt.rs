//! Terminal prompts for the verbs that ask questions.
//!
//! Only `thunet login -i` reaches this. The binary uses the GUI subsystem, so it
//! attaches to the launching console first (see `main::attach_parent_console`),
//! and the flag is the caller's promise that the shell is waiting for it: an
//! interactive cmd or PowerShell would otherwise read the same console and eat
//! the answers. The secret read and the Ctrl+C handler come from `rpassword` and
//! `ctrlc`, so no console or termios call is written here.
//!
//! The pipe path uses none of this: `piped_line` reads plain lines from stdin.

use std::io::{IsTerminal as _, Write};

/// Whether there is a terminal to ask a question on.
pub fn available() -> bool {
  std::io::stdin().is_terminal()
}

/// End the command on Ctrl+C instead of leaving a question unanswered.
///
/// A cancel can arrive from the console handler thread, and `rpassword` returns
/// its own interrupt when the terminal delivers Ctrl+C as a character (POSIX
/// clears `ISIG`, and Windows hands the byte through with echo off). Both kinds
/// end here, so the message and the status are the same either way, and the
/// terminal is already back: `rpassword` restores it as its read returns.
pub fn arm_cancel() {
  let _ = ctrlc::set_handler(|| cancel());
}

/// Report the cancel once and leave with 130.
fn cancel() -> ! {
  static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
  if !REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
    println!();
    println!("cancelled");
  }
  // The call is the tail expression on purpose: it returns `!`, so the block
  // diverges by construction. As a statement it still satisfies `-> !` for
  // rustc, but rust-analyzer then types the block as `()` and reports
  // "expected `!`, found `()`" on the closing brace.
  std::process::exit(130)
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

/// Ask for a secret: only the line terminator is stripped, because a password may
/// end in spaces.
///
/// A terminal is read by `rpassword`, which turns the echo off for the duration
/// and restores it when the read returns. Anything else - a pipe, `NUL` - is a
/// plain line: there is no echo to suppress and no terminal to configure.
pub fn secret(prompt: &str) -> Result<String, String> {
  print!("{prompt}");
  std::io::stdout().flush().map_err(|e| e.to_string())?;
  if available() {
    let answer = match rpassword::read_password() {
      Ok(answer) => answer,
      Err(e) if e.kind() == std::io::ErrorKind::Interrupted => cancel(),
      Err(e) => return Err(format!("cannot read the password: {e}")),
    };
    // A Windows console echoes the line break of the Enter even with echo off; a
    // POSIX terminal does not, so the newline is ours there.
    #[cfg(unix)]
    println!();
    return Ok(answer.trim_end_matches(['\r', '\n']).to_string());
  }
  read_stdin_line()
}

/// One line from stdin, terminator removed and nothing else touched.
fn read_stdin_line() -> Result<String, String> {
  let mut answer = String::new();
  std::io::stdin()
    .read_line(&mut answer)
    .map_err(|e| format!("cannot read the password: {e}"))?;
  Ok(answer.trim_end_matches(['\r', '\n']).to_string())
}

/// One line from a redirected stdin, terminator removed.
///
/// `None` means there was no line at all: the pipe was empty or closed, or a
/// launch with no console and no pipeline (the tray started from Explorer) has no
/// usable handle. The caller falls back to the stored credential instead of
/// treating that as an answer - which is what separates "nothing was piped" from
/// "the first line was left empty".
pub fn piped_line() -> Option<String> {
  let mut answer = String::new();
  match std::io::stdin().read_line(&mut answer) {
    Ok(0) | Err(_) => None,
    Ok(_) => Some(answer.trim_end_matches(['\r', '\n']).to_string()),
  }
}
