//! Waiting for the link to change instead of asking it every second.
//!
//! Two things want to know: the thread that publishes the link row, and the
//! core loop that decides what a move means. Both used to look once a second,
//! which on a phone is a wakeup per second for a fact that changes a few times
//! a day.
//!
//! Windows reports the changes itself - `NotifyIpInterfaceChange` for the
//! interface set and its addresses, `NotifyRouteChange2` for the routing table
//! (which is also what an SSL VPN rearranges) - and Android pushes them in over
//! JNI from the `Application` that registered the connectivity callbacks. Both
//! land in one generation counter here. Everywhere else, and as a backstop on
//! those two, the timeout still does the work.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long a waiter sleeps when nothing happens.
///
/// The platforms that can report a change do not need a look every second; the
/// others still do, because nothing tells them.
#[cfg(any(windows, target_os = "android"))]
pub const PERIOD: Duration = Duration::from_secs(30);
#[cfg(not(any(windows, target_os = "android")))]
pub const PERIOD: Duration = Duration::from_secs(1);

/// A handle both the OS callbacks and the Android push can signal.
///
/// Cheap to clone (one `Arc`), safe to share across threads, and created once
/// per process.
#[derive(Clone)]
pub struct Watcher {
  inner: Arc<Inner>,
}

struct Inner {
  /// Bumped by whoever saw a change.
  ///
  /// A counter rather than a flag: two threads wait here, and a single flag
  /// would be consumed by whichever of them woke first, leaving the other to
  /// sit out its whole timeout after an event it should have seen.
  generation: Mutex<u64>,
  signal: Condvar,
}

impl Watcher {
  pub fn new() -> Self {
    let inner = Arc::new(Inner {
      generation: Mutex::new(0),
      signal: Condvar::new(),
    });
    #[cfg(windows)]
    register(&inner);
    Self { inner }
  }

  /// Read this *before* looking at the link, and hand it to [`wait_since`].
  ///
  /// Taking the mark first is what closes the gap between reading the link and
  /// going to sleep: a change that lands in between makes the wait return at
  /// once instead of after the timeout.
  pub fn generation(&self) -> u64 {
    self
      .inner
      .generation
      .lock()
      .map(|generation| *generation)
      .unwrap_or_default()
  }

  /// Block until the link changed after `seen`, or `timeout` passes.
  ///
  /// `true` means something reported a change; `false` means the timeout ran
  /// out and the caller should look anyway.
  pub fn wait_since(&self, seen: u64, timeout: Duration) -> bool {
    let Ok(generation) = self.inner.generation.lock() else {
      std::thread::sleep(timeout);
      return false;
    };
    // The predicate is what makes a spurious wakeup harmless, and what lets two
    // waiters both see one event.
    let (generation, _) = self
      .inner
      .signal
      .wait_timeout_while(generation, timeout, |current| *current == seen)
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    *generation != seen
  }

  /// Report a change from something that is not one of the platform callbacks -
  /// today that is the Android `Application`, over JNI.
  #[cfg_attr(not(target_os = "android"), allow(dead_code))]
  pub fn wake(&self) {
    notify(&self.inner);
  }
}

fn notify(inner: &Inner) {
  if let Ok(mut generation) = inner.generation.lock() {
    *generation = generation.wrapping_add(1);
  }
  inner.signal.notify_all();
}

/// Ask Windows to report interface and route changes.
///
/// The registration lives as long as the process, so the returned handles are
/// not kept and `CancelMibChangeNotify2` is never called; the context pointer is
/// a leaked `Arc` for the same reason, which is what lets a callback outlive the
/// last waiter.
#[cfg(windows)]
fn register(inner: &Arc<Inner>) {
  use windows_sys::Win32::NetworkManagement::IpHelper::{
    MIB_IPFORWARD_ROW2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
    NotifyRouteChange2,
  };
  use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

  unsafe extern "system" fn on_interface(
    context: *const core::ffi::c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    _kind: MIB_NOTIFICATION_TYPE,
  ) {
    if !context.is_null() {
      notify(unsafe { &*(context as *const Inner) });
    }
  }

  unsafe extern "system" fn on_route(
    context: *const core::ffi::c_void,
    _row: *const MIB_IPFORWARD_ROW2,
    _kind: MIB_NOTIFICATION_TYPE,
  ) {
    if !context.is_null() {
      notify(unsafe { &*(context as *const Inner) });
    }
  }

  let context = Arc::into_raw(Arc::clone(inner)) as *const core::ffi::c_void;
  // One handle per registration; both are written by the API and then dropped,
  // because nothing here ever unregisters.
  let mut interfaces = std::ptr::null_mut();
  let mut routes = std::ptr::null_mut();
  let (interface_result, route_result) = unsafe {
    (
      NotifyIpInterfaceChange(
        AF_UNSPEC,
        Some(on_interface),
        context,
        false,
        &mut interfaces,
      ),
      NotifyRouteChange2(AF_UNSPEC, Some(on_route), context, false, &mut routes),
    )
  };
  // Said out loud because a refusal is invisible: everything still works, it
  // just stops being instant, and nothing else would explain the delay.
  if interface_result == 0 && route_result == 0 {
    crate::diag::line("link watch: interface and route notifications registered");
  } else {
    crate::diag::line(&format!(
      "link watch: notifications refused ({interface_result}/{route_result}), looking every {} s",
      PERIOD.as_secs()
    ));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_wake_ends_the_wait_early() {
    let watcher = Watcher::new();
    let other = watcher.clone();
    let seen = watcher.generation();
    std::thread::spawn(move || {
      std::thread::sleep(Duration::from_millis(50));
      other.wake();
    });
    let started = std::time::Instant::now();
    assert!(
      watcher.wait_since(seen, Duration::from_secs(5)),
      "the wake is reported"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // The event was consumed: the next wait runs its own timeout.
    let seen = watcher.generation();
    assert!(!watcher.wait_since(seen, Duration::from_millis(10)));
  }

  #[test]
  fn a_wake_during_the_look_is_not_lost() {
    let watcher = Watcher::new();
    // What a waiter does: mark, read, then sleep. A change reported while it
    // reads must end the sleep at once instead of after thirty seconds.
    let seen = watcher.generation();
    watcher.wake();
    assert!(watcher.wait_since(seen, Duration::from_secs(30)));
    assert!(!watcher.wait_since(watcher.generation(), Duration::from_millis(10)));
  }

  #[test]
  fn two_waiters_both_see_one_wake() {
    let watcher = Watcher::new();
    let seen = watcher.generation();
    let (tx, rx) = std::sync::mpsc::channel();
    for _ in 0..2 {
      let (watcher, tx) = (watcher.clone(), tx.clone());
      std::thread::spawn(move || {
        let _ = tx.send(watcher.wait_since(seen, Duration::from_secs(5)));
      });
    }
    std::thread::sleep(Duration::from_millis(50));
    watcher.wake();
    let results: Vec<bool> = rx.iter().take(2).collect();
    assert_eq!(results, vec![true, true]);
  }
}
