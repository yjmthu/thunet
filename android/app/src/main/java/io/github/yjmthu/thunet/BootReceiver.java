package io.github.yjmthu.thunet;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Boot hook: start the keep-alive service when the user asked for it, and arm
 * the one-shot otherwise.
 *
 * {@code BOOT_COMPLETED} is exempt from the background start restrictions, but
 * a ROM can still refuse; the toggle in the app remains the way back.
 */
public class BootReceiver extends BroadcastReceiver {
  @Override
  public void onReceive(Context context, Intent intent) {
    if (!Intent.ACTION_BOOT_COMPLETED.equals(intent.getAction())) {
      return;
    }
    Context app = context.getApplicationContext();
    String mode = Wake.mode(app);
    if (Wake.MODE_SERVICE.equals(mode)) {
      try {
        app.startForegroundService(new Intent(app, KeepAliveService.class));
      } catch (Throwable ignored) {
        // The service can still be started from the app.
      }
    } else if (Wake.MODE_WAKE.equals(mode)) {
      Wake.scheduleBackstop(app);
      Wake.arm(app);
      // Booting straight onto a Wi-Fi leaves nothing to arm (the shot would be
      // consumed at once) and the backstop may be a quarter of an hour away, so
      // sign in now. With no Wi-Fi the arm above is what covers the next join.
      if (Wake.wifiAvailable(app)) {
        PendingResult pending = goAsync();
        String dir = app.getFilesDir().getAbsolutePath();
        new Thread(
                () -> {
                  try {
                    Native.ensureOnline(dir);
                  } finally {
                    pending.finish();
                  }
                },
                "thunet-boot")
            .start();
      }
    }
    // Off means off: no service, no wake-up, no periodic job.
  }
}
