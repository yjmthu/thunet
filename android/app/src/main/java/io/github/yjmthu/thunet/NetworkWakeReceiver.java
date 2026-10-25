package io.github.yjmthu.thunet;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

/**
 * Woken by the system when the Wi-Fi the one-shot waited for appears.
 *
 * The registration behind this broadcast is spent on delivery, so the receiver
 * re-arms only while no matching Wi-Fi is available — arming while the network
 * is up would consume the next shot immediately.
 */
public class NetworkWakeReceiver extends BroadcastReceiver {
  @Override
  public void onReceive(Context context, Intent intent) {
    Log.i(Wake.TAG, "wake: broadcast received");
    Context app = context.getApplicationContext();
    PendingResult pending = goAsync();
    String dir = app.getFilesDir().getAbsolutePath();
    new Thread(
            () -> {
              try {
                // The shot belongs to the middle level; the resident one is
                // kept alive by its service, and "off" asked for nothing.
                if (Wake.MODE_WAKE.equals(Wake.mode(app))) {
                  Native.ensureOnline(dir);
                  if (!Wake.wifiAvailable(app)) {
                    Wake.arm(app);
                  }
                }
              } finally {
                pending.finish();
              }
            },
            "thunet-wake")
        .start();
  }
}
