package io.github.yjmthu.thunet;

/**
 * JNI entry points implemented in Rust ({@code src/android.rs}).
 *
 * The class is only a name for the symbols: loading the library here is what
 * makes them reachable from a process that Android started for the service or
 * for a wake-up broadcast, where {@code android_main} never ran.
 */
final class Native {
  static {
    System.loadLibrary("thunet");
  }

  private Native() {}

  /** One login attempt (or one message to the running core). 0 on success. */
  static native int ensureOnline(String filesDir);

  /** Host the core thread in this process. Idempotent; 0 on success. */
  static native int startCore(String filesDir);

  /** Ask a running core to log out. 0 when the request was sent. */
  static native int disconnect(String filesDir);

  /**
   * The autofill service filled the virtual credential form. A null argument
   * means the service left that field alone.
   */
  static native void autofillResult(String user, String pass);

  /** True when the window can step back; Java backgrounds the task otherwise. */
  static native boolean backPressed();

  /** The keyboard committed this text to the focused credential field. */
  static native void imeCommit(String text);

  /** The keyboard is composing this text; an empty string ends the composition. */
  static native void imePreedit(String text);

  /** Delete this many characters before and after the cursor. */
  static native void imeDelete(int before, int after);

  /**
   * The connected Wi-Fi name in {@code files/ssid.txt} changed. The core reads
   * the file again and re-probes instead of polling it.
   */
  static native void ssidChanged();
}
