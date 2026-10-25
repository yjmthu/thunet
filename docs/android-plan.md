# Android port: execution checklist

# Android port: execution checklist

Status: draft (2026-10-25). Working document in Chinese; translate before merging into README if it stays.

## 0. Scope and fixed decisions

- **v1 (must ship)**: one Activity, egui UI, the core runs inside the same process, no resident background service and no persistent notification. Login on launch, keep retrying while the process is alive, a live network callback for every change while alive, and a system-held one-shot network callback (`registerNetworkCallback(request, PendingIntent)`) that is armed only while the matching Wi-Fi is absent, so a Wi-Fi connect can still cold-start the app after it was killed.
- **Background is a three-level setting, default off.** *Off* changes nothing (no notification ever). *Wi-Fi change* adds the wake-up path but still no notification. *Resident* runs a foreground service with a low-importance, silent, minimisable notification; it is the only level that can survive a swipe from recents or start at boot.
- **Explicitly out of scope**: push wake-up (there is no server to send it, and the portal only supports polling; the system-held network callback below is not push), `VpnService` pretending to be a VPN, AccessibilityService / device-admin tricks, silent boot auto-start.
- **Continue with Rust and egui**. The protocol and session layers stay in Rust; the UI is the existing egui code with a mobile layout pass, not a rewrite.
- **Keep `eframe` 0.37 during the port.** Upgrading egui is a separate change with its own risk, especially across 5,000 lines of UI; do it before or after the port, never in the same step.
- **Latest SDK, no legacy support.** Do not add compatibility branches for old Android versions; raise the floor instead.
- **Layout is chosen by available width, not by `cfg(target_os)`.** The same code then renders on the phone, the emulator and a desktop preview window, and a layout bug can be found without a device.

| Item | Value | Consequence |
| --- | --- | --- |
| `compileSdk` / `targetSdk` | 36 (Android 16) for the first pass. The SDK Manager also offers `platforms/android-37.0`, `37.1` and `37.2` (Android 17 with minor versions), which are too new to bet the port on; revisit before the first release | opts into every current restriction: edge-to-edge is mandatory, the foreground-service types of Android 14/15/16 apply, 16 KB native pages are required |
| `minSdk` | 33 (Android 13) | Android 12 and older cannot install; in exchange there are no legacy storage, notification or permission branches. Lowering it later is a one-line manifest change as long as no 33-only API is used |
| ABI | `arm64-v8a` (release), `x86_64` (emulator) | no `armeabi-v7a`, no 32-bit code |
| NDK | r28 or newer | 16 KB page alignment by default; required for `targetSdk` 35+ |
| Gradle / AGP / Kotlin | the latest stable combination the SDK Manager or Android Studio offers | do not pin old build tools |
| Distribution | sideload (APK) | no store review, no Play policy limits on the foreground-service type; users enable "install unknown apps" and update manually |

Why this shape works: the project already has a "portable" mode where the core is hosted in a thread of the window process (`ipc::host_core()`, `src/ipc.rs:1401`) and both halves talk over a local socket. Android is that mode with a different endpoint path and a different startup entry point, which is far cheaper than inventing a new frontend/core API.

## 1. Phase 0: feasibility spike (before touching the repository)

Build a throwaway crate under `target/android-spike/` (gitignored, so nothing lands in history) with only `eframe`, `ureq`, `interprocess` and the minimum platform code. Do not restructure the real crate until all seven checks pass.

Environment (installed 2026-10-25):

```bash
# pixi env `android`: openjdk 21.0.10 + Gradle 9.3.1 (`pixi run -e android ...`).
# AGP 9.4.1 requires Gradle 9.6.0, so the Android project carries its own
# wrapper: `pixi run -e android -- ./gradlew assembleDebug` (pixi supplies the
# JDK, the wrapper supplies Gradle).
# SDK root: ~/Android/Sdk, command-line tools 23.0.0. Newer sdkmanager uses `/`
# in package paths; older ones use `;` (`platforms;android-36`).
SDK=/home/yjmthu/Android/Sdk
$SDK/cmdline-tools/latest/bin/sdkmanager --sdk_root=$SDK \
  "platform-tools" "emulator" \
  "system-images;android-36;google_apis;x86_64" \
  "platforms/android-36" "build-tools/36.0.0" "ndk/29.0.14206865"

# Rust targets belong to the rustup toolchain in ~/.cargo/bin (not the pixi
# `rust` package, which carries no cross std). cargo-ndk 4.1.2 is installed.
~/.cargo/bin/rustup target add aarch64-linux-android x86_64-linux-android
~/.cargo/bin/cargo install cargo-ndk
```

- [x] **S1: egui renders on a real device.** Passed on API 36 (emulator, `Renderer::Glow`): the window opens, Chinese text renders, rotation does not crash. `eframe` 0.37 needs the `android-native-activity` feature and `NativeOptions.android_app`; the entry point is `#[unsafe(no_mangle)] pub extern "C" fn android_main(app: AndroidApp)`.
- [x] **S1b: edge-to-edge and insets.** `AndroidApp::content_rect()` returned the whole window (`l0 t0 r1080 b2400`) under edge-to-edge, so it is **not** an inset source. The working approach is a Java `NativeActivity` subclass with an `OnApplyWindowInsetsListener`: on the emulator it reported `top 121 / bottom 63` physical pixels, and `ime 883` once the keyboard opened. Apply those values as padding; the IME inset is the bottom inset that matters for a focused text field.
- [x] **S2: app-private storage.** Wrote and read `files/spike.txt` at `/data/user/0/<pkg>/files`.
- [x] **S3: local socket.** A filesystem Unix socket at `files/control.sock` (61-byte path) accepts, echoes and closes inside the app sandbox; no SELinux problem.
- [x] **S4: networking.** Plain HTTP returned 204 and HTTPS returned 200 through rustls + webpki-roots. Note: `timeout_global` did **not** cover the body read (a large page hung the worker); the real client must set `timeout_recv_body` (and `timeout_recv_response`) explicitly.
- [x] **S5: fonts and keyboard.** `/system/fonts/NotoSansCJK-Regular.ttc` exists on API 36 and renders Chinese correctly. `egui::TextEdit` receives typed text and the IME inset arrives; no layout tearing.
- [x] **S6: wake-up after task removal.** Mechanism confirmed, but weaker than hoped: see the wake-up section in Phase 5. A `PendingIntent` request is **one-shot per registration** (`ConnectivityService.mPendingIntentSent`), it survives process death, and it does cold-start the app when the registration happened while Wi-Fi was off.
- [ ] **S7: same-SSID roaming.** With logging registered on both `onAvailable` and `onLinkPropertiesChanged`, walk from the canteen AP to the dorm AP (same SSID): record whether the network object changes, whether the IP changes, and which callbacks fire. This decides whether a roam needs a login at all. Needs a physical device.

