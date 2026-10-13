# Parity: remote desktop sessions and view

RDP and VNC tabs end to end: the backend flow of `server/src/remote-desktop/desktop-socket.ts`
and `desktop-passwords.ts` (in `muxus-session`, module `desktop`) and the view of
`client/src/components/RemoteDesktopViewImpl.tsx` (in `muxus-app`, feature
`remote_desktop`). The protocol clients are `muxus-rdp` and `muxus-vnc` (see
`remote-desktop.md`).

```bash
CARGO_TARGET_DIR=target-ui-c cargo test -p muxus-session --lib desktop     # 15 tests
CARGO_TARGET_DIR=target-ui-c cargo test -p muxus-app --lib remote_desktop  # 14 tests
CARGO_TARGET_DIR=target-ui-c cargo build -p muxus-app --example remote_desktop
hack/shot.sh --env MUXUS_WINDOW=fullscreen out.png -- target-ui-c/debug/examples/remote_desktop
```

The example serves the documentation's invented desktop (`hack/demo-vnc.mjs`, ported) from an
in-process RFB server, or from IronRDP's own server with `MUXUS_DESKTOP=rdp`, and opens the view
where the reference screenshot shows the pane (250, 93; 1170×787). `MUXUS_THEME=dark`,
`MUXUS_DESKTOP_STATE=connected|connecting|ended|idle`, `MUXUS_DESKTOP_TOOLBAR=pinned|auto`.

## Architecture

In TypeScript a control socket (`/ws/desktop`) carried prompts and decisions, single-use
tickets authorized a second socket, and IronRDP (WebAssembly) or noVNC ran in the renderer
behind an RDCleanPath proxy or a WebSocket relay. Natively the backend runs the whole
connection: `Backend::open_desktop_session(profile, options) -> DesktopHandle` resolves the
saved host, dials the SSH gateway, gets the logon (vault or prompt), connects the RDP/VNC
client, retries a rejected logon, records the connection, remembers the password, and forwards
the client's picture/pointer/clipboard events until `DesktopEvent::Exit`. The live
`DesktopConnection` (RDP or VNC) is handed to the view for input and framebuffer reads.
`retry`, `connected`, `credentials-request`, `server-key(-verdict)` and the tickets have no
counterpart.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Saved-host resolution (current settings, kind check, "This saved host no longer exists.") | `desktop-socket.ts` `resolveProfile` | done | `DesktopEvent::Dialing` carries the dialled profile (`ready.profile`). |
| SSH gateway: connect with the session's prompts (host key, password), wait for post-auth, `direct-tcpip` tunnel (15 s), channel-open errors as socket sentences, gateway close ends the session ("The SSH gateway X disconnected…"), lease released with the session | `desktop-socket.ts` `dialGateway`, `openStream`; `util/dial.ts` | done | Tunnel passed as `RdpTransport::Tunnel`/`VncTransport::Tunnel`. A session that ends right after its gateway reports the gateway (250 ms grace). Never dials around a gateway it could not reach (tested). |
| RDP logon: saved password (with vault unlock), `Remote Desktop logon` prompt (user name asked when the host has none), remember offer, rejected-logon instructions, retry on `credentials` failures, `Logon cancelled.` | `loadRdpCredentials`, `askLogin`, view `startRdp` | done | Tested against IronRDP's server with NLA (wrong password → prompt again). |
| VNC credentials mid-handshake (saved password unless rejected, `VNC authentication` prompt, user name only when the server wants one) and retry after a rejected login | `provideVncCredentials`, view `startVnc` / `onDisconnect` | done | |
| `DesktopVault` over the store's vault (login-password service, scrypt off the async workers) | `desktop-passwords.ts` | done | `StoreDesktopVault`; prompts via `DesktopPasswords`. |
| After login: record recent connection once, remember the password (vault create/unlock prompts), keep a typed user name on the saved host, failures only as a note | `loginSucceeded` | done | Tested: mismatched vault confirmation leaves the desktop up with "The password could not be remembered: …"; a remembered password is used next time without a prompt. |
| RDP certificate pinning (challenge only when unverified and not pinned, pin on accept, keyed by host/port/gateway) | `acceptCertificate`, `certificates.ts` | done | Tested: pinned on first connect, not asked again. |
| VNC RSA-AES server key pinning (new / mismatch / per port) | `acceptServerKey` | done | Tested at the handler (no RA2 server in the session tests; the crate tests the handshake). |
| Session end sentences and reasons (completed / disconnected / failed; user close = "The connection closed.") | view `finish`, `run()` | done | |
| Failure cases of `desktop-socket.test.ts`: refused TCP, X.224 refusal, reset while the certificate prompt is open, unknown saved host | `desktop-socket.test.ts` | done | Plus tickets/RDCleanPath cases: n/a. |
| Picture: framebuffer drawn as tiled GPU images (256 px tiles with a 1 px border), only tiles a dirty rectangle touches are copied and uploaded, replaced tiles leave the atlas a frame later | canvas in `RemoteDesktopViewImpl.tsx`, ironrdp-web `canvas.rs`, noVNC `display.js` | done | BGRA framebuffer straight into `RenderImage`, no swizzle. Seamless when scaled (border texels feed the bilinear filter). |
| Scaling: RDP `fitRect` (whole-pixel box, centred); VNC noVNC `autoscale` (exact ratio, fractional centring) | `fitRect`, noVNC `Display.autoscale` | done | Pointer mapping per client: RDP rounds, noVNC truncates. |
| Remote resize after the pane settles (300 ms), first RDP size from the measured pane (falls back to 1280×800 when not drawn within 250 ms) | `resize` effect, `startRdp` box | done | |
| Keyboard: GPUI keystrokes → DOM `code`/`key`; modifier keydown/keyup rebuilt from modifier-state changes; event flags from the window's live state (keeps Shift GPUI drops from `!`); Caps Lock toggles reported as key presses | browser `KeyboardEvent` | partial | Verified in the headless shell against IronRDP's server: Shift+1 sends Shift down, `2`, Shift up; Caps Lock toggles then syncs. GPUI gives no physical key code, so `code` is derived from the produced key in US-layout terms (exact on a US layout; other layouts map by character); left/right modifiers are not distinguished; Num Lock / Scroll Lock state is unknown (reported off); AltGr is not reported; keypad digits arrive as digits. |
| Key grabbing: app shortcuts first (capture), then the desktop; keys the client cannot send propagate; Ctrl+Shift+I / Cmd+Alt+I unbound in the `RemoteDesktop` context | `shortcuts.ts`, canvas `onKeyDown` | done | `remote_desktop::init`. |
| Pointer: move, buttons incl. back/forward, drag capture beyond the desktop, release on blur; wheel (GPUI lines → 40 px each, 120 px a notch) | canvas mouse handlers, `wheel` | done | Chrome's pixels-per-notch on Linux is assumed to be 120 (one RDP `WHEEL_DELTA`). |
| Cursor shapes | ironrdp-web cursor callback, noVNC cursor | partial | Shapes reach the view (`RemoteDesktopView::cursor`), but GPUI 0.3.9 cannot set an image cursor or hide the pointer: the system arrow stays over the desktop. |
| Focus: focus on connect when active and on click; release all keys on blur, tab hidden or window deactivated | `focusDesktop`, `onBlur`, `active` effect | done | |
| Clipboard: local text offered on focus, window activation and connect; remote text written to the clipboard; off with `shareClipboard: false` | `pushClipboard`, `onRemoteClipboard` | done | |
| Toolbar: `KIND · host:port`, Ctrl+Alt+Del (not for view-only VNC), Windows key (RDP), Reconnect, Disconnect; shown 2.5 s after connecting, hover strip at the top brings it back, leaving hides it; 160 ms slide and fade | toolbar `Paper` | done | Caption is `text.primary` as the Electron build renders it (`color="textSecondary"` is not a colour MUI 9 knows). |
| Connecting / ended / idle screens: spinner, status line, Cancel; icon, message (wrapped, 520 px), redial countdown caption, Connect / Reconnect / Reconnect now | overlay `Stack` | done | muxus-ui `CircularProgress`, `Button`, `IconButton`; colours from the theme tokens `remote_desktop_bg` / `remote_desktop_ink`. No reference screenshot; not yet compared with Electron. |
| Status lines: overlay while connecting, toasts for lasting notes once up | `status` handler | done | `RemoteDesktopViewEvent::Toast`. |
| Auto-reconnect (preference, attempts, stable reset, no redial after a declined certificate or a user disconnect) | `autoReconnectDelayMs` use | done | `muxus_session::recovery`. |
| Prompts: auth, host key, certificate | `AuthPromptDialog`, `HostKeyDialog`, `DesktopCertificateDialog` | partial | The view emits `RemoteDesktopViewEvent::Prompt` and takes the answers; the shared dialogs are the app's (not drawn yet). |
| Live check against a container | — | missing | Not run in this wave; the clients were checked live against xrdp and TigerVNC in `remote-desktop.md`. |

