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
   | Windows | `thunet-<version>-x64.msi` | double-click, `msiexec /i thunet-<version>-x64.msi`, or `winget install --id THUnet.THUnet -e` once published (see [Installing with winget](#8-installing-with-winget)) |
   | Debian / Ubuntu | `thunet_<version>-1_amd64.deb` | `sudo apt install ./thunet_<version>-1_amd64.deb` |
   | other Linux | `thunet-<version>-linux-x64.tar.gz` | unpack and run `./thunet` (optionally `./thunet shortcut` for a desktop launcher) |

2. **Run it.** Start `THUnet` from the Start Menu / application menu, or run
   `thunet` in a terminal. A small window opens; everything the app does is
   written to the log strip at the bottom of that window.
3. **Open Settings.** Click the button in the top-right corner of the window.
4. **Enter your credentials.** Type your TUNET account (for example
   `1234567890`) and your password, tick **Remember password**, then click
   **Apply**. On Windows the password is handed to Credential Manager — it never
   ends up in the settings file.
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
| Start with the system | Adds or removes the autostart entry for your user |
| Start minimised to tray | Independent of the line above: autostart can stay hidden while a manual launch opens the window, or the other way round |
| Auto connect | Log in as soon as a usable network appears |
| Create desktop shortcut | Puts a launcher for the running executable on your desktop (the same action as `thunet shortcut`) |
| Account / Password / Remember password / Apply / Revert | This is the only row that does not save as you type: the button turns into the accent colour and an *Unsaved* marker appears while it differs from what is stored. Applying hands the password to the OS credential store (never to the JSON file) and passes it to the running session immediately; *Revert* drops the edits and restores the stored account, password and checkbox (it is greyed out while *Remember password* is off, since there is nothing stored to go back to) |
| Language | Follow system (default), 中文, English |
| Log level | `DEBUG` / `INFO` / `WARN` / `ERROR`: the lowest level shown in the log page (`INFO` by default, stored in `config.json`) |
| Version | Version on the left, build time on the right (local clock to the second, with its UTC offset) |

The three switches confirm the state they just moved to in a toast
(*Start with system: On*), because nothing else on the page changes when one of
them is flipped.

**Connect** uses the password the background worker holds: at launch that is what
Credential Manager has (only if *Remember password* was saved), and afterwards
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

**Right-click** for `Show window`, `Hide window`, `Connect`, `Disconnect`,
`Autostart` and `Quit`.

Closing the window hides it — the app keeps working. Only `Quit` really exits.

### Feedback messages

Every action answers with a coloured strip at the bottom of the window: **green**
for success, **red** for failure, **blue** for information such as "already
online", "copied", or the state a switch just moved to
("Start with system: On"). Nothing succeeds silently.

## 3. Everyday tasks

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
then carries `--minimized`).

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
thunet status                # JSON session / traffic dump
thunet login [--force]       # authenticate once
thunet logout                # de-authenticate
thunet keepalive --interval 30 --count 5
thunet run [--once]          # headless auto-connect loop
thunet ac-id                 # print the detected portal ac_id
thunet config                # config path + effective settings
thunet shortcut              # create a desktop shortcut
thunet selftest              # note: the real test vectors live in `cargo test`
```

Starting `thunet` while it is already running does **not** start a second copy:
the existing window is brought back instead.

Global overrides: `--host <host>`, `--http`, `--timeout <secs>`, `-u/--username`,
`-p/--password`, `--ip <addr>`, `--ac-id <id>`.

Precedence is **command line > config file > credential store**, so
`thunet -u 1234567890 -p secret status` works without touching your saved
settings.

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
| `credential.json` | macOS and Linux only: the saved password, mode `0600`. Windows uses Credential Manager instead. |
| `crash.log` | Appended only when the app panics. |

Password storage by platform:

| OS | Store |
| --- | --- |
| Windows | Credential Manager, generic credential, target `thunet/auth4.tsinghua.edu.cn` (visible under *Control Panel → Credential Manager → Windows Credentials*) |
| macOS / Linux | `credential.json` in the config directory, readable only by your user — no portable OS keyring is linked |

Deleting `config.json` resets every setting. Uninstalling the package leaves this
directory alone, so your account and preferences survive a reinstall; delete the
directory and the Credential Manager entry for a clean slate.

## 7. Installing, uninstalling, shortcuts

**THUnet is installed and removed by the operating system only** — the `.msi` on
Windows, the `.deb` on Debian/Ubuntu. The application itself never copies files,
writes uninstall entries or registers itself; there is no "install for me"
button and no `thunet install` command. That keeps one owner per installation
and avoids the two registrations fighting over the same files.

| OS | Installed executable | Registration (created by the package) |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\thunet\thunet.exe` | Windows Installer entry in *Settings → Apps*, Start Menu entry under `THUnet`, desktop shortcut |
| Linux | `/usr/bin/thunet` | `/usr/share/applications/thunet.desktop`, icon under `/usr/share/icons/hicolor/scalable/apps/` |

The MSI is per-user (`Scope="perUser"`): no administrator rights and no machine-wide
state, so every account on a shared machine keeps its own settings. It installs
to `%LOCALAPPDATA%` rather than `Program Files`, because writing there would
require UAC elevation for install, upgrade *and* removal — a poor trade for a
single-file tray app.

Uninstalling while THUnet is running is safe: the MSI terminates the running copy
first (`taskkill /IM thunet.exe /F` before `InstallValidate`), then removes the
files, the shortcuts, the Start Menu folder and the install directory.

Because that `taskkill` also runs for upgrades and repairs, every interactive
install (UILevel ≥ 4) starts the app again once the transaction is finished; a
silent install (`msiexec /qn`, winget) does not, so it never hijacks a desktop.
Double-clicking the package while it is already installed therefore ends with
the app running rather than with a killed copy.

Autostart is **not** part of the package — it is a user preference stored by the
app:

| OS | Autostart entry |
| --- | --- |
| Windows | A per-user Task Scheduler logon task named `thunet` (`schtasks /Create /XML`, no delay, least privilege) that runs `thunet.exe` (plus `--minimized` when *Start minimised* is on). The Task Scheduler service fires it before `explorer.exe` processes `Run` keys, so the app comes up earlier than a `Run` entry would. |
| Linux | `~/.config/autostart/thunet.desktop`, started by the desktop session itself (XDG autostart). A systemd user unit could be launched slightly earlier, but it would race the tray host and the display socket, so the `.desktop` entry is both the standard and the reliable choice. |
| macOS | LaunchAgent managed by the `auto-launch` crate |

Releases before this one wrote `HKCU\...\CurrentVersion\Run\thunet` on Windows;
the first start of a newer build moves that value into the logon task (keeping
the executable it recorded) and deletes it, so the app never starts twice.

The only self-service action left is the desktop shortcut: **Create desktop
shortcut** on the Settings page, or `thunet shortcut`, which puts a launcher for
the running executable on your desktop (on Linux it also marks the `.desktop`
file as trusted so GNOME will launch it).

To remove THUnet: *Settings → Apps → THUnet → Uninstall* on Windows, or
`sudo apt remove thunet` on Debian/Ubuntu. Your settings in `%APPDATA%\thunet`
are left alone; delete that directory (and the Credential Manager entry) if you
want a clean slate. Turn *Start with the system* off before uninstalling if you
do not want the leftover autostart entry (on Windows that is the `thunet` task
in Task Scheduler) to point at a missing executable.

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
| `packaging/thunet.wxs` | WiX v5 source for the per-user MSI |
| `scripts/build_msi.ps1` | Reads the version and drives `wix build` |
| `scripts/build_tarball.sh` | Reads the version and packs the tarball |
| `Cargo.toml` → `[package.metadata.deb]` | Debian control fields and file list |
| `build.rs` | Embeds `assets/thunet.ico` (and the version block) into the exe |
| `assets/thunet.ico`, `assets/thunet.svg` | Icons used by the packages and shortcuts |
| `assets/thunet.desktop` | Launcher entry installed by the `.deb` |
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
| `src/secret.rs` | credential store backends |
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
- On Windows the password is protected by the OS credential store; on macOS and
  Linux it is a `0600` file, so treat your home directory as trusted.
- The client trusts the portal's TLS certificate; `--http` exists for debugging
  only.
- `netsh`-based SSID detection needs Windows Location permission. Without it the
  client still works, but it cannot tell `Tsinghua-Secure` from a captive Wi-Fi —
  the internet probe then decides.
- The single-instance guard activates the existing window on Windows. On Linux
  and macOS a second launch simply exits: raising another process's window is not
  portable, so use the tray icon instead.
- Right-click in the log strip copies whole lines; free-form selection copy uses
  the usual `Ctrl+C`.

## 13. License

MIT — see [LICENSE](LICENSE). The name "THUnet" refers to the campus network,
not to an affiliation with Tsinghua University.