Spike result: S1, S1b, S2, S3, S4, S5 and S6 pass; S7 is the only open check. The spike lives in `target/android-spike/` and is not part of the repository.

Exit criteria: all seven pass, or the failing one changes the plan (S1 twice → evaluate a Kotlin UI; S3 → the in-memory `Link` fallback in section 9; S5 fonts → embed a subset font; S6 → v1 keeps the WorkManager fallback as the primary path and the README stops promising auto-login after a swipe; S7 → document the roam behaviour that was measured, do not assume one).

### Preview workflow (no separate virtual machine needed)

The Android Emulator is itself a KVM virtual machine; VirtualBox, VMware or Waydroid are not needed and would add nothing. Verified 2026-10-25: 16 cores, 28 GB RAM, 459 GB free, and the user is now in the `kvm` group (`/dev/kvm` readable and writable).

- [x] Grant KVM access: `sudo usermod -aG kvm $USER`, then log out and back in. For a quick test without a re-login, `sudo setfacl -m u:$USER:rw /dev/kvm` works until the next boot.
- [x] Install the SDK without Android Studio: `sdkmanager --install "platform-tools" "emulator" "system-images;android-36;google_apis;x86_64" "platforms/android-36" "build-tools/36.0.0" "ndk/29.0.14206865"`, then `avdmanager create avd -n thunet -k "system-images;android-36;google_apis;x86_64" -d pixel_8a`.
- [x] Boot it headless for automated checks with `emulator -avd thunet -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect`; it reaches `sys.boot_completed=1` in about 80 s, `adb devices` shows `emulator-5554`, and DNS plus ICMP work through the host. Start a second, windowed instance (same `-avd` name will conflict; use `-read-only` or a second AVD) when a human needs to look at it. Build for `x86_64-linux-android` so no arm64 translation is involved.
- [ ] **Desktop mobile preview.** Add a `--preview` switch (or `THUNET_PREVIEW=1`) to the desktop binary that opens a 412x915 resizable window, skips the tray, and uses the same width-driven layout. This is the fast loop for UI work; it cannot show real insets, IME behaviour or touch, so the emulator and a real phone still gate the release.
- [ ] Emulator limits to write down: its Wi-Fi is a single virtual AP (`AndroidWifi`) and cannot simulate the campus SSID or a roam; its network is NATed through the host, so if the host is already authenticated the probes report `DirectInternet` and no login is attempted. Test the protocol against a local mock portal (`docs/protocol.md`) through a debug-only host override, and keep the real network for a physical device. One useful accident: the emulator's Wi-Fi reports `INTERNET` with `PARTIAL_CONNECTIVITY` and without `VALIDATED`, which is exactly the capability set a captive-portal network has, so S6 can be exercised there.
- [ ] Physical device over `adb` remains the only way to verify the real portal, the roam and the OEM kill behaviour.

Acceptance: the emulator runs the APK; the desktop preview renders the same layout without the tray; a physical device logs in to the real portal.

## 2. Phase 1: split the crate into `lib` + thin `bin` (desktop behaviour unchanged)

The desktop CLI (`src/main.rs`, 1333 lines) is all `clap`/service/console plumbing. It must stop being the crate root so Android can link the reusable half.

- [x] Create `src/lib.rs` with the module declarations currently at the top of `src/main.rs`, and with the platform modules gated: `#[cfg(not(target_os = "android"))] pub mod desktop;`, `#[cfg(windows)] mod winsvc;`. `exe_path()` stays at the crate root because `ui`, `single`, `elevate`, `autostart` and `shortcut` all call `crate::exe_path()`.
- [x] Move the body of `src/main.rs` into `src/desktop.rs` as `pub fn main()`, keeping the crate-level `#![cfg_attr(windows, windows_subsystem = "windows")]` in `src/main.rs`. The bare module paths became `use crate::…`; `elevate` is only ever written as `crate::elevate::…`, so it is not imported.
- [x] Reduce `src/main.rs` to a shim so the binary compiles for every target:

```rust
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() {
  thunet::desktop::main();
}

#[cfg(target_os = "android")]
fn main() {}
```

- [x] In `Cargo.toml`, declare `[lib] name = "thunet"`. `crate-type = ["rlib", "cdylib"]` is deferred to the Android packaging step, so a desktop build does not produce an unused `.so`.
- [ ] Move the desktop-only dependencies under `[target.'cfg(not(target_os = "android"))'.dependencies]`: `clap`, `clap_complete`, `rpassword`, `ctrlc`, `tray-icon`.
- [ ] `auto-launch` and `libc` currently sit in `[target."cfg(not(windows))".dependencies]`. Narrow that block for `auto-launch` to `cfg(all(not(windows), not(target_os = "android")))`, because `autostart` is stubbed on Android; keep `libc` available, since `secret.rs` and `ident.rs` use it.
- [ ] Gate `wifi`, `single`, `elevate`, `shortcut`, `autostart`, `prompt` with stubs (Phase 2.3) rather than removing them: `ui.rs` references all of them, and rewriting those call sites costs more than a stub.

Acceptance: passed on 2026-10-26. `cargo fmt --check` clean, `cargo check --all-targets` clean, `cargo test` 108 passed / 2 ignored, `cargo build` clean, `cargo run -- --version` prints `thunet 0.1.5`. The only warning was an unused `elevate` import in `desktop.rs`; it was removed afterwards, for the next build to confirm.

## 3. Phase 2: platform seams (no Android UI yet)

The goal is `cargo check --lib --target aarch64-linux-android` passing with every desktop module compiling or stubbed.

### 2.1 Tray abstraction (the only real refactor)

There is no tray on Android, but `ui.rs` used to drive one directly.

- [x] Added `src/tray.rs`, a façade with `Tray::new(title, texts, fill)`, `available()`, `set_texts`, `set_enabled`, `set_color`, `set_tooltip` and `poll() -> Option<Action>`. The desktop implementation wraps `tray-icon`; the Android one is inert.
- [x] `ui.rs` no longer imports `tray_icon`; the menu items, the icon colour, the tooltip and the enabled state all go through the façade.
- [x] The single/double click burst and its test moved into the façade; Windows still counts clicks itself because tray-icon never emits `DoubleClick`.
- [x] `icons::make_icon` and its `tray_icon::Icon` import are gated to non-Android builds.
- [x] `tray-icon` moved to `[target.'cfg(not(target_os = "android"))'.dependencies]`.

