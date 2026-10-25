package io.github.yjmthu.thunet;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** The notification's "Disconnect" action; talks to the core over the local socket. */
public class NotificationActionReceiver extends BroadcastReceiver {
  @Override
  public void onReceive(Context context, Intent intent) {
    if (!Wake.ACTION_DISCONNECT.equals(intent.getAction())) {
      return;
    }
    Context app = context.getApplicationContext();
    PendingResult pending = goAsync();
    String dir = app.getFilesDir().getAbsolutePath();
    new Thread(
            () -> {
              try {
                Native.disconnect(dir);
              } finally {
                pending.finish();
              }
            },
            "thunet-action")
        .start();
  }
}
