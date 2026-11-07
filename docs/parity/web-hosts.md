# Parity: web page hosts

Saved web pages (v0.15, upstream `090a88f` and `2c2903c`): a **Web** host keeps a page's address
in the host list and opens it in a Muxus browser window of its own, optionally through a SOCKS5
proxy on an SSH gateway. Ports `electron/src/web-windows.ts`, `client/src/web-hosts.ts`,
`components/WebHostEditorContent.tsx`, `components/host-editor/web-draft.ts`, the window's part of
`dial-prompts.ts`/`WindowDialPrompts.tsx`, and the server's port-0 and independent forwards. The
data layer (schema v36 `web` kind, `WebProfile`) is tracked in `core-store.md`.

```bash
cargo test -p muxus-webview                                         # 9 tests (13 with --features webview)
cargo test -p muxus-app --lib -- web_draft web_hosts                # 9 tests, one over a test sshd
cargo test -p muxus-ssh --lib forward_manager                       # port 0 and independent forwards
# The page window alone, the way the app opens it (needs libwebkit2gtk-4.1-dev on Linux):
cargo run -p muxus-webview --features webview --example web_window -- https://example.com [socks-port] [seconds]
cargo run -p muxus-app --features webview                           # the app with page windows
```

## Design

The app's windows are GPUI; a web page needs a browser engine. A page window is therefore a
**process of its own**: the `muxus` binary started with the hidden `--web-window` switch
(`muxus_webview::run_if_requested`, the first thing `main` does) runs a tao event loop with one
wry webview (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows). On Linux that keeps
GTK's main loop, which wants the main thread, away from GPUI's, and a crashing page away from the
app. The app talks to it over stdin/stdout, one JSON message per line
(`muxus_webview::protocol`): a launch line (the request and the host's data folder), then
`focus`/`close` commands; the window answers `opened` or `error`. The window closes when its stdin
ends, so it never outlives the app.

`muxus_webview::WebWindows` is the app's side (`openWebWindow`, `focusWebWindow`,
`closeAllWebWindows`, `registerWebWindows`): one window per saved host ID, focused (not dialed
again) when opened again; it reports each closed window with the forward behind its proxy, and the
app stops that forward (`ui::web_hosts::install`). The window code is behind the `webview` cargo
feature (`muxus-app/webview` → `muxus-webview/webview`), so the workspace builds without
WebKitGTK; such a build opens pages the way the browser build of Muxus does.

`muxus-webview` sits in layer L5 (it depends on `muxus-core` only; `muxus-app` uses it).

## Features