### 2.2 Notification abstraction

- [ ] Deferred to Phase 6: nothing calls it before the keep-alive service exists, and an unused module would only add dead-code warnings. The façade API is already planned there.

### 2.3 Stubs for the desktop-only modules

- [x] `autostart`: an Android `imp` reports "not supported" for the mutating verbs (`Mode::Off`, `prepare_service` false, `service` an error). `auto-launch`'s Android build is a stub without `enable`/`disable`, which is why the whole non-Windows imp is gated to `all(not(windows), not(android))`.
- [x] `single` and `elevate` needed no stub: their Unix code uses only `std` (`single`) or `Command::new("pkexec")` (`elevate`), and neither is reachable on Android once the service UI is hidden.
- [x] `shortcut` already had a non-Windows/non-Linux fallback returning an error; `APP_TITLE` stays.
- [x] `prompt` is used only by the desktop CLI; it compiles but is not reached from the UI.
- [ ] Hiding the autostart/service rows in the settings page is a UI change and belongs to Phase 3/5.

### 2.4 Link and Wi-Fi knowledge

- [x] `srun::link_id()` works unchanged: it only connects a UDP socket, which is a route lookup.
- [x] `srun::link_kind()` reads `files/link.txt`, which `ThunetActivity` keeps up to date from `ConnectivityManager.registerDefaultNetworkCallback`: `wlan`, `ethernet`, `other` or `none`, with the route table as the fallback before the first write.
- [x] `wifi::classify()` compiles through the same fallback and returns `NoWifi` (no note, no log line), so no SSID hint is produced and no SSID permission is requested. The Phase 2 note that it returns `Unknown` was wrong: the Android stub returns `Ok(None)`.
- [x] `ident` compiles with `SO_PEERCRED`; in portable mode both ends are the same uid.

### 2.5 Paths

- [x] `config::state_dir()` has an Android branch falling back to `std::env::temp_dir()`; the real directory is injected through `THUNET_STATE_DIR` in Phase 3.
- [x] `config::machine_dir()` already falls through to the user directory on Android.
- [x] `secret.rs` mode `0600` works inside the app sandbox; backup exclusion is a Phase 5 item.

### 2.6 Cargo features

- [x] Added `android-native-activity` to the `eframe` features; it is harmless on desktop.
- [x] `x11`/`wayland` still compile for Android (verified by the check), so no split target table is needed for eframe.
- [x] Only `tray-icon` had to move; the rest of the dependency slimming (`clap`, `clap_complete`, `rpassword`, `ctrlc`) is deferred to Phase 4, since they compile for Android and only add unused build time.

Acceptance: passed on 2026-10-26. `cargo check --lib --target aarch64-linux-android` exits 0; its 326 warnings are all `dead_code` and come from the missing Android entry point (the UI tree is unreachable until Phase 3 adds `android_main`). Desktop is unchanged: `cargo fmt --check`, `check --all-targets`, `test` (108 passed, 2 ignored) and `build` all pass on Linux.

## 4. Phase 3: Android entry point and runtime

- [x] `src/android.rs`: `android_main` injects `THUNET_STATE_DIR` from `internal_data_path()`, points `diag` at `core.log`, installs a panic hook writing `crash.log`, and calls `ui::run(cfg, false, app)`.
- [x] `ui::run`/`run_with` take a `cfg`-selected `AndroidApp` (a unit type on desktop) and set `NativeOptions.android_app`; the desktop signatures are unchanged.
- [x] `ipc::endpoint()` Android branch returns `files/control.sock`; the core logs `listening on /data/user/0/<pkg>/files/control.sock (machine=false)`.
- [x] Startup order verified: `App::new` opens the link before hosting the core; the 2-second retry covers the race and the log shows `connected to the portable core`.
- [x] Window: Android skips `with_inner_size`/`with_resizable`; the 420 pt desktop layout fits the 412 pt emulator screen.
- [x] Insets: `ThunetActivity` (a `NativeActivity` subclass under `android/`) writes `top bottom ime` to `files/insets.txt`; the UI reads it, shortens the layout by the bottom inset (navigation bar or IME) and pads the top. Measured on the emulator: `top 121 / bottom 63` physical pixels.
- [x] Fonts: Android CJK candidates added; `/system/fonts/NotoSansCJK-Regular.ttc` is used.
- [x] Back: **steps back inside the window first, then sends the task to the background.** `ThunetActivity` registers an `OnBackInvokedCallback` (the manifest sets `android:enableOnBackInvokedCallback`) and also overrides `onBackPressed`; both call `Native.backPressed()`, which is true when the window has somewhere to go — an open menu or modal, the switch confirmation, or the settings page — and the window then closes or leaves that. A false answer runs `moveTaskToBack(true)`, because finishing the Activity is not an option: `android-activity` can run `android_main` only once per process, so the next launch after Back stayed on the splash screen while the core thread was still alive (observed on 2026-10-26). This supersedes the "finish the Activity" wording of decision A; see section 13.
- [x] A second `android_main` in the same process now logs `android: ui exited: ...` instead of failing silently.
- [x] Desktop-only settings rows hidden: autostart, start minimised, desktop shortcut; `app_name()` never says "Portable" on Android; the build stamp is hidden because it overflows a phone-width footer.
- [x] The real APK built and ran: `android/` Gradle project, x86_64 debug, installed on the API 36 emulator; the status page shows the live session and the settings page shows the reduced set of rows.
- [x] The background level row belongs to Phase 6; the settings page shows it on Android only.
- [x] Link row: `ConnectivityManager` reports WLAN/none through `files/link.txt` (A-L item E); the emulator shows `WLAN`.
- [x] Touch copy and hover hints (A-L item J): see section 6.

Acceptance: partially met on 2026-10-26. The debug APK starts and renders (English on the `en-US` emulator; the CJK font is loaded), and the core reads the live session through the host's already-authenticated network. A real portal login was **not** exercised: the emulator NATs through an online host and no credentials were entered. That test, and the credential-persistence check, need a physical device and belong to Phase 5.

Findings recorded:

