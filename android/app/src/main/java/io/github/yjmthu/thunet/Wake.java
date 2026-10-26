package io.github.yjmthu.thunet;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.Settings;
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
  /** The user confirmed the OEM toggles that the app cannot read. */
  static final String ALLOWED_FILE = "background.allowed";
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
  /** Waits before each attempt to arm the one-shot after a Wi-Fi is lost. */
  private static final long[] ARM_RETRY_MS = {0, 1000, 3000, 8000};
  private static final Handler MAIN = new Handler(Looper.getMainLooper());

  /** What one arming attempt did. */
  enum Arm {
    /** The registration is in the system and will wake us on the next join. */
    Armed,
    /** A matching Wi-Fi is up, so the shot has nothing left to cover. */
    WifiUp,
    /** The system refused the registration. */
    Failed,
  }

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

  /**
   * Record the user's confirmation for the OEM toggles.
   *
   * MIUI's autostart and power policy have no readable state and are not the
   * AOSP battery whitelist, so the app keeps the user's own answer instead of
   * guessing.
   */
  static void setAllowed(Context context, boolean on) {
    File marker = new File(context.getFilesDir(), ALLOWED_FILE);
    if (on) {
      write(context, ALLOWED_FILE, "1");
    } else {
      marker.delete();
    }
  }

  /**
   * Ask the system to exempt this app from battery optimisation.
   *
   * Returns false when it is already exempt (or the ROM has no such dialog), so
   * the caller can go straight to the OEM page.
   */
  static boolean requestPower(Activity activity) {
    PowerManager power = activity.getSystemService(PowerManager.class);
    if (power == null || power.isIgnoringBatteryOptimizations(activity.getPackageName())) {
      return false;
    }
    Intent intent =
        new Intent(
                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:" + activity.getPackageName()))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
    try {
      activity.startActivity(intent);
      return true;
    } catch (Throwable error) {
      Log.i(TAG, "background: no battery dialog: " + error);
      return false;
    }
  }

  /**
   * Open where the ROM keeps the toggles this app cannot set itself: MIUI's
   * autostart and its power policy.
   *
   * The OEM actions are not public API and differ by ROM, so each one is tried
   * in turn and the app's own info page - which carries both toggles on the
   * ROMs we know - is the last resort.
   */
  static void openOemSettings(Context context) {
    String pkg = context.getPackageName();
    for (String action :
        new String[] {"miui.intent.action.OP_AUTO_START", "miui.intent.action.APP_PERM_EDITOR"}) {
      Intent intent =
          new Intent(action)
              .addCategory(Intent.CATEGORY_DEFAULT)
              .putExtra("extra_pkgname", pkg)
              .putExtra("extra_package_name", pkg)
              .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
      try {
        context.startActivity(intent);
        return;
      } catch (Throwable ignored) {
        // Not this ROM; try the next.
      }
    }
    try {
      context.startActivity(
          new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:" + pkg))
              .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    } catch (Throwable ignored) {
      // Nothing left to open; the hint still says where to look.
    }
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
      // Nothing on the system's books yet: arm now if there is no Wi-Fi to
      // cover. `onLost` is what re-arms after the network goes away.
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
   * Measured on a phone (Android 16, `dumpsys connectivity`): a registration
   * made while the network is already up is delivered immediately - the
   * broadcast arrives within the same second - and the system releases the
   * request about five seconds later (`RELEASE ... to trigger PendingIntent`).
   * Keeping one armed permanently is therefore impossible: it would fire at
   * once, be released, and re-registering would just repeat that in a loop
   * instead of covering the next join.
   */
  static Arm arm(Context context) {
    if (wifiAvailable(context)) {
      return Arm.WifiUp;
    }
    ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
    if (manager == null) {
      return Arm.Failed;
    }
    try {
      manager.registerNetworkCallback(request(), wakeIntent(context));
      Log.i(TAG, "wake: one-shot armed");
      return Arm.Armed;
    } catch (Throwable error) {
      // Too many registrations or a refused request: the backstop still runs.
      Log.i(TAG, "wake: arm failed: " + error);
      return Arm.Failed;
    }
  }

  /**
   * Arm once the Wi-Fi that just went away has left the system's network list.
   *
   * `onLost` runs while the system still lists the network, so a single attempt
   * there reads {@link #wifiAvailable} as true and silently leaves the app
   * without its one-shot; the next join is then noticed only by the 15-minute
   * backstop. Retrying is what makes "drop the Wi-Fi, kill the app, rejoin"
   * work instead of being a coin toss.
   */
  static void armSoon(Context context) {
    armAttempt(context.getApplicationContext(), 0);
  }

  private static void armAttempt(Context context, int step) {
    if (step == ARM_RETRY_MS.length) {
      Log.i(TAG, "wake: not armed: a wifi network is still listed");
      return;
    }
    MAIN.postDelayed(
        () -> {
          switch (arm(context)) {
            case Armed, Failed -> {}
            case WifiUp -> {
              Log.i(TAG, "wake: wifi still listed, retrying");
              armAttempt(context, step + 1);
            }
          }
        },
        ARM_RETRY_MS[step]);
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
