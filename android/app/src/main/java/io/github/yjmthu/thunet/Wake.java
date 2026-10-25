package io.github.yjmthu.thunet;

import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.util.Log;
import androidx.work.Constraints;
import androidx.work.ExistingPeriodicWorkPolicy;
import androidx.work.NetworkType;
import androidx.work.PeriodicWorkRequest;
import androidx.work.WorkManager;
import java.io.File;
import java.io.FileWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.concurrent.TimeUnit;

/**
 * The background level, the one-shot wake-up and the 15-minute backstop.
 *
 * The level mirrors the desktop's autostart triad: {@code off}, {@code wake}
 * (the core alone, reached through a network change) and {@code service} (the
 * core plus the always-present entry point, here the keep-alive service and its
 * notification). "Keep online" is a different setting and is not touched here.
 *
 * The level lives in a file rather than in {@code config.json} because three
 * parties write it — the Activity, the notification's stop action and the boot
 * receiver — and the Rust side only reads it. One file means one answer.
 */
final class Wake {
  static final String MODE_FILE = "background.txt";
  static final String DENIED_FILE = "background.denied";
  static final String MODE_OFF = "off";
  static final String MODE_WAKE = "wake";
  static final String MODE_SERVICE = "service";
  static final String TAG = "THUnet";
  static final String ACTION_WAKE = "io.github.yjmthu.thunet.WAKE";
  static final String ACTION_STOP = "io.github.yjmthu.thunet.STOP_KEEPALIVE";
  static final String ACTION_DISCONNECT = "io.github.yjmthu.thunet.DISCONNECT";
  static final String CHANNEL = "thunet-keepalive";
  static final int NOTIFICATION_ID = 0x5448; // "TH"
  private static final String WORK = "thunet-wake";

  private Wake() {}

  /** {@code off}, {@code wake} or {@code service}; a missing file is off. */
  static String mode(Context context) {
    try {
      byte[] raw = Files.readAllBytes(new File(context.getFilesDir(), MODE_FILE).toPath());
      String mode = new String(raw, StandardCharsets.UTF_8).trim();
      if (MODE_OFF.equals(mode) || MODE_WAKE.equals(mode) || MODE_SERVICE.equals(mode)) {
        return mode;
      }
      return MODE_OFF;
    } catch (Throwable ignored) {
      return MODE_OFF;
    }
  }

  static void setMode(Context context, String mode) {
    write(context, MODE_FILE, mode);
  }

  /** True when the last request for the resident level lost the permission. */
  static boolean denied(Context context) {
    return new File(context.getFilesDir(), DENIED_FILE).exists();
  }

  static void setDenied(Context context, boolean denied) {
    File marker = new File(context.getFilesDir(), DENIED_FILE);
    if (denied) {
      write(context, DENIED_FILE, "");
    } else {
      marker.delete();
    }
  }

  /**
   * Move to {@code mode}, stopping whatever the previous level started.
   *
   * The resident level still has to be started by the Activity, which owns the
   * notification permission flow; this only tears down and schedules.
   */
  static void apply(Context context, String mode) {
    context.stopService(new Intent(context, KeepAliveService.class));
    disarm(context);
    setMode(context, mode);
    if (MODE_OFF.equals(mode)) {
      cancelBackstop(context);
      return;
    }
    scheduleBackstop(context);
    if (MODE_WAKE.equals(mode)) {
      arm(context);
    }
  }

  /**
   * Is a Wi-Fi network with INTERNET available?
   *
   * Deliberately not {@code NET_CAPABILITY_VALIDATED}: a campus network behind
   * the portal has INTERNET but is not validated until after the login this
   * app performs.
   */
  static boolean wifiAvailable(Context context) {
    ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
    if (manager == null) {
      return false;
    }
    for (Network network : manager.getAllNetworks()) {
      NetworkCapabilities caps = manager.getNetworkCapabilities(network);
      if (caps != null
          && caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)
          && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
        return true;
      }
    }
    return false;
  }

  /**
   * Arm the one-shot wake-up, but only while no matching Wi-Fi is available.
   *
   * Android spends a {@code PendingIntent} registration on the first
   * availability and refuses every later one, so arming while the network is
   * already up burns the shot.
   */
  static void arm(Context context) {
    if (wifiAvailable(context)) {
      return;
    }
    ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
    if (manager == null) {
      return;
    }
    try {
      manager.registerNetworkCallback(request(), wakeIntent(context));
      Log.i(TAG, "wake: one-shot armed");
    } catch (Throwable ignored) {
      // Too many registrations or a refused request: the backstop still runs.
    }
  }

  /** Drop the one-shot when the level no longer wants it. */
  static void disarm(Context context) {
    ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
    if (manager == null) {
      return;
    }
    try {
      manager.unregisterNetworkCallback(wakeIntent(context));
    } catch (Throwable ignored) {
      // Nothing was armed.
    }
  }

  private static NetworkRequest request() {
    return new NetworkRequest.Builder()
        .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
        .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        .build();
  }

  private static PendingIntent wakeIntent(Context context) {
    return PendingIntent.getBroadcast(
        context,
        0,
        new Intent(context, NetworkWakeReceiver.class).setAction(ACTION_WAKE),
        PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
  }

  /** The periodic backstop; WorkManager keeps it scheduled across reboots. */
  static void scheduleBackstop(Context context) {
    // The off level asked for nothing; scheduling here (the Application does it
    // on every start) must not resurrect a job the user turned off.
    if (MODE_OFF.equals(mode(context))) {
      return;
    }
    Constraints constraints =
        new Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build();
    PeriodicWorkRequest request =
        new PeriodicWorkRequest.Builder(WakeWorker.class, 15, TimeUnit.MINUTES)
            .setConstraints(constraints)
            .build();
    try {
      WorkManager.getInstance(context)
          .enqueueUniquePeriodicWork(WORK, ExistingPeriodicWorkPolicy.KEEP, request);
    } catch (Throwable ignored) {
      // Without WorkManager there are still the live callback and the one-shot.
    }
  }

  static void cancelBackstop(Context context) {
    try {
      WorkManager.getInstance(context).cancelUniqueWork(WORK);
    } catch (Throwable ignored) {
      // Not scheduled.
    }
  }

  private static void write(Context context, String name, String value) {
    try {
      FileWriter writer = new FileWriter(new File(context.getFilesDir(), name), false);
      writer.write(value + "\n");
      writer.close();
    } catch (Throwable ignored) {
      // A missing file reads as the safe default.
    }
  }
}