- The debug APK is 308 MB because `libthunet.so` is unstripped; Phase 4 must set `ndkVersion` (so AGP strips it) or package a release build.
- After the entry point exists, the Android target has 63 warnings: dead code used only by the desktop service/CLI paths, plus the expected `improper_ctypes_definitions` on `android_main(app: AndroidApp)`.
- Screenshots taken on the emulator show the host's live session; they contain real identifiers and are never committed (they live in `target/`, which is ignored).

## 5. Phase 4: Android project and packaging

Keep a real Gradle project from the start, not `cargo apk`'s generated one: the keep-alive service and its permission flow need a real app module anyway.

- [x] `android/settings.gradle`, `android/build.gradle`, `android/app/build.gradle`, `android/app/src/main/AndroidManifest.xml` exist.
- [x] Manifest: `INTERNET`, `ACCESS_NETWORK_STATE`, `RECEIVE_BOOT_COMPLETED`; `.ThunetActivity` (a `NativeActivity` subclass) with the `android.app.lib_name=thunet` metadata and a MAIN/LAUNCHER filter.
- [x] Launcher icon: `scripts/make_icon.py` writes `mipmap-*/ic_launcher.png` and an adaptive `mipmap-anydpi-v26/ic_launcher.xml` (green disc from the same generator as the desktop icons, white T foreground).
- [x] ABI filters: debug carries `arm64-v8a` + `x86_64`, release `arm64-v8a` only; both are built by `scripts/android_build.sh`.
- [x] `ndkVersion '29.0.14206865'`: AGP strips `libthunet.so`, so the debug APK fell from 322 MB to 92.7 MB (29 entries, 89 MB of them the two libraries).
- [x] 16 KB pages: every `LOAD` segment of both libraries has `align 0x4000`, and `zipalign -c -P 16 -v 4` passes.
- [x] Cleartext: `usesCleartextTraffic` is gone; `res/xml/network_security_config.xml` allows plain HTTP only for the three probes and `auth4`/`login.tsinghua.edu.cn`. Unknown HTTP portals are not followed (documented in the README).
- [x] `android:allowBackup="false"`.
- [x] Signing: debug uses the debug key; release reads `android/keystore.properties` (git-ignored), which points at `~/.android/keystores/thunet-release.jks` (RSA 4096, alias `thunet`, 10,000 days). The APK is signed with the v2 scheme and verified with `apksigner`. `versionName`/`versionCode` come from `Cargo.toml` (0.1.5 → 105).
- [x] Gradle builds the core: `cargoNdkDebug`/`cargoNdkRelease` run `scripts/android_build.sh`, wired to `preDebugBuild`/`preReleaseBuild`; `-PnoRust` skips it. `pixi run -e android apk-debug` is the documented entry point (README §10).
- [ ] CI (optional): an `android` job that runs only `cargo ndk -t arm64-v8a check --lib`.

Acceptance: met on 2026-10-26. `./gradlew assembleDebug` produces an installable APK; `adb install -r` upgrades over the previous build (same debug key) and keeps `config.json`. Note for future runs: AGP's incremental packager can append a rebuilt `.so` and leave a hole, which grew one APK to 146 MB; deleting `android/app/build/outputs/apk` before packaging gives the expected size again.

## 6. Phase 5: v1 behaviour on Android

- [x] **Login on launch.** `Config::auto_connect` is true and the worker starts with the process; repeated launches log `already online` instead of re-running a login. A real portal login could not be exercised from the emulator (see the note below).
- [x] **No credentials yet.** The amber banner (`auto_connect_without_credentials[_online]`) already leads to the gear; no extra Android-only copy was needed.
- [x] **Process lifecycle.** Back sends the task to the background. The core thread keeps running while the task is hidden, and relaunching from the launcher or recents brings the same Activity and the same `App` back (no re-init, session intact). `logic` logs `app: window focused` on the focus edge (debug level).
- [x] **Logs.** `adb logcat` carries stderr (`RustStdoutStderr`), `files/core.log` carries the same lines with timestamps, and the panic hook writes `files/crash.log` (verified: no crash occurred, so the file was absent).
- [x] **Privacy.** The eye masks account and addresses and persists `privacy: true` in `config.json` (verified on the emulator).
- [x] **Link row.** `files/link.txt` from `ConnectivityManager` makes the row read `WLAN` (item E).
- [x] **Theme auto (item L).** `files/theme.txt` follows `uiMode`; switching the emulator between light and dark recoloured the running window within ~2 s.
- [x] **Touch semantics (item J).** A 450 ms hold on a log line opens its menu (Copy line / Copy all); the line is tinted from the moment the finger lands and the hold ticks the phone, so the target is never a guess. The menu is a `Popup` opened from the hold, not `Response::context_menu`: a hold is a *primary* click in egui, and `context_menu` only listens to secondary clicks. Copy goes through JNI to `ClipboardManager`: `clipboard::flush` drains every `OutputCommand::CopyText` at the end of the pass, because egui-winit's Android clipboard is in-process only.
- [x] **Disabled-button hint.** When *Apply* is disabled because the password field is empty, the desktop's hover text is shown as a gray inline line under the row.
- [x] **Credential fields (found on a real device).** `NativeActivity` forwards only key events, so an egui text field has no `InputConnection`: no IME suggestions, no system paste and no autofill by itself. The fields stay egui (`TextEdit`, same row layout as the desktop) and two bridges restore what matters. A long press reads the system clipboard and replaces the field, because a credential is one value and a phone has no visible caret. The form is also published to the autofill framework as a *virtual view structure* — `AutofillHost.onProvideAutofillVirtualStructure` in `ThunetActivity`, the same route Flutter takes for its own text fields — with username/password hints, both fields' screen rectangles and their current values; `App::autofill_update` drives `notifyViewEntered`/`notifyViewExited`/`notifyValueChanged`, and `apply_credentials` calls `commit()` so a password manager can offer to save the pair. Widget focus is read from `Memory::has_focus`, never `Response::has_focus()`: the latter also requires the *window* to be focused, and the dropdown itself takes that focus away, which made the field leave and re-enter and the dropdown flash. A move only updates the saved bounds; a tap is the one explicit "show me the suggestions again"; switching fields is a single enter so the keyboard is never hidden in between. The keyboard itself talks to `AutofillHost` through a `BaseInputConnection`: `commitText`/`setComposingText`/`deleteSurroundingText` become egui `ImeEvent`s, so a candidate such as Sogou's "red" lands in the field and the composition is drawn inline. The host is focusable, requests focus per field, and sets `EditorInfo.inputType` to password or email. Verified on the emulator: Gboard composition and commits land in the field, the password field gets the password keyboard, and the autofill session plus fill request arrive. The save prompt needs a real manager.
- [x] **Campus link with mobile data up (found on a real device).** Android keeps cellular as the default network while the campus Wi-Fi is unvalidated, so probes and logins went out over the carrier and the verdict was "no campus network". `ThunetApp` now binds the process to the connected Wi-Fi network (`bindProcessToNetwork`) from Application start, which survives the Activity being swiped away; the binding is released when that network goes away. `link.txt` reports the bound link.
- [x] **Password looked unsaved after a restart (found on a real device).** In portable mode the window's `Config::load()` reads the sealed blob, so seeding `pass_edit` with it made a saved password look like an unsaved edit and the *Unsaved* marker never cleared. `App::new` now drops the plaintext, and "saved" comes from the core's `CredentialState`; verified with a crafted blob — after a restart the row says "Saved — leave blank to keep it" with no marker.
- [x] **Secure detection (item F, revised).** An optional switch next to *Hide private info* reads the Wi-Fi name (needs `ACCESS_FINE_LOCATION`, requested when ticked) and writes it to `files/ssid.txt` only while it is on; `wifi::classify()` then tells `Tsinghua-Secure` from a portal network. Without the permission the file is empty and the probes decide, which is the old behaviour. Verified: allowing the permission wrote `AndroidWifi`, the core logged `wifi: AndroidWifi uses the portal`; turning it off made the verdict disappear again.
- [x] **System back.** The platform gesture (edge swipe on Android 10+, predictive back on 13+) and the button both reach the window: it closes the innermost thing first (log menu, switch confirmation, device list), then leaves the settings page for the status page, and only then does Java send the task to the background. There is no in-app swipe detector any more.
- [x] **Device class.** `device_class()` on Android returns `Smartphones/PDAs/Tablets`; `AndroidOS` was already sent, so the portal's device list has a type where it used to be empty.
- [ ] **Credential persistence.** The sealed blob is written only after the portal accepts a login, which needs real credentials on a real network; placeholder credentials are rejected (`E2901`) and the core then keeps no password. `config.json` (username, `remember_password`) does persist across restarts. Verify `credential.bin`/`key.bin` (both `0600`) on a physical device with the acceptance run.

