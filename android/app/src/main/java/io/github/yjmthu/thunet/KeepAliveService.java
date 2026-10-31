package io.github.yjmthu.thunet;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.graphics.drawable.Icon;
import android.net.ConnectivityManager;
import android.net.Network;
import android.os.IBinder;
import android.util.Log;
import java.util.Locale;

/**
 * The optional keep-alive service: a foreground service with a silent
 * notification, which is the only way Android keeps a process alive after the
 * task is swiped away.
 *
 * It owns the process life; the core itself is the Rust thread
 * {@link Native#startCore} hosts, so the window and the service share one core
 * exactly as the desktop's portable mode does.
 */
public class KeepAliveService extends Service {
  private ConnectivityManager.NetworkCallback callback;

  @Override
  public void onCreate() {
    super.onCreate();
    Log.i(Wake.TAG, "keep-alive: onCreate");
    createChannel();
    ConnectivityManager manager = getSystemService(ConnectivityManager.class);
    if (manager != null) {
      callback =
          new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
              String dir = getFilesDir().getAbsolutePath();
              // The core polls on its own; this only makes a reconnect after a
              // Wi-Fi change immediate.
              new Thread(() -> Native.ensureOnline(dir), "thunet-wake").start();
            }
          };
      try {
        manager.registerDefaultNetworkCallback(callback);
      } catch (Throwable ignored) {
        // The core's own poll still covers the change.
      }
    }
    // Belt and braces: on this ROM a sticky restart sometimes delivers only the
    // create transaction, leaving the start call pending in ActivityManager
    // (`callStart=true`) and `onStartCommand` never running - the service then
    // exists without a notification and without a core. Both calls are
    // idempotent, so doing them here as well costs nothing when the normal path
    // does run.
    //
    // Only the resident level hosts a core: at the `wake` level the service is
    // merely the target of a one-shot network callback and must stop itself
    // again, so a core there would defeat the level the user chose.
    if (Wake.MODE_SERVICE.equals(Wake.mode(this))) {
      startTheCore();
    }
  }

  /**
   * Become a foreground service and host the core.
   *
   * Called from both lifecycle entry points on purpose; see {@link #onCreate}.
   */
  private void startTheCore() {
    // Every step is logged: a service-only start has no window, so this journal
    // is the only place that can say whether `startCore` ran and what it said.
    try {
      Log.i(Wake.TAG, "keep-alive: startForeground");
      startForeground(Wake.NOTIFICATION_ID, buildNotification());
      Log.i(Wake.TAG, "keep-alive: startCore");
      int ranked = Native.startCore(getFilesDir().getAbsolutePath());
      Log.i(Wake.TAG, "keep-alive: startCore -> " + ranked);
    } catch (Throwable error) {
      Log.e(Wake.TAG, "keep-alive: start failed", error);
    }
  }

  @Override
  public int onStartCommand(Intent intent, int flags, int startId) {
    Log.i(Wake.TAG, "keep-alive: start " + (intent == null ? "restart" : intent.getAction()));
    if (intent != null && Wake.ACTION_STOP.equals(intent.getAction())) {
      Log.i(Wake.TAG, "keep-alive: stopping");
      // The notification belongs to the resident level; dropping to the middle
      // one keeps the wake-up path, which needs no notification.
      Wake.apply(this, Wake.MODE_WAKE);
      stopForeground(STOP_FOREGROUND_REMOVE);
      stopSelf();
      return START_NOT_STICKY;
    }
    // Every step is logged while a service-only start is being diagnosed: the
    // restart after a swipe has no window, so this journal is the only place
    // that can say whether `startCore` ran and what it answered.
    startTheCore();
    // The user asked for this to survive; a restart after a low-memory kill is
    // the point of the setting.
    return START_STICKY;
  }

  @Override
  public IBinder onBind(Intent intent) {
    return null;
  }

  @Override
  public void onDestroy() {
    if (callback != null) {
      ConnectivityManager manager = getSystemService(ConnectivityManager.class);
      if (manager != null) {
        try {
          manager.unregisterNetworkCallback(callback);
        } catch (Throwable ignored) {
          // Already gone.
        }
      }
    }
    super.onDestroy();
  }

  private void createChannel() {
    NotificationManager manager = getSystemService(NotificationManager.class);
    if (manager == null) {
      return;
    }
    NotificationChannel channel =
        new NotificationChannel(
            Wake.CHANNEL,
            text("后台保活", "Background keep-alive"),
            NotificationManager.IMPORTANCE_LOW);
    channel.setDescription(
        text(
            "划掉任务后仍保持校园网会话在线",
            "Keeps the campus session online while the app is swiped away"));
    channel.setShowBadge(false);
    channel.setSound(null, null);
    channel.enableVibration(false);
    manager.createNotificationChannel(channel);
  }

  private Notification buildNotification() {
    PendingIntent open =
        PendingIntent.getActivity(
            this,
            0,
            new Intent(this, ThunetActivity.class).setFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    PendingIntent stop =
        PendingIntent.getService(
            this,
            1,
            new Intent(this, KeepAliveService.class).setAction(Wake.ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    PendingIntent disconnect =
        PendingIntent.getBroadcast(
            this,
            2,
            new Intent(this, NotificationActionReceiver.class).setAction(Wake.ACTION_DISCONNECT),
            PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    Icon icon = Icon.createWithResource(this, R.drawable.ic_stat_thunet);
    return new Notification.Builder(this, Wake.CHANNEL)
        .setSmallIcon(R.drawable.ic_stat_thunet)
        .setContentTitle("THUnet")
        .setContentText(text("保持校园网会话在线", "Keeping the campus session online"))
        .setContentIntent(open)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
        .setShowWhen(false)
        .setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        .addAction(
            new Notification.Action.Builder(icon, text("断开", "Disconnect"), disconnect).build())
        .addAction(new Notification.Action.Builder(icon, text("停止保活", "Stop keep-alive"), stop).build())
        .build();
  }

  /** The notification follows the system language, not the app's setting. */
  private String text(String zh, String en) {
    return Locale.getDefault().getLanguage().startsWith("zh") ? zh : en;
  }
}
