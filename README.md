# THUnet

**English** | [简体中文](README.zh-CN.md)

A small cross-platform tray app that logs you into the Tsinghua University
campus network (`auth4.tsinghua.edu.cn`, SRun3K portal) and keeps the connection
alive.

One executable, no runtime dependencies, no console window, runs on Windows,
Linux and macOS, and speaks Chinese or English.

> **New here?** Read [Quick start](#1-quick-start) — you only need your TUNET
> account name and password. The protocol details are at the very end of this
> document; you never need them to use the app.

## Contents

**Getting started**

1. [Quick start](#1-quick-start)
2. [The window at a glance](#2-the-window-at-a-glance)
3. [Everyday tasks](#3-everyday-tasks)
4. [Troubleshooting](#4-troubleshooting)
5. [Command line](#5-command-line)

**Reference**

6. [Where your data lives](#6-where-your-data-lives)
7. [Installing, uninstalling, shortcuts](#7-installing-uninstalling-shortcuts)
8. [Installing with winget](#8-installing-with-winget)

**For the curious / for developers**

9. [How the network login actually works](#9-how-the-network-login-actually-works)
10. [Building from source](#10-building-from-source)
11. [Project layout](#11-project-layout)
12. [Limits and notes](#12-limits-and-notes)
13. [License](#13-license)

---

## 1. Quick start

1. **Get the executable.** Take the installer for your system (recommended), or
   build it yourself (see [Building from source](#10-building-from-source)):

   | System | File | Install with |
   | --- | --- | --- |
   | Windows | `thunet-<version>-x64.msi` | double-click, `msiexec /i thunet-<version>-x64.msi` (administrator rights, installs to `Program Files`), or `winget install --id THUnet.THUnet -e` once published (see [Installing with winget](#8-installing-with-winget)) |
   | Debian / Ubuntu | `thunet_<version>-1_amd64.deb` | `sudo apt install ./thunet_<version>-1_amd64.deb` |
   | other Linux | `thunet-<version>-linux-x64.tar.gz` | unpack and run `./thunet` (optionally `./thunet shortcut` for a desktop launcher) |

2. **Run it.** Start `THUnet` from the Start Menu / application menu, or run
   `thunet` in a terminal. A small window opens; everything the app does is
   written to the log strip at the bottom of that window.
3. **Open Settings.** Click the button in the top-right corner of the window.
4. **Enter your credentials.** Type your TUNET account (for example
   `1234567890`) and your password, tick **Remember password**, then click
   **Apply**. The password is sealed with a key bound to this machine before it
   touches the disk, and never ends up in the settings file.
5. **Connect.** Go back to the Status page and click **Connect** (or just wait a
   few seconds if *Auto connect* is already enabled). The dot in the header turns
   green and your IPv4 address appears.

That is all. From now on the app reconnects by itself whenever the network comes
back, and closing the window only hides it in the tray.

**Optional, worth doing once:**

- Settings → **Start with the system**, so the client is already running after a
  reboot.
- Settings → **Start minimised to tray**, if you do not want the window to pop up
  at login.
- Settings → **Language**, to pick 中文 or English instead of following the
  system language.

## 2. The window at a glance

The window has exactly two pages, switched with the button in the top-right
corner.

![The THUnet status page](docs/images/en-status.png)

*Status page, with the account name, addresses and user name redacted.*

### Status page

| Element | Meaning |
| --- | --- |
| Coloured dot + label | `Online`, `Authenticating` or `Offline` |
| `Account` | The account the portal reports for the current session |
| `IPv4` / `IPv6` | Addresses assigned by the campus network, each with a **Copy** button |
| `Session traffic` | Bytes downloaded / uploaded during this session |
| `Service` | Whether a core is answering, with its version and uptime; it turns to `not running` as soon as the core is stopped. The button beside it stops or starts that process — like `thunet service stop`, it never closes the window |
| **Connect** / **Disconnect** / **Refresh** | Manual actions; only the ones that make sense are enabled |
| Log strip | The most recent events, newest at the bottom |

Log lines look like this:

```
[2026-10-20 12:56:26] [INFO] connected
[2026-10-20 12:56:41] [DEBUG] portal: auth4.tsinghua.edu.cn
```

Each level (`DEBUG`, `INFO`, `WARN`, `ERROR`, `FATAL`) has its own colour. Log
text is always English — it is developer-facing diagnostics, so it does not
follow the *Language* setting; everything else in the window does. The strip
shows the newest lines and scrolls.

*Settings → Log level* hides lines below the chosen level (`DEBUG` shows
everything, then `INFO`, `WARN`, `ERROR`). Hiding is display-only: the lines are
still kept, so lowering the level again brings them back.

Right-click on selected log text copies the selection straight to the
clipboard. With nothing selected, right-click opens a small menu with
**Copy line** and **Copy all** (both copy only what the current level shows).

### Settings page

![The THUnet settings page](docs/images/en-settings.png)

*Settings page, with the account and password fields redacted.*

| Setting | Effect |
| --- | --- |
| Start with the system | Three modes on one row: Off / Service / Service + tray (see [7](#7-install-uninstall-shortcut)). "Tray only" is not offered — the tray never starts a core by itself, so the icon would only ever report "service not running". On Windows this row is the only place the app asks for administrator rights: registering or changing the service takes one UAC prompt, starting and stopping it takes none |
| Start minimised to tray | Independent of the line above: autostart can stay hidden while a manual launch opens the window, or the other way round. Greyed out in *Service*, where no tray starts |
| Auto connect | Log in as soon as a usable network appears |
| Create desktop shortcut | Puts a launcher for the running executable on your desktop (the same action as `thunet shortcut`) |
| Account / Password / Remember password / Apply / Revert | This is the only row that does not save as you type: the button turns into the accent colour and an *Unsaved* marker appears while it differs from what is stored. Applying seals the password into `credential.bin` (never into the JSON file) and passes it to the running session immediately; *Revert* drops the edits and restores the stored account, password and checkbox (it is greyed out while *Remember password* is off, since there is nothing stored to go back to) |
| Language | Follow system (default), 中文, English |
| Log level | `DEBUG` / `INFO` / `WARN` / `ERROR`: the lowest level shown in the log page (`INFO` by default, stored in `config.json`) |
| Version | Version on the left, build time on the right (local clock to the second, with its UTC offset); the GitHub icon next to the version opens this repository |

The three switches confirm the state they just moved to in a toast
(*Start with system: On*), because nothing else on the page changes when one of
them is flipped.

**Connect** uses the password the background worker holds: at launch that is what
the credential file has (only if *Remember password* was saved), and afterwards
whatever was last applied — text typed into the fields does nothing until
**Apply**, which is what the *Unsaved* marker warns about. Unticking *Remember
password* and applying deletes the stored entry, yet the password you just typed
keeps working until the app exits. With no password anywhere, **Connect** reports
it instead of sending an empty login to the portal.

Having no saved password is not the same as being offline: the client keeps
asking the portal for the session state either way, so an existing session
(today's, or one another device on this network created) still shows up as
online with its account, addresses and traffic.

The build stamp shows the version on the left and, on the right, the local time
of the compile that produced this binary with its UTC offset
(`Built at 2026-10-20 19:23:23 +08:00`). Both are embedded at build time
(`build.rs`), so the stamp always matches the running executable — handy when
you are not sure whether the update actually took. The wall clock is rendered in
the timezone of whoever is looking, so the same binary reads differently on a
machine set to another zone; the offset says which one you are seeing.

**Connect is greyed out off-campus.** A login can only succeed behind the campus
portal, so the client probes the network every few seconds and disables the
button — with the reason on hover — when the machine is on an 802.1X network
such as `Tsinghua-Secure`, has no link at all, sits behind someone else's captive
portal, or already reaches the internet without a portal. **Refresh** stays
available in every case, and **Disconnect** follows the session state alone, so
an existing session can always be ended.

### Tray icon

**Left-click the tray icon** to show the window (or to bring an existing one
back to the front); **double-click** it to hide the window again. Telling the
two apart takes a fixed 300 ms, so a single click acts once that delay has
passed.

**Right-click** for `Show window`, `Hide window`, `Connect`, `Disconnect`
and `Quit`.

Closing the window hides it — the app keeps working. `Quit` ends the window and
its tray icon only: the background service keeps running, whatever the autostart
mode says, and is stopped explicitly from the status page's *Stop* button or
with `thunet service stop`.

### Feedback messages

Every action answers with a coloured strip at the bottom of the window: **green**
for success, **red** for failure, **blue** for information such as "already
online", "copied", or the state a switch just moved to
("Start with system: On"). Nothing succeeds silently.

## 3. Everyday tasks

**Start the service** — the window never starts it by itself: while the
`Service` row says `not running`, click **Start** next to it (or run
`thunet service start`). A red banner at the top of the Status page says the
same until a service answers again. **Stop** ends the service and leaves the
window open.

**Log in and out** — use the buttons on the Status page, the tray menu, or
`thunet login` / `thunet logout`. If a session already exists the client says so
instead of authenticating twice.

**See your address** — the IPv4/IPv6 rows refresh automatically every 15 seconds
while online. Click **Copy** and a confirmation strip appears.

**Keep it running without a window** — close the window. The tray icon stays,
logging continues, and the session is kept alive with a request to the campus
homepage every 5 minutes.

**Start with the computer** — Settings → *Start with the system*. Enable *Start
minimised to tray* as well if the window should stay hidden (the autostart entry
then carries `--minimized`). On Windows the switch asks for administrator rights
once, because the background core has to be registered with the Service Control
Manager; on Linux everything stays inside your user account.

**Switch language** — Settings → *Language*. The choice is stored in the config
file and applies immediately.

## 4. Troubleshooting

| Symptom | What to do |
| --- | --- |
| Log says `login rejected` / `E2901` | Wrong password. Re-enter it in Settings, tick *Remember password* again and click **Apply**. |
| Log says `E2808 Control policy not found` | The portal reported an unexpected access-controller id. The client normally detects it automatically; on a captive Wi-Fi it can only guess. Reconnect to the campus Wi-Fi and click **Refresh**. |
| Log says `E2553` | The account is already online elsewhere (phone, another laptop). Log out there first. |
| Internet works but no session appears | You are on `Tsinghua-Secure` or another 802.1X network, where no portal login is needed — the client stays idle by design. |
| Nothing happens after clicking **Connect** | Read the log strip. When the machine has no link at all, the client waits 30 s before retrying. |
| **Connect** is greyed out | The client does not think a login can work here (802.1X Wi-Fi such as `Tsinghua-Secure`, no link, another operator's portal, or the internet already works). Hover the button for the exact reason; **Refresh** still polls the portal. |
| **Connect** says `No account or password saved` | Nothing is available to log in with: type the password in Settings and click **Apply**. The client does not send an empty login to the portal. |
| The window disappeared | It is in the tray. Click the tray icon (or use `Show window` in its menu). |
| The app crashed | Read `crash.log` next to `config.json` (see [Where your data lives](#6-where-your-data-lives)). The app has no console, so that file is the only record. |
| A command prints nothing in PowerShell | The binary has no console and PowerShell does not wait for it. Use `cmd /c thunet status` or `Start-Process -Wait thunet`. |

## 5. Command line

The window is the default mode; every subcommand is optional, and the same
executable does both jobs.

```
thunet                       # tray + window (same as no arguments)
thunet --minimized           # start hidden in the tray
thunet core                  # background service: owns the network session
thunet service <action>      # install/uninstall/enable/disable/start/stop/restart/status
thunet status                # JSON: service state + portal session/traffic
thunet login                 # ask the service to authenticate now
thunet logout                # ask the service to de-authenticate
thunet ac-id                 # portal ac_id the service is using
thunet config                # config path + effective settings
thunet shortcut              # create a desktop shortcut
thunet completions <shell>   # print a bash/zsh/fish/elvish/powershell completion script
thunet selftest              # note: the real test vectors live in `cargo test`
```

On Windows the MSI puts the executable in `C:\Program Files\THUnet\bin` and
appends that directory to the system `PATH`, so `thunet` works in any shell
started after the install; the `.deb` installs it as `/usr/bin/thunet`.

Completion scripts come from the same `clap` definition the parser uses, so they
cover every subcommand (including `service <action>`) and every global flag:

```
thunet completions bash > ~/.local/share/bash-completion/completions/thunet
thunet completions zsh  > ~/.zfunc/_thunet        # with: fpath=(~/.zfunc $fpath)
thunet completions fish > ~/.config/fish/completions/thunet.fish
```

The `.deb` already installs bash, zsh and fish completions under `/usr/share`, so
on Debian/Ubuntu they work right after `apt install` with nothing to set up.

The window is only a *frontend*. The network session lives in `thunet core`,
which the window talks to over a local socket (a named pipe on Windows, a Unix
socket elsewhere). The window never starts that process on its own: it comes
from autostart, `thunet service start`, or the *Start* button on the status
page. Closing the window therefore keeps you online, and the core is the half
that can start at boot — a systemd user unit on Linux, a **Windows service** on
Windows, both created by the *Start with the system* setting. That switch is
the only place a UAC prompt appears: registering the service needs
administrator rights, starting and stopping it afterwards does not.

`status`, `login`, `logout` and `ac-id` are frontends too: they ask the running
service over that same socket instead of contacting the portal themselves, so
there is exactly one owner of the session. With no service running they stop
with an error naming the command that starts it — they never fall back to a
second, unmanaged connection.

`thunet status` prints one object with both halves:

```json
{
  "core":   { "version": "0.1.4", "build_epoch": 1792571000, "pid": 1234,
              "uptime_secs": 812,
              "state": "Online", "env": "Campus", "ac_id": "169",
              "host": "auth4.tsinghua.edu.cn", "auto_connect": true,
              "wanted_online": true, "failures": 0, "frontends": 1,
              "endpoint": "/run/user/1000/thunet/control.sock" },
  "portal": { "error": "ok", "online_ip": "…", "bytes_in": … }
}
```

The `core` half describes the running service (`uptime_secs`, the network
verdict in `env`, the attached frontends). If the portal query fails, `portal`
is `null`, `portal_error` carries the reason, and the command exits non-zero.

Starting `thunet` while it is already running does **not** start a second copy:
the existing window is brought back instead.

Global overrides: `--host <host>`, `--http`, `--timeout <secs>`, `-u/--username`,
`-p/--password`, `--ip <addr>`, `--ac-id <id>`.

Precedence is **command line > config file > credential store**. `-u` / `-p` are
handed to the service for `login` (in memory only, never written to disk), while
`--host` / `--http` / `--timeout` shape what `thunet core` talks to:
`thunet -u 1234567890 -p secret login`.

## 6. Where your data lives

| OS | Config directory |
| --- | --- |
| Windows | `%APPDATA%\thunet\` |
| macOS | `~/Library/Application Support/thunet/` |
| Linux | `$XDG_CONFIG_HOME/thunet/` (usually `~/.config/thunet/`) |

Files in that directory:

| File | Contents |
| --- | --- |
| `config.json` | Account name, portal host, autostart / minimise / auto-connect flags, keepalive settings, language. **No password** — that field is skipped when serialising. |
| `credential.bin` | The sealed account/password pair, mode `0600`. Rewritten on every apply; unreadable if copied to another machine. |
| `crash.log` | Appended only when the app panics. |

Password storage is one implementation on every platform: `credential.bin` holds
`MAGIC ‖ nonce ‖ ciphertext` (ChaCha20-Poly1305) with mode `0600`, under a key
derived as `SHA-256("thunet/credential/v1" ‖ 0 ‖ machine identity)`.

| OS | Machine identity |
| --- | --- |
| Windows | `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` |
| Linux | `/etc/machine-id` (or `/var/lib/dbus/machine-id`) |
| macOS | host name, until a release needs IOKit's `IOPlatformUUID` |

The point of a machine key is unattended startup: the core has to log in before
anyone logs in, so no logged-on user may be needed to unlock the secret. That
also sets the limit — the file is useless once it leaves this machine (backups,
dotfile repositories, a synced config directory), but any process running as you
can read the same identity and decrypt it. Only a passphrase prompt or a TPM
would stop that, and both cost unattended startup.

Deleting `config.json` resets every setting. Uninstalling the package leaves this
directory alone, so your account and preferences survive a reinstall; delete the
directory for a clean slate.

## 7. Installing, uninstalling, shortcuts

**THUnet is installed and removed by the operating system only** — the `.msi` on
Windows, the `.deb` on Debian/Ubuntu. The application itself never copies files
or writes uninstall entries; the one thing it does register is autostart (a
logon task for the tray, a service for the core), and only when you ask for it.
There is no "install for me" button and no `thunet install` command. That keeps
one owner per installation and avoids the two registrations fighting over the
same files.

| OS | Installed executable | Registration (created by the package) |
| --- | --- | --- |
| Windows | `C:\Program Files\THUnet\bin\thunet.exe` | Windows Installer entry in *Settings → Apps*, Start Menu entry under `THUnet`, desktop shortcut, and `C:\Program Files\THUnet\bin` appended to the system `PATH` |
| Linux | `/usr/bin/thunet` | `/usr/share/applications/thunet.desktop`, icon under `/usr/share/icons/hicolor/scalable/apps/` |

The MSI is per-machine (`Scope="perMachine"`), which is what a single UAC
prompt at install buys: the package owns machine-wide state, so it can stop the
core service before files are replaced and delete it on uninstall — a per-user
package runs unelevated and can do neither, leaving a service behind that points
at a deleted executable. The trade is that install, upgrade and removal all ask
for administrator rights once.

Settings are still **per user** (`%APPDATA%\thunet`), and the service is still
registered by the application when you enable autostart, because it has to point
at *your* config directory.

Uninstalling while THUnet is running is safe: the MSI is elevated, so it stops
the service (`ControlService`, then waits for it to be gone), terminates the tray
(`taskkill /IM thunet.exe /F` before `InstallValidate`), removes the service on
a real uninstall, and then removes the files, the shortcuts, the Start Menu
folder and the install directory. A major upgrade stops the service the same way
but keeps the registration — and then points it at the executable that was just
installed, so a package that moves the file does not leave the SCM launching a
stale path; the tray logon task is rewritten the same way on the next launch.

Because that `taskkill` also runs for upgrades and repairs, every interactive
install (UILevel ≥ 4) starts the app again once the transaction is finished. It
does so through the desktop shell's own `ShellExecute` (the WiX util extension's
`WixUnelevatedShellExec`), which runs the tray with the user's own token instead
of inheriting the installer's elevation — asking `explorer.exe` to do it opened a
folder window instead of the app on some machines. A silent install
(`msiexec /qn`, winget) does not start anything, so it never hijacks a desktop.
Double-clicking the package while it is already installed therefore ends with
the app running rather than with a killed copy.

Autostart is **not** part of the package — the app registers the entries itself
and reads the state back from the OS instead of storing it in `config.json`.
There are two entries (tray and background service) and three modes:

| Mode | Linux | Windows |
| --- | --- | --- |
| **Off** | nothing registered | nothing registered |
| **Service** | systemd user unit `thunet.service` enabled; with `loginctl enable-linger` it also starts at boot, before anybody logs in | a `thunet` **service** (LocalSystem, start type `auto`): up before the login screen, no tray icon |
| **Service + tray** | unit + `~/.config/autostart/thunet.desktop` | that service + a `thunet-tray` logon task |

There is deliberately no tray-only mode: the tray never starts a core by
itself, so an icon without a service could only report "not running".

| OS | Autostart entry |
| --- | --- |
| Windows | The core is a **service** (`thunet`, LocalSystem, `SERVICE_WIN32_OWN_PROCESS`) that the Service Control Manager starts at boot — before anyone logs on, which a logon task cannot do. The tray stays a per-user Task Scheduler logon task (`schtasks /Create /XML`, no delay, `InteractiveToken` + `LeastPrivilege`), which is what keeps the location permission working. Registering or reconfiguring the service takes administrator rights (one UAC prompt); starting and stopping it does not. |
| Linux | Tray: `~/.config/autostart/thunet.desktop`, started by the desktop session (XDG autostart), which every desktop honours, including the ones that never pull `graphical-session.target`. Core: a systemd user unit enabled per user — no root involved. |
| macOS | A LaunchAgent managed by the `auto-launch` crate; the core itself is started on demand (`thunet core`, `thunet service start`, or the status page's *Start* button). |

The unit file is written on first start even with autostart off, so
`systemctl --user start|stop|status thunet` works without visiting the
settings page. `thunet service <action>` wraps both platforms in the same verbs:

| Action | Linux | Windows |
| --- | --- | --- |
| `install` / `uninstall` | write / remove the unit | create / delete the service, administrator only |
| `enable` / `disable` | put it in the login/boot path (and try `enable-linger`) | set the service start type to `auto` / `demand`, administrator only |
| `start` / `stop` / `restart` | `systemctl --user …` | `StartServiceW`, `ControlService` — allowed for the signed-in user |
| `status` | `systemctl --user status` | `QueryServiceStatusEx` + `QueryServiceConfig` |

Once the core is registered it belongs to systemd / the Service Control Manager:
the window only reconnects to it and never spawns one behind the manager's back.
`enable` and `disable` only decide what happens at the **next** boot; they never
start or stop the running core — that is what `start` and `stop` are for.

On Linux the unit is written on first start even with autostart off, so
`thunet service start|stop|status` works right away. On Windows the service is
created by `thunet service install` (or by picking a service mode), and
`start|stop|status` then work without any further prompt.

The only self-service action left is the desktop shortcut: **Create desktop
shortcut** on the Settings page, or `thunet shortcut`, which puts a launcher for
the running executable on your desktop (on Linux it also marks the `.desktop`
file as trusted so GNOME will launch it).

To remove THUnet: *Settings → Apps → THUnet → Uninstall* on Windows, or
`sudo apt remove thunet` on Debian/Ubuntu. Your settings in `%APPDATA%\thunet`
are left alone; delete that directory if you want a clean slate. Uninstalling
needs no preparation on Windows: the package stops the tray and the core
service, deletes both registrations (the service and the `thunet-tray` logon
task) and removes the `PATH` entry. Turning *Start with the system* off first
is harmless but no longer required.

## 8. Installing with winget

winget can install THUnet if a package manifest exists for it. There are three
levels, from "just for me" to "available to everyone":

### 8.1 Already published (nothing to do)

Once the manifests are merged into the community repository this is all it
takes:

```
winget install --id THUnet.THUnet -e
winget upgrade --id THUnet.THUnet -e
winget uninstall --id THUnet.THUnet -e
```

### 8.2 Install from the local manifests (no publishing)

This repository generates the manifests next to the MSI:

```
pixi run -e package winget         # -> dist/winget/*.yaml
```

Then, **in an elevated terminal** (the feature is off by default):

```
winget settings --enable LocalManifestFiles
winget validate --manifest dist\winget
winget install --manifest dist\winget
```

The generated `InstallerUrl` points at a GitHub release that has to exist;
pass `-BaseUrl` to change it:

```
pwsh -File scripts/winget_manifest.ps1 -BaseUrl https://github.com/<you>/thunet/releases/download/v<version>
```

### 8.3 Publishing so anyone can `winget install THUnet.THUnet`

1. Upload `dist/thunet-<version>-x64.msi` to a release; keep the asset name
   stable — the manifest hash must match the exact bytes.
2. Regenerate the manifests with the real `-BaseUrl`, so `InstallerSha256`
   matches the uploaded file.
3. Submit them, either with
   [`wingetcreate`](https://github.com/microsoft/winget-create):
   `wingetcreate submit --token <GitHub PAT> dist/winget`, or by opening a pull
   request that adds the three YAML files under
   `manifests/t/THUnet/THUnet/<version>/` in
   [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs).
4. After the automated validation passes and the PR is merged, `winget search
   THUnet` finds the package and install/upgrade/uninstall work as in 8.1.

Things the community repository will check: the installer URL must be public
HTTPS, the SHA256 must match, the package must install **and** uninstall
silently (`/qn` — the MSI does), and `Publisher` / `PackageName` should match
what the installer writes to *Installed apps* (`THUnet contributors` / `THUnet`).

Two details exist only for the installation-validation step, and removing them
brings back the `Validation-Executable-Error` label:

- `packaging/thunet.wxs` sets `ARPINSTALLLOCATION` so *Installed apps* records
  an `InstallLocation`; MSI never writes one on its own, and without it nothing
  can locate the installed executable.
- `scripts/winget_manifest.ps1` declares `thunet.exe` as the `InstallationMetadata`
  `launch` file with `InvocationParameter: --version`. THUnet is a tray client:
  started without arguments it stays resident until the user quits, which reads
  as a hung/failed program for anything that executes installed binaries.
  `--version` prints the version and exits 0 in well under a second.

> A package cannot be published for a version whose artifact is not reachable
> yet, so upload the release **before** opening the pull request.

Steps 1–3 are automated by [`.github/workflows/release.yml`](.github/workflows/release.yml):
push a tag that matches `Cargo.toml` (`git tag v0.1.1 && git push origin v0.1.1`)
and the workflow builds the MSI, publishes it with the generated manifests, and
opens the `winget-pkgs` pull request when a `WINGET_TOKEN` secret (classic PAT
with `public_repo`) is configured.

## 9. How the network login actually works

Everything is plain HTTP(S) against the portal — no browser and no JavaScript.
The full walkthrough lives in **[docs/protocol.md](docs/protocol.md)**: network
detection, `ac_id` discovery, the challenge token, `{MD5}` / `{SRBX1}` encoding,
the checksum, session verification, keepalive and the portal error codes.

The implementation is in `src/srun.rs` (protocol) and `src/session.rs` (state
machine); `src/crypto.rs` holds MD5/SHA-1/XXTEA. Nothing there is required to
use the app.

## 10. Building from source

Prerequisites: Rust 1.90 or newer (edition 2024) and a C toolchain for the
windowing dependencies. The repository pins everything through
[pixi](https://pixi.sh):

```
pixi run -e dev cargo build --release
pixi run -e dev cargo test
pixi run -e dev cargo fmt
```

The release profile uses `lto`, `opt-level = "z"`, `codegen-units = 1`,
`panic = "abort"` and `strip`, so the result is a single self-contained
executable with no runtime assets — the window, tray icon and fonts are all
created at runtime.

### Building the installers

Packaging lives in the `package` pixi environment and produces everything in
`dist/`:

```
pixi run -e package msi        # Windows: thunet-<version>-x64.msi       (WiX v5)
pixi run -e package deb        # Debian/Ubuntu: thunet_<version>-1_amd64.deb
pixi run -e package tarball    # any Linux: thunet-<version>-linux-x64.tar.gz
```

Sources:

| Path | Purpose |
| --- | --- |
| `packaging/thunet.wxs` | WiX v5 source of the per-machine MSI |
| `scripts/build_msi.ps1` | Reads the version and drives `wix build` |
| `scripts/build_tarball.sh` | Reads the version and packs the tarball |
| `Cargo.toml` → `[package.metadata.deb]` | Debian control fields and file list |
| `build.rs` | Embeds `assets/thunet.ico` (and the version block) into the exe |
| `assets/thunet.ico`, `assets/thunet.svg` | Icons used by the packages and shortcuts |
| `assets/thunet.desktop` | Launcher entry installed by the `.deb` |
| `assets/completions/` | bash/zsh/fish completion scripts installed by the `.deb`; a unit test keeps them in sync with the CLI |
| `scripts/make_icon.py` | Regenerates the icon assets (`pixi run -e tools python scripts/make_icon.py`) |

Notes:

- **The version has one source of truth: `Cargo.toml`.** The packaging scripts
  read it and pass it on (`wix build -d Version=…`, `cargo deb`, the tarball
  name), so nothing has to be edited by hand when bumping. An ad-hoc build can
  override it: `pwsh -File scripts/build_msi.ps1 -Version 0.2.0` or
  `bash scripts/build_tarball.sh 0.2.0`.
- `dotnet` and WiX are installed inside `.pixi/` — nothing is added to your
  global .NET tool list and no admin rights are needed.
- Binaries are unsigned; Windows SmartScreen will warn on first run until a code
  signing certificate is used (`signtool sign /fd sha256 ...`).

## 11. Project layout

| File | Responsibility |
| --- | --- |
| `src/main.rs` | CLI entry, console/panic plumbing, GUI launch |
| `src/srun.rs` | portal HTTP client, `ac_id` detection, request building |
| `src/crypto.rs` | MD5, SHA-1, XXTEA and the portal base64 alphabet |
| `src/session.rs` | state machine: refresh, login, keepalive, logout, backoff |
| `src/ui.rs` | worker thread, window, tray, toasts, log strip |
| `src/config.rs` | settings load/save, platform paths |
| `src/secret.rs` | sealed credential file (one implementation for every platform) |
| `src/single.rs` | single-instance guard (second launch shows the first window) |
| `src/shortcut/` | desktop shortcut creation per platform |
| `src/i18n.rs` | Chinese/English strings |
| `src/logline.rs` | log record and `[YYYY-MM-DD HH:MM:SS] [LEVEL] msg` rendering |
| `src/wifi.rs` | current SSID, enterprise-Wi-Fi detection |
| `docs/protocol.md` | SRun3K login walkthrough (see §9) |
| `packaging/`, `scripts/` | installer sources and helper scripts |

## 12. Limits and notes

- Only the campus portal protocol is implemented; there is no 802.1X or VPN
  support.
- The saved password is sealed with a key bound to this machine (`credential.bin`,
  mode `0600`): useless off this machine, but code running as you can decrypt
  it, so treat your account on this machine as trusted.
- **"Connected before login" assumes the home directory is readable at boot.**
  File permissions only test the uid, not whether a login session exists, so
  `systemd --user` (started at boot by `enable-linger`) reads
  `~/.config/thunet/credential.bin` as you. If the home directory itself is only
  unlocked or mounted at login (`systemd-homed`, ecryptfs / gocryptfs, a network
  home directory behind autofs), the file does not exist at boot and the core
  falls back to connecting after a login.
- The client trusts the portal's TLS certificate; `--http` exists for debugging
  only.
- `netsh`-based SSID detection needs Windows Location permission. Without it the
  client still works, but it cannot tell `Tsinghua-Secure` from a captive Wi-Fi —
  the internet probe then decides.
- On Windows the core runs as a **service** under LocalSystem, because only a
  service can start before a login. It reads the same `%APPDATA%\thunet` you do
  (the path travels as `--state-dir`), and its control pipe is machine-wide with
  a DACL that lets the signed-in user drive it: anyone who can log on locally
  can start or stop the session. Registering the service itself stays
  administrator-only.
- The single-instance guard brings the existing window back on every platform:
  Windows rings a session-local event, and on Linux/macOS the second launch
  leaves a marker file that the running instance polls, so the window is raised
  from inside the process that owns it.
- After an upgrade plus a service restart (`systemctl --user restart thunet`),
  a window still running the old build compares the two build stamps and
  relaunches itself on the new binary. A restart with the same build, or a copy
  whose own file was not replaced, leaves the window untouched.
- Right-click in the log strip copies whole lines; free-form selection copy uses
  the usual `Ctrl+C`.

## 13. License

MIT — see [LICENSE](LICENSE). The name "THUnet" refers to the campus network,
not to an affiliation with Tsinghua University.
