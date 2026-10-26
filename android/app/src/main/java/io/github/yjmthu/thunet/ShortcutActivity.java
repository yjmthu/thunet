package io.github.yjmthu.thunet;

import android.app.Activity;
import android.os.Bundle;
import android.widget.Toast;

/**
 * The two launcher shortcuts: sign in or out without opening the window.
 *
 * The launcher can only start an Activity, so this one is a thin shell around
 * the same JNI calls the notification uses. It is translucent and finishes as
 * soon as the request is done, so nothing is left on screen but the toast; the
 * window itself is not started and the core either serves the request itself or
 * sends one message to the core that is already running.
 */
public class ShortcutActivity extends Activity {
  static final String ACTION_CONNECT = "io.github.yjmthu.thunet.shortcut.CONNECT";
  static final String ACTION_DISCONNECT = "io.github.yjmthu.thunet.shortcut.DISCONNECT";

  @Override
  protected void onCreate(Bundle state) {
    super.onCreate(state);
    String action = getIntent() == null ? "" : getIntent().getAction();
    String dir = getFilesDir().getAbsolutePath();
    new Thread(
            () -> {
              int message;
              if (ACTION_CONNECT.equals(action)) {
                message =
                    Native.ensureOnline(dir) == 0
                        ? R.string.shortcut_connected
                        : R.string.shortcut_failed;
              } else if (ACTION_DISCONNECT.equals(action)) {
                message =
                    Native.disconnect(dir) == 0
                        ? R.string.shortcut_disconnected
                        : R.string.shortcut_failed;
              } else {
                message = R.string.shortcut_failed;
              }
              final int text = message;
              runOnUiThread(
                  () -> {
                    Toast.makeText(this, text, Toast.LENGTH_SHORT).show();
                    finish();
                  });
            },
            "thunet-shortcut")
        .start();
  }
}