**Operational note.** The emulator NATs through the host, so its portal requests carry the host's address. Applying placeholder credentials from the emulator really logged the host's portal session out and then earned an `E2901`; do not repeat that on a machine whose session matters. This is why the persistence check above needs a physical device on the campus network.

### Wake-up path: no service, no notification

Measured on Android 16 (API 36, emulator) on 2026-10-26. The system-held request works, but it is **one-shot per registration**, which changes what v1 can promise.

- The PendingIntent overload sends the intent **once** and then sets `mPendingIntentSent`; AOSP `ConnectivityService.sendPendingIntentForRequest` refuses every later availability for that request. A Wi-Fi drop and return does not wake the app a second time.
- The request **does** outlive the process: with the app registered while Wi-Fi was off and then `am kill`ed, the next Wi-Fi availability cold-started the process and delivered the broadcast, which completed an HTTP request inside the window. This is the case the API is good for.
- Registering while the matching network is already available consumes the shot immediately. That happened at every launch with Wi-Fi up.
- Re-registering from inside the receiver while the network is up delivers again at once, so it must not be done unconditionally.

What v1 can promise:

| Situation | Wake-up |
| --- | --- |
| App running (foreground or cached) | live `NetworkCallback`, immediate, every change |
| Killed while offline, then Wi-Fi appears | one-shot cold start, immediate |
| Wi-Fi drops while the app still runs, then the app is killed, then Wi-Fi returns | one-shot armed on `onLost`, immediate |
| Killed while online, then a Wi-Fi appears | **not guaranteed**; the 15-minute WorkManager backstop, or opening the app |

Implementation rules (completed 2026-10-26):

- [x] Keep a live `NetworkCallback` while the process lives: the Activity's `registerDefaultNetworkCallback` also arms the one-shot on `onLost`, and the service registers its own callback for `onAvailable`.
- [x] Arm the one-shot only while no matching Wi-Fi is available (`Wake.arm` checks first).
- [x] In the receiver, check whether a matching network is available before re-arming.
- [x] `NetworkRequest.Builder().addTransportType(TRANSPORT_WIFI).addCapability(NET_CAPABILITY_INTERNET)`, deliberately without `NET_CAPABILITY_VALIDATED`.
- [x] `PendingIntent.getBroadcast(..., FLAG_IMMUTABLE)` with an explicit `NetworkWakeReceiver` component.
- [x] `NetworkWakeReceiver` uses `goAsync()` and `Native.ensureOnline` on a worker thread; the Rust function writes its log line before any request.
- [x] Rust `Native.ensureOnline(filesDir)`: sets the state directory, asks a running core over the control socket (`Cmd::Connect`), otherwise runs one `Session::ensure_online()`; it never starts the event loop or the tray. A process-wide mutex keeps two entry points from logging in at once.
- [x] `BootReceiver` starts the service when the level is *Resident*, arms the one-shot when it is *Wi-Fi change*, and does nothing at *Off*.
- [x] Manifest: every receiver `android:exported="false"`.
- [ ] The `8 s` budget plus a one-time `WorkManager` follow-up from the receiver were **not** implemented: the login uses the session's own timeouts, and the 15-minute periodic job is the only retry. A run that outlives the broadcast simply leaves its log line behind.
- [x] Periodic backstop: `androidx.work` 2.12.0, 15-minute unique periodic work with a `NetworkType.CONNECTED` constraint; it checks the session when a network exists and re-arms the one-shot otherwise.
- [ ] **Roam caveat.** Two APs with the same SSID are one configured network to Android. A seamless BSSID roam (same subnet, no disconnect) produces no `onAvailable`, so nothing fires; if the IP also stays the same, no login is needed either. If the roam changes the IP, v1 relies on the periodic backstop or on the user opening the app. Measure the real campus behaviour in S7; a second registration with `NET_CAPABILITY_VALIDATED` is an experiment, not an assumption.
- [x] The boundary is written down in the README (§10): after "Force stop", nothing wakes the app; a process killed while online may wait for the 15-minute backstop.
- [x] The *Resident* level is the alternative when an immediate wake is required in every case.

