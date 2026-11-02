package io.github.yjmthu.thunet;

import android.app.Application;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.util.Log;

import java.net.Inet4Address;
import java.net.Inet6Address;
import java.net.InetAddress;

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
  /** Wi-Fi networks the system has lost and must not be named as a link again. */
  private final java.util.Set<Network> lostWifi = new java.util.HashSet<>();
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
              lostWifi.remove(network);
              manager.bindProcessToNetwork(network);
              boundWifi = network;
              refreshLink();
            }

            // An address arrived or left on this Wi-Fi. Android announces a
            // join through `onAvailable`, but the IPv6 address is assigned
            // later - SLAAC and DHCPv6 finish seconds after association - and
            // only this callback fires then. Without it the rows kept the IPv4
            // address alone until some unrelated event made `refreshLink` run.
            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties props) {
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
              // The active-network read below can still name this network, or
              // an older one the system has not forgotten yet, for a while.
              // Remember them all, so `refreshLink` writes the loss at once
              // instead of keeping "wlan" - and the account and address that
              // belong to it - on screen until the handover finishes.
              lostWifi.add(network);
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

            // The same address event on the default network: a Wi-Fi that took
            // over as default reports its IPv6 here, and mobile data reports
            // the addresses it hands out after the APN comes up.
            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties props) {
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
    // A binding the system no longer knows is not a link: drop it here too, so
    // a lost `onLost` cannot leave the row and the core describing a network
    // that is gone.
    if (boundWifi != null
        && (lostWifi.contains(boundWifi)
            || manager.getNetworkCapabilities(boundWifi) == null)) {
      lostWifi.add(boundWifi);
      manager.bindProcessToNetwork(null);
      boundWifi = null;
    }
    if (boundWifi != null) {
      write("wlan", boundWifi);
      return;
    }
    Network active = manager.getActiveNetwork();
    // A network the system has lost is no link, even if the active-network
    // read still returns it for a moment: naming it would keep the old Link row
    // on screen until the real handover finished.
    NetworkCapabilities caps =
        active == null || lostWifi.contains(active)
            ? null
            : manager.getNetworkCapabilities(active);
    if (caps == null) {
      write("none", null);
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
      write("wlan", active);
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) {
      write("ethernet", active);
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_USB)) {
      write("usb", active);
    } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
      // The one link this app falls back to on a phone: named so the status
      // page says "mobile data" instead of the useless "other".
      write("mobile", active);
    } else {
      write("other", active);
    }
  }

  /**
   * Write the link the process is using, with the addresses it holds on it.
   *
   * The core publishes these as one fact, so the transport and the address can
   * never describe different networks; the old one-line format could keep
   * "wlan" beside a mobile-data address while a handover was in progress. The
   * log names only the transport: an address must not end up in a log that gets
   * pasted into an issue.
   */
  private void write(String kind, Network network) {
    String ip4 = "";
    String ip6 = "";
    LinkProperties props = network == null ? null : manager.getLinkProperties(network);
    if (props != null) {
      for (LinkAddress link : props.getLinkAddresses()) {
        InetAddress address = link.getAddress();
        if (address.isAnyLocalAddress()
            || address.isLoopbackAddress()
            || address.isLinkLocalAddress()) {
          continue;
        }
        String text = address.getHostAddress();
        int scope = text.indexOf('%');
        if (scope >= 0) {
          text = text.substring(0, scope);
        }
        if (address instanceof Inet4Address && ip4.isEmpty()) {
          ip4 = text;
        } else if (address instanceof Inet6Address && ip6.isEmpty()) {
          ip6 = text;
        }
      }
    }
    try {
      // The only way the Rust side learns this: the push itself is what wakes
      // the core, so nothing has to look at the interface or the file system.
      Native.setLink(kind + "\n" + ip4 + "\n" + ip6 + "\n");
      Log.i(Wake.TAG, "link: now " + kind);
    } catch (Throwable ignored) {
      // The library is not loaded yet: the next network change pushes again.
    }
  }
}
