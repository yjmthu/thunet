package io.github.yjmthu.thunet;

import android.app.Application;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.util.Log;

/**
 * Process-wide wiring: which link the app talks over, and the state the Rust
 * side reads.
 *
 * This lives in the Application rather than in the Activity because a process
 * can be started by the keep-alive service or by a wake-up broadcast, and the
 * network binding has to survive the task being swiped away.
 */
public class ThunetApp extends Application {
  /** The Wi-Fi network the process is bound to, if any. */
  private Network boundWifi;
  private ConnectivityManager manager;
  private ConnectivityManager.NetworkCallback wifiCallback;
  private ConnectivityManager.NetworkCallback defaultCallback;

  @Override
  public void onCreate() {
    super.onCreate();
    manager = getSystemService(ConnectivityManager.class);
    // Synchronous first, so a process cold-started by the service or a wake-up
    // broadcast already probes over Wi-Fi before the first Rust request.
    bindWifi();
    watchWifi();
    watchDefault();
    // No-op at the "off" level; idempotent otherwise.
    Wake.scheduleBackstop(this);
    // A process started on mobile data (or by the backstop) never sees a
    // `wifi lost`, so the one-shot has to be armed here to cover the next join.
    // With a Wi-Fi already up this declines silently.
    if (Wake.MODE_WAKE.equals(Wake.mode(this))) {
      Wake.arm(this);
    }
  }

  /**
   * Prefer Wi-Fi for every socket this process creates.
   *
   * The portal session belongs to the Wi-Fi link, but Android keeps cellular as
   * the default network while the campus Wi-Fi is not validated — so probes
   * would ask the carrier whether there is a portal, and the answer would be
   * "no campus network" even though the campus Wi-Fi is connected.
   */
  private void bindWifi() {
    if (manager == null) {
      return;
    }
    Network wifi = null;
    for (Network network : manager.getAllNetworks()) {
      NetworkCapabilities caps = manager.getNetworkCapabilities(network);
      if (caps != null
          && caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)
          && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
        wifi = network;
        break;
      }
    }
    if (wifi != null) {
      manager.bindProcessToNetwork(wifi);
      boundWifi = wifi;
      Log.i(Wake.TAG, "link: bound to " + wifi);
    } else {
      manager.bindProcessToNetwork(null);
      boundWifi = null;
      Log.i(Wake.TAG, "link: no wifi, following the default network");
    }
    refreshLink();
  }

  /** Follow Wi-Fi networks, including the ones Android does not make default. */
  private void watchWifi() {
    if (manager == null) {
      return;
    }
    NetworkRequest request =
        new NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build();
    try {
      wifiCallback =
          new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
              Log.i(Wake.TAG, "link: wifi available " + network);
              manager.bindProcessToNetwork(network);
              boundWifi = network;
              refreshLink();
            }

            @Override
            public void onLost(Network network) {
              Log.i(Wake.TAG, "link: wifi lost " + network);
              // Back to the system default: without this the process would stay
              // bound to a network that no longer exists. Deliberately no
              // `bindWifi()` here: the system can still list the network that
              // was just lost for a moment, and re-binding to it leaves every
              // socket (DNS included) on a dead link until the app is restarted.
              // The next `onAvailable` binds a replacement Wi-Fi instead.
              if (network.equals(boundWifi)) {
                manager.bindProcessToNetwork(null);
                boundWifi = null;
              }
              refreshLink();
              // Not a single `arm()`: the system still lists the network that
              // just went away, so one attempt here usually reads "a wifi is
              // up" and leaves the app without a shot for the next join.
              // Only the middle level acts on the shot; "off" asked for nothing.
              if (Wake.MODE_WAKE.equals(Wake.mode(ThunetApp.this))) {
                Wake.armSoon(ThunetApp.this);
              }
            }
          };
      manager.registerNetworkCallback(request, wifiCallback);
      Log.i(Wake.TAG, "link: watching wifi");
    } catch (Throwable error) {
      Log.w(Wake.TAG, "link: cannot watch wifi", error);
    }
  }

  /** Follow the default network for the link row and the one-shot arming. */
  private void watchDefault() {
    if (manager == null) {
      return;
    }
    try {
      defaultCallback =
          new ConnectivityManager.NetworkCallback() {
            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
              refreshLink();
            }

            @Override
            public void onLost(Network network) {
              refreshLink();
              if (Wake.MODE_WAKE.equals(Wake.mode(ThunetApp.this))) {
                Wake.arm(ThunetApp.this);
              }
            }
          };
      manager.registerDefaultNetworkCallback(defaultCallback);
    } catch (Throwable error) {
      Log.w(Wake.TAG, "link: cannot watch the default network", error);
    }
  }

  /**
   * Write the link the app is actually using: the bound Wi-Fi when there is
   * one, otherwise the default network's transport.
   */
  private void refreshLink() {
    if (manager == null) {
      return;
    }
    // The Wi-Fi name changes with the link, and the optional SSID detection
    // must follow it; the call is a no-op without the permission.
    Ssid.update(this);
    // A binding the system no longer knows is not a link: drop it here too, so
    // a lost `onLost` cannot leave the row and the core describing a network
    // that is gone.
    if (boundWifi != null && manager.getNetworkCapabilities(boundWifi) == null) {
      manager.bindProcessToNetwork(null);
      boundWifi = null;
    }
    if (boundWifi != null) {
      write("wlan");
      return;
    }
    Network active = manager.getActiveNetwork();
    NetworkCapabilities caps = active == null ? null : manager.getNetworkCapabilities(active);
    if (caps == null) {
      write("none");
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
      write("wlan");
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) {
      write("ethernet");
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
      // The one link this app falls back to on a phone: named so the status
      // page says "mobile data" instead of the useless "other".
      write("mobile");
    } else {
      write("other");
    }
  }

  private void write(String value) {
    try {
      java.io.FileWriter writer =
          new java.io.FileWriter(new java.io.File(getFilesDir(), "link.txt"), false);
      writer.write(value + "\n");
      writer.close();
    } catch (Throwable ignored) {
      // The Rust side then falls back to the route table.
    }
  }
}