Acceptance: met on the emulator on 2026-10-26. Killing the process while offline and reconnecting Wi-Fi cold-started it through the broadcast (`wake: broadcast received`, then the Rust line in `core.log`); the service's `onAvailable` asked the running core to connect; the 15-minute job appears in `dumpsys jobscheduler`. The full matrix still needs a physical device.

## 7. Phase 6: background level (default off)

The background behaviour is a three-level setting that mirrors the desktop's autostart triad, and it is deliberately separate from *Stay online*: that one decides whether the **core** signs back in when a session drops, exactly as on the desktop, and never changes here.

| Level | Android | Desktop counterpart |
| --- | --- | --- |
| `off` | nothing behind: no service, no wake-up registration, no periodic job | autostart off |
| `wake` | live callback while alive + one-shot on the next Wi-Fi + 15-minute backstop; no notification | core only |
| `service` | the above plus a `specialUse` foreground service, a silent notification, and a start at boot | core + tray |

- [x] The row is "Background" with three radios: *Off*, *Wi-Fi change*, *Resident*. The level lives in `files/background.txt` (`off` / `wake` / `service`) instead of `Config.background_keepalive`: three parties write it (the Activity, the notification action, the boot receiver) and the Rust side only reads it, so one file has one answer. `src/background.rs` reads it; `ThunetActivity.setMode` is the JNI side.
- [x] Choosing *Resident* requests `POST_NOTIFICATIONS` from the visible Activity; the service starts only after it is granted. A refusal falls back to *Wi-Fi change* and writes `files/background.denied`, which the row shows as a gray line.
- [x] Revocation is detected in `onResume`: the level drops to *Wi-Fi change* and the marker is written (verified with `pm revoke`).
- [x] `KeepAliveService` (Java, not Kotlin: the project has no Kotlin sources) with `foregroundServiceType="specialUse"` and the subtype property; `FOREGROUND_SERVICE_SPECIAL_USE` is declared.
- [x] Notification: channel `thunet-keepalive`, `IMPORTANCE_LOW`, no sound/vibration/badge, `setOngoing(true)`, `FOREGROUND_SERVICE_IMMEDIATE`, body opens the app, actions `Disconnect` and `Stop keep-alive`. *Stop keep-alive* moves to *Wi-Fi change*, because the notification is the only thing that level cannot have.
- [x] `onStartCommand` calls `Native.startCore`; `ipc::host_core()` is now idempotent, so the Activity opening later attaches to the service-hosted core instead of logging a conflict.
- [x] `Wake.apply` is the one transition point: it stops the service, unregisters the one-shot, cancels or schedules the backstop, and writes the new level. `BootReceiver` then starts the service only for `service`, arms the one-shot only for `wake`, and does nothing for `off`.
- [x] The service registers a default `NetworkCallback` and calls `Native.ensureOnline` on `onAvailable`; the `wake` level keeps the one-shot and the backstop.
- [ ] Battery: the 15/60-second cadence comparison needs a device; the core's offline probe cadence is unchanged.
- [ ] First-run per-ROM guide (MIUI/HyperOS, EMUI, ColorOS, OriginOS): not written.
- [ ] Repeated-failure amber notification text and the 8-hour measurement: deferred.
- [x] Each level states its price inline under the row, and the README states the "Force stop" boundary.

Acceptance: met on the emulator. *Off* leaves no service, no notification and no scheduled job, including across a reboot; *Wi-Fi change* arms the one-shot and schedules the backstop; *Resident* creates one silent notification, survives a swipe from recents (process and service stayed alive, the UI reattached on return), comes back after `adb reboot`, and stops cleanly from the notification action; revoking the permission drops to *Wi-Fi change* with the explanation.

## 8. Acceptance matrix (v1)

| Case | Steps | Expected |
| --- | --- | --- |
| First launch | install APK, open | window appears, Chinese text, no crash, no tofu |
| No credentials | open settings, enter account and password | saved, then online within one login round trip |
| Background 10 min | Home, wait 10 minutes, return | state refreshes; if the process was reclaimed, the app logs in again |
| Wi-Fi drop and return | toggle Wi-Fi off for 30 s, back on | online again within ~30 s while the process lives |
| Swipe from recents | swipe, reopen | no panic in `crash.log`; state and credential intact |
| Killed while offline, then Wi-Fi appears | Force stop is not used: swipe the app while Wi-Fi is off, then connect | online again without opening the app: the armed one-shot cold-starts it (measured in the spike) |
| Killed while online, then a Wi-Fi appears | swipe the app while connected, then switch SSIDs | not guaranteed: the 15-minute backstop, or immediate after opening the app. Record per ROM |
| Wi-Fi drops while the app runs, then the app is killed, then Wi-Fi returns | unplug/rejoin the SSID, swipe the app during the gap | immediate: the live callback armed the one-shot on `onLost` |
| Same-SSID roam, no disconnect | walk between two APs sharing one SSID on the same subnet | no wake-up, and no login needed: the session survives |
| Same-SSID roam, IP changes | walk between two APs sharing one SSID on different subnets | v1: backstop within 15 min, or immediate after opening the app; record what S7 measured |
| Force stop, then connect Wi-Fi | Force stop in Settings, toggle Wi-Fi | expected: nothing happens until the app is opened again (documented boundary) |
| Reboot, then connect Wi-Fi | reboot, unlock, join the campus SSID without opening the app | online again, because `BOOT_COMPLETED` re-registered the callback |
| Background: off | install, never change the setting | no notification at any point; no wake-up and no scheduled job either |
| Background: Wi-Fi change | select it | no notification; the one-shot and the 15-minute job run |
| Background: resident | select it, accept the notification permission | one silent, low-importance notification; survives a swipe on stock Android |
| Background: permission denied | deny `POST_NOTIFICATIONS`, select *Resident* | falls back to *Wi-Fi change* with an explanation; no service, no notification |
| Background: resident, screen off 8 h | leave the phone idle | still online; record the battery delta |
| Screen off 30 min, background off | lock the phone, unlock | allowed to be offline if the process was frozen; online within 30 s after reopening |
| Upgrade in place | `adb install -r` with the same signing key | settings and sealed credential preserved |
| Backup | inspect auto-backup / `adb backup` | no `credential.bin`, `key.bin`, `config.json` |
| Rotation | rotate during login | no crash, layout usable |
| Keyboard | open settings, focus password | keyboard appears, field not covered |

