# Parity: desktop shell (`features/desktop`, `muxus_session::{updater, links}`, `packaging/`)

What `electron/src/main.ts` and its helpers did around the windows: one running instance and
the launches later processes hand over, ssh:// and telnet:// link registration, the in-app
updater (and its Microsoft Store variant), the shell's log file, and the Linux packages.
Windows, tabs, detaching and window placement belong to the app integration.

```bash
CARGO_TARGET_DIR=target-ui-a cargo test -p muxus-session --lib -- updater links    # 68 tests
CARGO_TARGET_DIR=target-ui-a cargo test -p muxus-app --lib features::desktop        # 13 tests
CARGO_TARGET_DIR=target-release cargo build --release -p muxus-app
MUXUS_BINARY=target-release/release/muxus packaging/build-deb.sh
MUXUS_BINARY=target-release/release/muxus packaging/build-appimage.sh
MUXUS_BINARY=target-release/release/muxus packaging/release-feed.sh
```

Nothing in the tests reaches the network, changes a real default application or installs
anything: the updater runs against a fake HTTP layer and a loopback server with a recording
installer, link registration against a fake xdg-mime in scratch folders, the single-instance
tests in scratch runtime folders.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Single instance per data folder, second launch hands over and exits | `main.ts` (`requestSingleInstanceLock`, `second-instance`) | done (Linux), partial (Windows) | `single_instance::acquire`: exclusive `File::try_lock` on `<runtime>/muxus-rs-<hash>.lock`, a 0600 Unix socket next to it, one JSON line per launch (`argv` + the launch the sender parsed), validated again by the receiver. Keyed by the data folder, so `MUXUS_DATA_DIR` sandboxes and development builds never hand over to the installed app. Stale sockets are replaced. Windows uses a named pipe (`\\.\pipe\muxus-rs-<hash>-<user>`, remote clients rejected); that code has not been compiled here (no Windows target). |
| Command line and links of later launches | `command-line.ts`, `main.ts` (`deliverCommandLineLaunch`, `open-url`) | done | `events::DesktopEvent::{Launch, Focus}`; `from_open_url` for macOS `open-url`; `initial_launch(argv)`. Parsing itself is `muxus_import::launch` (golden-tested there). |
| Launch planning against the catalogs | `CommandLineLaunchHandler.tsx` | done | `launch::plan_launch(backend, request)` → owned `PlannedLaunch` (action + toast). Performing it (opening tabs, focusing a workspace window, the request queue) is the app integration's. |
| Relaunch after an update waits for the old process | electron-updater relaunch | done | `RELAUNCH_AFTER_PID_VAR`; `acquire` waits up to 60 s for that pid before taking the lock. |
| Link handlers, Linux (xdg-mime, generated hidden desktop file, alias link for paths xdg-utils cannot quote, installed `muxus.desktop`) | `link-handlers.ts` | done | `muxus_session::links::LinuxLinkHandlers`; every `link-handlers.test.ts` Linux case ported. The Desktop Entry helpers it needs (`parseKeyFileGroup`, `splitExec`, `unescapeDesktopString`, `dataDirectories`) are small copies, because the SFTP feature's `linux_apps.rs` lives in the app layer. |
| Link handlers, macOS and Windows | `link-handlers.ts` (`SystemLinkHandlers`) | partial | Decision logic ported with all its tests (`ProtocolClientApi` fake). The real APIs are command based and untested here: Windows through `reg.exe` (the keys `setAsDefaultProtocolClient` writes; resolution through `UserChoice` → `shell\open\command`), macOS through `osascript` (JavaScript for Automation calling `LSSetDefaultHandlerForURLScheme` and `NSWorkspace`). Electron's source-checkout registration (Electron plus app path) has no Rust counterpart. |
| Backend for Settings → Behavior → Links | `main.ts` (`muxus:link-handlers:get/register`) | done | `Backend::link_handlers()` (`None` where unsupported) and `Backend::register_link_handler(scheme)`, logging like `mainLog`. Tested end to end in a sandbox with a scripted xdg-mime. |
| Links group in Settings → Behavior | `LinkHandlerSettings.tsx` | done | `links_view::LinkHandlerSettings`, embedded by one line in `settings/view/behavior.rs`; status texts, disabled rules and toasts are unit tested; refreshes when the settings window is focused. Built from the settings layout (`group`, `row`, `status_text`) and the muxus-ui `Button`; no raw styling. No screenshot reference: the browser capture the settings use has no desktop bridge, so the TypeScript renders no Links group there. |
| Muxus link scheme | — | n/a | Muxus registers only ssh:// and telnet://; there is no own scheme. |
| Updater state machine (single check/download in flight, download waits for a running check, explicit install, recovery after a failed installer, progress in whole percents) | `updater.ts` (`DesktopUpdater`) | done | `muxus_session::updater::DesktopUpdater`; all `updater.test.ts` cases ported except one assertion that cannot occur natively (progress after the download finished). |
| Background schedule (15 s after start, every 4 h, only while "Notify me" is on, never once an update waits) | `updater.ts` (`UpdateSchedule`) | done | tokio timers; tested with the paused clock. The app follows `notifyOnNewVersion` through `SettingsPrefs`. |
| Disabled reasons (development, Store, Linux package installs) | `updater.ts` (`updateDisabledReason`) | done | A `.deb` install stays `disabled: package-manager`, so About keeps the manifest check that links to the release (the "deb hint"), as in Electron. |
| Release feed (electron-builder `latest*.yml`, GitHub `/releases/latest` then the release's feed, generic mirror) | electron-updater `GitHubProvider`, `GenericProvider`, `findFile` | done | `updater::feed` (YAML subset + JSON) and `updater::transport::FeedTransport`. The feed constant is `RELEASE_OWNER`/`RELEASE_REPO` (`FloSch62/muxus`); `MUXUS_UPDATE_FEED=<folder URL>` points the app at a mirror. Plain http only to loopback. Staging percentages and differential (blockmap) downloads are not implemented. |
| Download with verification | electron-updater `DownloadedUpdateHelper` | done | Into `<cache>/muxus-rs-updater/pending/<name>.part`, size and base64 SHA-512 checked, then renamed; a verified earlier download is reused; older files are cleared. |
| Install handoff, Linux AppImage | electron-updater `AppImageUpdater.doInstall` | done | Same destination rules (keep a renamed file's name, follow versioned names); staged next to the target and renamed over it instead of unlinking first; started detached with `APPIMAGE_SILENT_INSTALL` and the relaunch pid. Tested with a recording launcher. Feeds without an AppImage are refused rather than installing another kind of file. |
| Install handoff, Windows (NSIS `--updated --force-run`) and macOS (zip → swap the `.app`, `open -n`) | `NsisUpdater`, `MacUpdater` | partial | Implemented, never run: there are no Windows or macOS packages of the Rust build yet, and no code-signature checks (electron-updater verified the Windows publisher; Squirrel.Mac the signature). |
| Microsoft Store updater (`check`/`install` through the Store helper, progress, cancel, errors) | `store-updater.ts`, `store-bridge.ts` | done (logic), n/a (Linux) | `updater::store::StoreUpdater` with every `store-updater.test.ts` case; `HelperStoreBridge` tested against a scripted helper. Only Windows Store packages build one (`MUXUS_UPDATE_MODE=store` at build time); elsewhere it never exists. |
| Store routing of the plain update check | `store-updates.ts` | done | `check_store_update`, `store_page_url` with all `store-updates.test.ts` cases. Not wired into `Backend::update_check` (session routes are not this area's); only relevant to Store packages. |
| App wiring: update entity, settings hooks, notice, restart confirmation, quit then install | `main.ts` (`createDesktopUpdater`, `before-quit`), `DesktopUpdateControls.tsx` | done (off by default) | `ui::run` calls `init_desktop_updates` only when `MUXUS_UPDATE_FEED` is set: the feed constant still names the Electron releases. Without it About shows "No updates are published for this build." (the backend's manifest check is off too, see `session.md`). Single instance, main log, crash capture and launch planning are wired in `ui::run` / `ui/desktop.rs` (`app.md`). |
| Main log (`<data>/logs/main.log`, 1 MiB rotation to `main.old.log`, mirrored into the log viewer, panic capture) | `main-log.ts` | done | `main_log::{init_main_log, main_log, install_crash_capture}`. Shares `logs/main.log` with the Electron build when both use the installed data folder (timestamped lines). |
| Linux desktop entry, icons, MIME registration | electron-builder `linux` config, `after-install.tpl` | done | `packaging/linux/muxus.desktop` (`MimeType=x-scheme-handler/ssh;x-scheme-handler/telnet;`, `StartupWMClass=muxus`), hicolor icons 16–1024 from the Electron build, postinst/postrm (update-alternatives `/usr/bin/muxus`, desktop database, icon cache). Windows must use app id `muxus` (`desktop::APP_ID`). |
| Login shell environment | `login-shell-environment.ts` | n/a here | `muxus-local::login_env`. |
| Linux "Open with" applications | `linux-applications.ts` | n/a here | SFTP feature (`features/sftp/linux_apps.rs`). |
| `.deb` package | electron-builder `deb` | done | `packaging/build-deb.sh`: `dpkg-deb --root-owner-group`, Electron's layout (`/opt/Muxus/muxus`), Depends from `dpkg-shlibdeps` plus the libraries loaded at run time (Wayland, Vulkan or EGL), Recommends xdg-utils. |
| AppImage | electron-builder `AppImage` | done | `packaging/build-appimage.sh`: appimagetool when installed (it is not on this machine), else `mksquashfs` (zstd) appended to the AppImage type 2 runtime (downloaded once, magic checked, sha256 printed). |
| Release feed for the packages | electron-builder publish, `hack/update-feeds.mjs` | done | `packaging/release-feed.sh` writes `latest-linux.yml` (AppImage first). The Windows feed merge (`merge_windows_feeds`, `to_yaml`) is ported with `update-feeds.test.mjs` in `updater::feed`. |
| Third-party licenses in the packages | electron-builder `LICENSES.chromium.html` | done | `packaging/third_party_licenses.py`: every crate linked into the binary (cargo metadata, normal dependencies, this target), its declared license and shipped license files, identical texts once (839 crates, 1.3 MB). |
| Distribution build config, signing, Store manifest | `scripts/build-config.ts`, `dist-args.ts`, appx | missing | No Windows/macOS packaging for the Rust build yet (`distribution-config.test.ts`, `store-manifest.test.ts` not ported). |

## Packages built from the current release binary

`target-release/release/muxus` (103,345,896 bytes, built 2026-10-12 19:56), version 0.11.1:

| Artifact | Size | Checked with |
|---|---|---|
| `target-release/release/packages/muxus-0.11.1-linux-amd64.deb` | 20,168,092 bytes (19.2 MiB) | `dpkg-deb --info` (control above), `dpkg-deb --contents` (binary, desktop file, 9 icon sizes, licenses, copyright; root-owned, 0755 directories) |
| `target-release/release/packages/muxus-0.11.1-linux-x86_64.AppImage` | 27,949,560 bytes (26.7 MiB) | `unsquashfs -o <runtime size>`: `AppRun`, `muxus.desktop`, `muxus.png`, `.DirIcon`, `usr/bin/muxus` byte-identical to the release binary, icons, licenses |
| `target-release/release/packages/latest-linux.yml` | 537 bytes | read by `updater::feed` |

Not installed on this machine. The binary needs glibc 2.39 (`libc6 (>= 2.39)`), because it
is built on this system; an AppImage for older distributions needs an older build host.

## Screenshots

Settings → Behavior scrolled to the Links group, from `examples/settings.rs` in the capture
harness (1000×720 at 2×, `MUXUS_SETTINGS_SECTION=behavior MUXUS_SETTINGS_SCROLL=2000`), light
and dark. The sandboxed backend asks the real xdg-mime with the sandbox's HOME, which reads
the system defaults (here KTelnetService for both schemes). There is no reference: the docs
screenshots have no Behavior page, and the browser build shows no Links group, so no pixel
difference is reported.

## Tests

- `muxus-session` (68): `updater::tests` (`updater.test.ts` 9, `store-updater.test.ts` 6, the
  feed transport against a fake web and a loopback server 6, cache folder and process wiring 2),
  `updater::feed` (9, including `update-feeds.test.mjs`), `updater::http` (2), `updater::install` (5),
  `updater::store` (7, including `store-updates.test.ts`), `links::tests` (22,
  `link-handlers.test.ts` plus the Desktop Entry helpers, the registry parser and a backend
  round trip with a scripted xdg-mime).
- `muxus-app` (13): `features::desktop` events (3), launch planning against a sandboxed
  backend (1), Links view logic (2), main log (2), single instance (5: hand-over over a real
  socket, focus, other data folders, stale socket, garbage, waiting for a replaced process).
- `features::settings` still passes its 117 tests with the Links group embedded.

## Risks

- The feed constant still names the Electron releases (`FloSch62/muxus`), so the app keeps the
  updater off unless `MUXUS_UPDATE_FEED` names a feed. Point `RELEASE_OWNER`/`RELEASE_REPO` at
  the Rust build's releases before turning it on by default.

## Hook points for the app integration

| API | What it does |
|---|---|
| `desktop::single_instance::acquire(data_dir, &argv)` | First thing in `main`. `Secondary { .. }` → exit. Keep `PrimaryInstance`. |
| `PrimaryInstance::events()` → `DesktopEvents::recv().await` | `DesktopEvent::Launch(request)` / `DesktopEvent::Focus` from later launches. Subscribe once. |
| `PrimaryInstance::sender()` + `DesktopEvent::from_open_url(url)` | macOS `open-url` (GPUI `on_open_urls`). |
| `desktop::events::initial_launch(&argv)` | The launch this process started with. |
| `desktop::launch::plan_launch(&backend, &request).await` | `PlannedAction` (connect target, open profile, connect host, launch folder, open workspace, fail) and the toast. |
| `desktop::updates::init_desktop_updates(&backend, DesktopUpdateInit { before_store_install }, cx)` | After `SettingsPrefs::init`. Installs `SettingsHooks::desktop_update` (merges into existing hooks); `DesktopUpdates::global(cx)` is the observable state. Needs `sftp::local_open::install_dialog_host` for the restart confirmation and the launcher `ToastHook` for toasts. |
| `desktop::main_log::{init_main_log, install_crash_capture, main_log}` | Start of `main`; `main_log` for shell milestones ("starting", "the backend failed to start" with its path in the error box). |
| `desktop::links_view::links_group` | Already embedded in Settings → Behavior. |
| `Backend::link_handlers()`, `Backend::register_link_handler(scheme)` | The Links backend. |
| `muxus_session::updater::AppUpdater` | The updater itself, for anything beyond the hooks. |
| `desktop::APP_ID` | `muxus`: the Wayland app id / X11 class windows must use. |

## Known differences

- Single instance: Electron's lock is per `userData`; this one is per data folder too, but
  the Rust and Electron builds never hand launches to each other.
- The updater refuses a feed without an installer of this platform's kind instead of
  downloading whatever electron-updater's `findFile` falls back to.
- The AppImage is replaced atomically (staged, then renamed) instead of unlink-then-move.
- A relaunched update waits for the old process to exit before taking the instance lock.

## Attribution

No third-party code was adapted. Everything here is ported from Muxus itself (MIT). The
AppImage runtime appended by `build-appimage.sh` (AppImage/type2-runtime, MIT) is downloaded at
build time and not stored in the repository. Dependencies added: `futures-util`, `sha2`
(muxus-session, MIT/Apache-2.0); tokio `net` on Windows (muxus-app).
