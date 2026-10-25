package io.github.yjmthu.thunet;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import java.io.File;
import java.io.FileWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * The optional Wi-Fi-name bridge.
 *
 * {@code WifiInfo.getSSID()} needs the location permission, so this only runs
 * while the user's "Secure detection" setting is on and the permission is held.
 * The name is written to {@code files/ssid.txt}; Rust's {@code wifi::classify()}
 * reads it and treats the empty string as "no name", which is exactly the
 * probe-only behaviour the app had before the setting existed.
 */
final class Ssid {
  static final String ENABLED_FILE = "secure.txt";
  static final String SSID_FILE = "ssid.txt";

  private Ssid() {}

  static boolean enabled(Context context) {
    try {
      byte[] raw =
          Files.readAllBytes(new File(context.getFilesDir(), ENABLED_FILE).toPath());
      return "on".equals(new String(raw, StandardCharsets.UTF_8).trim());
    } catch (Throwable ignored) {
      return false;
    }
  }

  static void setEnabled(Context context, boolean on) {
    write(context, ENABLED_FILE, on ? "on" : "off");
    update(context);
  }

  static boolean permitted(Context context) {
    return context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION)
        == PackageManager.PERMISSION_GRANTED;
  }

  /** Refresh {@code ssid.txt}; empty when the setting is off or the permission is missing. */
  static void update(Context context) {
    if (!enabled(context) || !permitted(context)) {
      write(context, SSID_FILE, "");
      return;
    }
    write(context, SSID_FILE, current(context));
  }

  /** The connected SSID without quotes; empty when it cannot be read. */
  static String current(Context context) {
    WifiManager manager = context.getSystemService(WifiManager.class);
    if (manager == null) {
      return "";
    }
    WifiInfo info = manager.getConnectionInfo();
    if (info == null) {
      return "";
    }
    String name = info.getSSID();
    if (name == null) {
      return "";
    }
    if (name.length() >= 2 && name.startsWith("\"") && name.endsWith("\"")) {
      name = name.substring(1, name.length() - 1);
    }
    // What the platform answers without the permission, or before an
    // association completes.
    if ("<unknown ssid>".equals(name) || name.startsWith("0x")) {
      return "";
    }
    return name;
  }

  private static void write(Context context, String name, String value) {
    try {
      FileWriter writer = new FileWriter(new File(context.getFilesDir(), name), false);
      writer.write(value + "\n");
      writer.close();
    } catch (Throwable ignored) {
      // The Rust side treats a missing file as "no name".
    }
  }
}