Measured on the API 36 emulator on 2026-10-26 (everything except the physical-device rows): first launch, no credentials, background, Wi-Fi drop and return, swipe from recents, killed while offline then Wi-Fi appears, the three background levels (off/wake/resident) including denial and revocation, reboot at each level, upgrade in place, rotation, keyboard. Still open: a real portal login, credential persistence, same-SSID roaming, 8-hour battery, and every per-ROM behaviour.

## 9. Risk register

| Risk | Likelihood | Check | Fallback |
| --- | --- | --- | --- |
| egui does not render on some device GPU | medium | S1 on the target device | try `Wgpu`; if both fail, evaluate a Kotlin UI (option C) |
| `interprocess` or the filesystem socket fails under SELinux | medium | S3 | in-memory `Link` variant: add a channel-backed implementation beside the socket one in `ipc.rs` (~100 lines), keep the same `Link` API |
| No readable CJK font | low | S5 | embed a subset font, accept 2-4 MB |
| Soft keyboard covers the field | medium | S5 and the matrix | scroll the settings page to the focused field; last resort is a Kotlin text dialog over JNI |
| Build breaks because features are not target-aware (`x11`/`wayland`) | medium | Phase 2.6 | drop those eframe features from the Android build by splitting the feature set per target with a small wrapper crate |
| Android 15 `dataSync` cap | certain if that type is used | Phase 6 | use `specialUse`, or accept periodic re-login |
| OEM blocks background broadcast delivery | high on some ROMs | matrix row "killed while offline" | document the autostart whitelist; never promise it. The keep-alive service does not help either if the ROM kills it |
| PendingIntent wake-up is one-shot per registration | certain (AOSP `mPendingIntentSent`) | measured in S6 | live callback while alive, arm only while offline, 15-minute backstop; the keep-alive toggle closes the gap |
| Seamless roam with a new IP is not delivered as a callback | medium | S7 | periodic WorkManager backstop (15 min); the keep-alive service would catch it immediately |
| Receiver budget (about 10 s) shorter than a slow login | medium | measure the AC-relay case | 8 s budget, then a one-time WorkManager job with a network constraint |
| Force stop or reboot without opening the app | certain | matrix rows above | documented boundary; `BOOT_COMPLETED` covers the reboot case, nothing covers force stop |
| 16 KB page alignment | certain | Phase 4 | NDK r28+ |
| OEM kills the foreground service | high with the toggle on | Phase 6 measurements | document it; never promise "survives swipe" |

## 10. Effort (rough, single developer)

| Phase | Work |
| --- | --- |
| 0 spike (including the preview setup) | 1-1.5 days |
| 1 lib/bin split | 0.5 day |
| 2 platform seams | 2-3 days |
| 3 Android entry | 2-3 days |
| 4 Gradle/packaging | 1-2 days |
| 5 v1 behaviour, wake-up path and matrix | 2-3 days |
| **v1 total** | **~9-14 days** |
| 6 keep-alive toggle (optional, default off) | 2-4 days |

## 11. Files touched, at a glance

| File | Change |
| --- | --- |
| `Cargo.toml` | add `[lib]`, move desktop deps under `not(android)`, add `android-native-activity`, add `jni` under `cfg(target_os = "android")` |
| `src/main.rs` | thin shim only |
| `src/desktop.rs` (new) | the old `main.rs` body |
| `src/lib.rs` (new) | module tree, Android entry |
| `src/android.rs` (new) | `android_main`, `prepare`, panic hook, and the `Native` JNI exports |
| `src/clipboard.rs` (new) | system clipboard: `ctx.copy_text` plus a JNI call to `ThunetActivity.copyText` |
| `src/android_jni.rs` (new) | VM/Activity registry and the typed JNI calls (`call_str`, `call_bool`) |
| `src/keepalive.rs` (new) | reads `files/keepalive.txt`; asks the Activity over JNI for a change |
| `android/app/src/main/java/**/Wake.java` | state file, one-shot arming, 15-minute backstop |
| `android/app/src/main/java/**/KeepAliveService.java` | foreground service and its notification |
| `android/app/src/main/java/**/NetworkWakeReceiver.java` | one-shot delivery, `goAsync` login |
| `android/app/src/main/java/**/BootReceiver.java` | service at boot for *Resident*, one-shot for *Wi-Fi change* |
| `android/app/src/main/java/**/NotificationActionReceiver.java` | the notification's disconnect action |
| `android/app/src/main/java/**/WakeWorker.java` | the 15-minute backstop |
| `android/app/src/main/java/**/Native.java` | the JNI method names `src/android.rs` exports |
| `android/app/src/main/java/**/ThunetApp.java` | process-wide work: bind to Wi-Fi, maintain `link.txt`, arm on loss |
| `android/app/src/main/java/**/Ssid.java` | optional SSID bridge (`secure.txt` / `ssid.txt`) |
| `src/secure_wifi.rs` (new) | reads the setting; asks the Activity over JNI |
| `android/app/src/main/java/**/ThunetActivity.java` | insets, link transport, night mode, clipboard, background level and permission flow |
| `android/app/src/main/res/xml/network_security_config.xml` | cleartext limited to the probes and the portal |
| `android/app/src/main/res/drawable/ic_stat_thunet.xml` | the notification's small icon |
| `scripts/android_build.sh` | `cargo ndk` for the Gradle build |
| `src/tray.rs` (new) | tray façade, Android no-op |
| `src/notify.rs` (new) | notification seam (empty in v1) |
| `src/ipc.rs` | Android endpoint; `host_core` idempotent so the service and the window can share one core |
| `src/ui.rs` | tray façade call sites, viewport gating, font candidates, background row, long-press copy |
| `src/config.rs` | Android state directory |
| `src/theme.rs` | Android branch reading `files/theme.txt` (**L**) |
| `src/srun.rs`, `src/core.rs` | link kind from `files/link.txt`; no SSID hint on Android |
| `src/{single,elevate,shortcut,autostart}.rs` | Android stubs |
| `android/**` (new) | Gradle project, manifest, icons |
| `android/.gitignore` | Android build output, `local.properties`, `jniLibs/` and signing material (scoped to the project) |

## 12. Semantics parity with the desktop

The baseline is the desktop's **portable mode**: the core is a thread of the app, there is no service row with Start/Stop, and no tray. Android adds one setting the desktop does not have (the background level), and replaces the tray with a notification that exists only at *Resident*.

### Same semantics, no change needed