| Feature | TypeScript | State | Notes |
|---|---|---|---|
| Web host editor: name, address (http:// added when no scheme, on leaving the field), folder, color | `WebHostEditorContent.tsx`, `web-draft.ts` | done | `features/host_editor/{logic/web_draft.rs,view/web.rs}`; "Web" type tab in Add host; save then folder/color metadata (`services::save_web_host`); `web-host-draft.test.ts` ported |
| Connection route: one SSH gateway (SOCKS5), with the TS explanation | `WebHostEditorContent.tsx` (`SshGatewayField`) | done | The gateway picker of the desktop/management editors (system-OpenSSH hosts listed but not pickable). The TS web profile has no gateway fallbacks (`webProfileSchema` has `sshGateway` only), so neither has this one |
| Validation of the draft (`webDraftProblem`) | `web-draft.ts` | done | `new URL()` is the `url` crate (WHATWG) |
| Open from the sidebar, quick launcher, host picker, Save & connect, command-line host launch, a new main window | `session-actions.ts` (`connectSavedHost`, `openSavedHostInNewWindow`) | done | `AppShell::connect_host` routes a page to `open_web_host`; "Open in new window" opens the page window instead of an app window |
| Launch a folder with pages | `session-actions.ts` (`launchManagedHostGroup`) | done | Pages open next to the tabs; a group of pages alone leaves the tabs and multi-exec groups alone |
| A double click opens once | `web-hosts.ts` (`opening`) | done | App-wide (`WebPages::opening`), not per window |
| An open page's window comes to the front, nothing dialed | `web-hosts.ts`, `focusWebWindow` | done | Checked before dialing; a second app window racing it gets `focused` and its proxy is stopped |
| Gateway: dial with the window's sign-in and host-key prompts, SOCKS5 forward on a free port, independent of terminal tabs | `web-hosts.ts` (`startProxy`), `forward-manager.ts`, `routes/forwards.ts` | done | `features::web_hosts::start_proxy` on `forwarding::start::dial_connection` (shared with tunnels); forwards on port 0 report the port they got (`muxus-ssh` `Binding::port`), `independent: true` survives `stop_session_for_connection`; tests port `forward-manager.test.ts` |
| A failed dial takes back its queued prompts | `dial-prompts.ts`, `WindowDialPrompts.tsx` | done | `close_prompts_of("web:<id>")` once the open finishes |
| The window owns its proxy: stopped when the window closes, or at once when it was already open or refused the request | `web-windows.ts`, `main.ts` | done | `ClosedWebWindow.forward_id`; a window that cannot start (no display, no WebKitGTK) reports its error as a toast and its proxy stops |
| `parseWebWindowRequest` (key, http(s) URL ≤ 4096, title ≤ 500, `socks5://127.0.0.1:<port>`, forward ID) | `web-windows.ts` | done | Checked by the app and again by the window process; `web-windows.test.ts` ported |
| One cookie jar and storage per host, kept between visits | `web-windows.ts` (`persist:muxus-web-<key>`) | done | wry `WebContext` on `<user data>/web-pages/<host id>` (created 0700); WebKit's data is not Chromium's, so pages opened by the Electron build sign in again once |
| Every request through the proxy, name lookups included; `localhost` is the gateway | `web-windows.ts` (`proxyRules`, `<-loopback>`) | done | wry `ProxyConfig::Socks5` (WebKitGTK network proxy settings, no ignore list); GLib's SOCKS5 sends host names (checked: CONNECT by name through a logging proxy) |
| Window: 1200×800, title `<host> — <page title>`, Muxus icon | `web-windows.ts` | done | Icon by name (`muxus`, the packages' hicolor icon) on Linux |
| Alt+Left/Right, Ctrl+[ / Ctrl+], F5 / Ctrl+R | `web-windows.ts` (`before-input-event`) | done (Linux) | WebKitGTK `key-press-event`; macOS/Windows use an injected key handler (Cmd on macOS), unverified |
| Right-click menu (Back, Forward, Reload, Cut, Copy, Paste) | `web-windows.ts` (`context-menu`) | done (Linux) | WebKitGTK `context-menu` signal: WebKit's menu (link, image and input method items) is replaced by the six items, Back/Forward while there is history that way, Cut/Copy/Paste as Chromium's `editFlags` put it (cut a selection in a field, copy a selection, paste into a field) from the hit test; Cut/Copy/Paste run WebKit's editing commands. Pages a page opened get the same menu (Electron shows none there). macOS/Windows: the engine's own menu (wry has no hook) |
| Pages stay on http(s); pages they open (target=_blank) get a window in the host's session and proxy, with the same rules, and close with it | `web-windows.ts` (`guardContents`) | done (Linux) | Popups are GTK windows with a related wry view; macOS/Windows let the engine open them in the opener's environment (same cookies and proxy), without the navigation guard |
| No camera, microphone, location or notifications | `web-windows.ts` (`setPermissionRequestHandler`) | done | wry permission handler denies everything |
| Untrusted certificate: ask once per certificate, "Go back" / "Continue anyway", the answer holds until the window closes | `web-windows.ts` (`handleWebCertificateError`) | done (Linux) | GTK dialog on `load-failed-with-tls-errors`; a yes is `allow_tls_certificate_for_host` on the window's own WebContext (in memory, gone with the window process, shared by its popups); a no goes back, or shows a short refusal page. Fingerprint shown as SHA-256 hex. macOS/Windows: wry offers no hook, so such pages fail to load (gap) |
| Web windows close with the last app window | `main.ts` (`closeAllWebWindows`) | done | On app quit; a window also closes when the app's end of its stdin closes |
| Browser build: no gateway opens the system browser; with one, start the proxy (reused while it runs) and say where it is | `web-hosts.ts` (`openInBrowser`) | done | What a build without the `webview` feature does |
| Page refused by the window ("invalid") reports an error | `web-hosts.ts` | done | |
| Sidebar details "via SOCKS5 on …", address, Copy address, kind icon, search, duplicate, export/import | `host-details.ts`, `saved-hosts.ts`, `managed-hosts.ts`, `data-transfer.ts` | done | Data-layer commit; duplicate goes through the web editor ("<name> copy") |
| Links never name a page | `command-line-launch.ts` | done | Data-layer commit (`muxus-import` launch resolve) |
| Packages | — | done (Linux) | `packages.yml` deb job installs `libwebkit2gtk-4.1-dev` and builds with `--features webview`; `build-deb.sh` adds `libwebkit2gtk-4.1-0` to Depends when the binary links it; the AppImage does not bundle WebKitGTK (fixed helper-process path) and `AppRun` names the missing package; the license list follows the feature. macOS and Windows packages are still built without the feature |

## Platforms

- **Linux** (WebKitGTK 4.1 ≥ 2.38): built, unit-tested and smoke-tested on WSLg (X11 and Wayland)
  against a local page through a logging SOCKS5 proxy: the page loads by a name only the proxy
  resolves, scripts run, the title follows the page, F5/Ctrl+R reload, a target=_blank link opens a
  popup through the same proxy, a self-signed HTTPS page asks and loads once accepted (and shows the
  refusal page when not), reopening only focuses, and closing reports the forward to stop; also
  `muxus --web-window` from the app binary itself. The right-click menu was checked on page text,
  on a field with selected text (Cut, Copy and Paste on) and after following a link (Back on;
  choosing it goes back).
- **Windows**: `cargo clippy -p muxus-webview --features webview --target x86_64-pc-windows-gnu`
  passes; never run.
- **macOS**: not compiled here (a build script needs Apple's toolchain); never run. `mac-proxy`
  makes the SOCKS5 proxy need macOS 14.

## Not ported / differences

- Untrusted certificates on macOS and Windows (no wry hook): the page does not load.
- The right-click menu is the six items on Linux only; macOS and Windows show the engine's own.
- WebKitGTK's own data layout per host; nothing migrates from Electron partitions.
- A build without the `webview` feature (the default for `cargo build`, and the macOS/Windows
  packages for now) behaves like the browser build of Muxus.