## Screenshots

Reference: `docs/assets/screenshots/remote-desktop{,-dark}.png` (1420×880 at 2×). The example
draws only the pane (the sidebar and tab strip are the app's), so the comparison crops the
pane (`--crop 250,93,1170,787`) and the toolbar (`--crop 650,95,400,45`). Share of device pixels
differing by more than 24/255:

| Shot | Pane | Toolbar | Remaining differences |
|---|---|---|---|
| light | 0.40% | 3.76% | picture edges (GPU vs Chrome bilinear filtering of the 1.0013× upscale), toolbar text rasterization |
| dark | 0.38% | 2.67% | same |

The remote picture is the same invented desktop (ported drawing), so the pane comparison
covers the picture placement and scaling too. Captured states without a reference:
RDP connected (IronRDP server), RDP connecting (logon unanswered), VNC ended (refused), VNC after
clicking Disconnect.

## Added to muxus-ui

- Theme tokens `Components::remote_desktop_bg` (`#101014`, both modes) and
  `Components::remote_desktop_ink` (MUI grey 300) in `crates/muxus-ui/src/theme.rs`.

## Hook points for the app

- `muxus_app::features::remote_desktop::init(cx)` once at startup.
- `open_remote_desktop_view(backend, profile, title, connect, window, cx) -> Entity<RemoteDesktopView>`;
  render the entity as the pane content.
- `RemoteDesktopView::{set_active, set_title, set_auto_reconnect, reconnect, disconnect,
  answer_auth, answer_host_key, answer_certificate, prompts, phase, current_profile}`.
- Events: `RemoteDesktopViewEvent::{Status, Prompt, PromptsClosed, Toast}`.
- Backend: `Backend::open_desktop_session` with `muxus_session::desktop::{DesktopHandle,
  DesktopEvent, DesktopConnection, DesktopOptions}`.

## Attribution

No third-party code was adapted. The example's desktop drawing and RFB server port Muxus's own
`hack/demo-vnc.mjs`.

## Integration pass

Remote desktop tabs run in the window (`ui/session_views.rs`): status, reconnects, titles,
focus, toasts, and the shared auth, host-key and certificate dialogs (`ui/prompts.rs`, the
certificate dialog ported from `DesktopCertificateDialog.tsx`).