| Operation | Desktop | Android |
| --- | --- | --- |
| Status ⇄ Settings | gear / back button | same |
| Privacy eye | header button, window-only state | same |
| Refresh | button | same |
| Connect / Disconnect | gated by the network probe verdict | same probes, same gating |
| Login / logout / ac_id / chksum / keep-online / auto-connect / reconnect delay | core | same core code |
| Session state, account, IPv4/IPv6, traffic, device list | from the core | same |
| Login failure banner, refresh failure rule, toasts | in-window | same |
| Language auto/zh/en | `sys-locale` | `sys-locale` has Android support |
| Log level | setting | same |
| Theme auto | follows the OS (portal on Linux, winit elsewhere) | `uiMode` through the Java helper, re-read every 2 s (**L**) |
| Account, password, remember, apply, unsaved/revert markers | setting page | same fields; long press pastes, and the pair is published to the autofill framework as a virtual view |
| Account switch warning and fallback | setting page | same |
| Credential sealing (`credential.bin` + `key.bin`) | machine dir | app-private dir, same format and magic |
| Privacy masking | setting | same |
| Heartbeat turtle and log pane | status page | same, mobile layout |

### Adapted: same intent, different trigger or wording

| Operation | Desktop | Android plan |
| --- | --- | --- |
| Close window | hides to tray, session continues | back sends the task to the background; the session and the window survive while the process lives, the *Resident* level makes it durable (**A**) |
| Tray menu | always available | at the *Resident* level the notification body opens the app and carries disconnect and stop actions; the other levels have no quick action (**B**) |
| Start with the system | three modes (off / core / core + tray) | the same triad as a *Background* setting: off / Wi-Fi change / resident, where resident also starts at boot (**C**) |
| Core/service row | service status + Start/Stop/Restart | "runs in this app", no buttons, as in portable mode (**D**) |
| Link row | Ethernet / WLAN / none / other | WLAN / none via ConnectivityManager, or hidden (**E**) |
| 802.1X shortcut | SSID says "no portal" | probes decide; on Tsinghua-Secure the outcome is "no login needed" but without the explanation (**F**) |
| Notifications | tray + in-window toasts | only the resident notification; failures stay in the log (**G**) |
| Start minimized, desktop shortcut, portable marker | present | hidden (**H**) |
| CLI verbs and `--host/--http/--ac-id/--ip/--timeout` | present | none; config file still carries the keys (**I**) |
| Log copy / disabled-button reasons | right-click, hover | long-press or a copy action, inline gray hints (**J**) |
| "Keep online" vs the new setting | one setting | two settings; wording has to keep them apart (**K**) |

### Cannot be identical

| Desktop concept | Why Android cannot match it |
| --- | --- |
| Machine-wide service and multi-user account | one Android app runs as one user; no service manager, no shared core |
| "Quit" of a background service while the window stays open | there is no second process to quit; the app and its core share one process |
| Tray icon as an always-present entry point | Android has no tray; the launcher icon is the entry point, and a notification exists only at *Resident* |
| Silent boot start | `BOOT_COMPLETED` can start the service, but the user must grant notification permission and OEM autostart |
| Package-manager updates | sideload means manual updates |

## 13. Semantics decisions (confirmed 2026-10-26)

All items below were confirmed as recommended. The word "Recommend" reads as "Decided"; they are the contract for Phases 1-6.

- **A. Back / close.** Back sends the task to the background (Home semantics) and never logs out. **Amended 2026-10-26** from the confirmed wording "the Activity finishes": `android-activity` can run `android_main` only once per process, so finishing the Activity left the next launch stuck on the splash screen while the core was still alive. The picture and the process are kept; a recents swipe or the system may still kill them. A second back from the launcher follows the OS.
- **B. Tray replacement.** At the *Resident* level the notification carries disconnect / open / stop; the other levels have no persistent quick action.
- **C. Autostart row.** Three levels that map one-to-one onto the desktop: *Off* (nothing), *Wi-Fi change* (the core alone, woken by a network change; no notification) and *Resident* (core + the always-present entry point: a foreground service, a silent notification and a start at boot). **Amended 2026-10-26** from the earlier "keep-alive switch + start-at-boot row": the level is the single setting, and *Stay online* keeps its desktop meaning unchanged.
- **D. Core row.** Recommend: show "runs in this app" with the version, no Start/Stop/Restart. Relaunching the app is the restart. Optionally a hidden debug restart later.
- **E. Link row.** Recommend: implement WLAN/none through the existing Java helper (one `ConnectivityManager` query), so the row keeps its meaning; phones have no Ethernet, so "other" covers tethering.
- **F. 802.1X.** The SSID shortcut exists again as an optional *Secure detection* switch: reading the Wi-Fi name needs the location permission, so it is asked for only when the user turns it on. With it off (the default) the probes decide, exactly as in the earlier probe-only decision.
- **G. Notifications.** Only the resident notification. Do not post login failures; at the *Resident* level repeated login failures can turn the notification text amber instead of raising a new one.
- **H. Hidden rows.** Recommend: hide Start minimized, desktop shortcut and the portable marker on Android without placeholders.
- **I. CLI and overrides.** Recommend: no Android CLI and no UI for `host/https/ac_id/ip`; the config file keeps the keys for anyone who edits it.
- **J. Log copy and hints.** Recommend: long-press a log line to copy it plus a "copy all" action; disabled buttons carry a gray inline hint where the desktop used hover text.
- **L. Theme auto.** Recommend: read `Configuration.uiMode & UI_MODE_NIGHT_MASK` in the existing Java helper and feed it to egui, so "follow the system" means the same thing it does on the desktop. Light and dark keep working without it.

## 14. Decisions

Decided:

- Sideloaded APK, not Play: the foreground-service type stays free of review, and users update manually.
- v1 has no resident background service by default and no persistent notification; Wi-Fi connect is handled by a live callback while alive, a one-shot system callback for the "killed while offline" case, and a 15-minute backstop.
- The background levels and the optional resident service **ship in the first release**; *Resident* is the only way to guarantee wake-up in every case.
- UI stays egui; the protocol and session layers stay in Rust.
- Android keeps the desktop's operation semantics; section 13 records the confirmed items (A-L) and is the contract for the UI work.
- Latest stable SDK, `minSdk` 33 (Android 13), `arm64-v8a` only for release, no legacy compatibility code.
- Package name `io.github.yjmthu.thunet`.
- No in-app update check; releases are announced manually.
- Development uses the debug signing key; a release key is generated and kept outside the repository before the first release.

Nothing is pending except the amended item A above, which needs a word from the user (implemented as background, not finish).
