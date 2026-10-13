# Parity: remote desktop

RDP and VNC sessions, without a browser in the way.

Crates: `muxus-rdp` and `muxus-vnc` (L2, both depend on `muxus-core` only).

The TypeScript build draws remote desktops in the renderer: IronRDP compiled to WebAssembly behind
a Node RDCleanPath proxy for RDP, noVNC behind a WebSocket relay for VNC. Natively both crates
talk to the server directly (or through a stream the caller opened through an SSH gateway), so
the RDCleanPath/X.224 proxy, the tickets and the `/ws/desktop*` sockets have no counterpart. What
they did that the user could see (error sentences, certificate pinning, timeouts) moved into the
crates.

Tests: `cargo test -p muxus-rdp -p muxus-vnc` (104 tests: muxus-rdp 40 unit, 7 handshake against
a fake RDP front end, 5 end to end against IronRDP's own server in process; muxus-vnc 41 unit, 11
against an in-process RFB server). Live checks (scratch programs, not in the repo): Alpine xrdp
0.9.24 container (autologon, certificate prompt, typing with Shift held, clipboard both ways,
graceful disconnect) and a TigerVNC (jlesage baseimage-gui) container (VncAuth, wrong password,
Tight/ZRLE picture, remote resize 1920×1080 → 1280×720).

## IronRDP revision

`muxus-rdp` uses IronRDP from git at `d90284f5f28dc24bb39c78883521721de76bf52f` (head of IronRDP
PR #2080: sspi 0.23, picky rc.26). The TypeScript build compiles `9b151c4c` to WebAssembly; that
commit pins `picky =7.0.0-rc.25`, whose `curve25519-dalek =5.0.0-rc.1` cannot resolve next to
russh. `d90284f5` is 50 commits ahead of `9b151c4c` and none behind, so it keeps the decoder
fixes the TypeScript build pins for (bitmap stride, RDP6 orientation, bulk decompression), which
the crates.io release 0.17.0 lacks. Do not move back to a revision that pins picky rc.25.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Physical key → set-1 scancode table, toolbar scancodes | `client/src/remote-desktop/scancodes.ts` | done | `muxus_rdp::scancodes`. |
| RDP keyboard: Shift kept held, lock sync only when it changed (sync releases all keys on Windows), held modifiers pressed again after a sync, no repeat for modifiers/locks, AltGr reported as AltGraph, modifiers held before focus | `rdp-client.ts` (`key`, `reconcileModifiers`, `syncLockKeys`) | done | `RdpKeyboard`; every case of `tests/unit/client/rdp-input.test.ts` ported, plus the same sequence checked as the server receives it (IronRDP server in process). |
| RDP wheel (DOM signs, both axes, pixel/line/page units) | `rdp-client.ts` (`wheel`), ironrdp-web `wheel_rotations` | done | Lines ×50, pages ×38×50 pixels like the WebAssembly build. |
| RDP pointer move/buttons, release all on blur, Ctrl+Alt+Del, Windows key | `rdp-client.ts` | done | Pointer in desktop pixels; `desktop_point` maps view pixels (clamped) like `desktopPoint`. |
| `rdpDesktopSize`, `rdpLogon` (DOMAIN\user split) | `rdp-client.ts` | done | Tests ported. |
| RDP connect: TCP (15 s), X.224 negotiation (20 s), TLS (20 s), CredSSP/NLA, autologon for TLS-only servers, display control and CLIPRDR channels | `rdp-client.ts`, `rdp-proxy.ts`, `x224.ts`, ironrdp-web `session.rs` + `autologon.patch` | done | rustls (ring). No RSA-key-transport retry: that worked around BoringSSL's key-usage check, which rustls does not make. TLS 1.0/1.1 are unsupported, as with Node's defaults. |
| SSH gateway | `desktop-socket.ts` (`openStream`, `connectThroughGateway`) | partial | `RdpTransport::Tunnel` / `VncTransport::Tunnel` take any `AsyncRead + AsyncWrite` stream; opening the `direct-tcpip` channel and the gateway lease is session-layer work. |
| Certificate trust: verify against Mozilla roots + OS store, host name check, TOFU pinning by SHA-256, mismatch warning | `server/src/remote-desktop/certificates.ts` | done | `presented_certificate` describes chain and name errors with Node's codes/messages (`DEPTH_ZERO_SELF_SIGNED_CERT`, `Hostname/IP does not match …`); `certificate_challenge` ported with its tests; the handler decides before any credentials go out. Pin storage is `muxus-store`. |
| Error sentences and retry decisions | `rdp-client.ts` (`describeRdpError`), `rdp-proxy.ts` (`connectErrorPdu`, `tlsErrorPdu`) | done | Socket errors map to the WSA sentences; CredSSP wrong password/logon failure and bare CredSSP/NTLM errors → `Credentials`; negotiation failures by code; legacy-RDP-only servers explained; certificate refusal → `Refused`. Tests ported. |
| Graphics into a shared framebuffer with dirty rectangles | ironrdp-web `session.rs`, `image.rs`, `canvas.rs` | done | `Framebuffer` (RGBA or BGRA, opaque), damage coalesced, one `Frame` event per batch. |
| Dynamic resize (display control) and Deactivate/Reactivate | `rdp-client.ts` (`resize`), view's 300 ms settle | done | Verified against IronRDP's server. Alpine xrdp 0.9.24 received the monitor layout but kept its size (as it would for the TypeScript build: same IronRDP code). The settle delay is the view's (`params::RESIZE_SETTLE_MS`). |
| Pointer shapes (default, hidden, bitmap, position) | ironrdp-web `set_cursor_style` | done | RGBA with hotspot; the browser's 32×32 downscale is not applied (a native cursor has no such limit). |
| Text clipboard both ways (lazy announce, remote fetch, force update, repeat suppression, off = no channel) | `rdp-client.ts` (`sendClipboardText`, callbacks), ironrdp-web `clipboard.rs` | done | CF_UNICODETEXT only, like the TypeScript client. An empty Format List ends CLIPRDR initialisation when there is no text yet. Tested against IronRDP's server and xrdp. |
| Session end reasons | `rdp-client.ts` (`run`), view `finish` | done | `SessionEnd::{Completed{reason}, Failed, Closed}` with the view's sentences. |
| Saved passwords (unlock, create vault, remember after success) | `server/src/remote-desktop/desktop-passwords.ts` | done | `DesktopPasswords` over the `DesktopVault`/`DesktopPrompts` traits (protocol-neutral; drive it for VNC too). Store implementation of the trait is session-layer work. |
| Logon prompt texts and answers | `desktop-socket.ts` (`askLogin`) | done | `LogonPrompt`. |
| Desktop session orchestration (saved-host resolution, credential flow, retry with `rejected`, record connection, remember after `connected`, save a typed user name, pin certificate / VNC key in the database, gateway close) | `desktop-socket.ts` (`DesktopSession`) | done | In `muxus_session::desktop` (`Backend::open_desktop_session`, `StoreDesktopVault`); see `remote-desktop-ui.md`. |
| Tickets, control and stream sockets, RDCleanPath codec, X.224 parser | `desktop-socket.ts`, `rdcleanpath.ts`, `x224.ts`, `rdp-proxy.ts` | n/a | Browser-only plumbing. Their user-visible behaviour is covered above; `rdcleanpath.test.ts` has nothing left to apply. |
| VNC protocol versions 3.3/3.7/3.8 (+ARD, AMT, RealVNC 4/5 aliases), UltraVNC repeater | noVNC `core/rfb.js` | done | |
| VNC security: None, VncAuth, RA2ne (RSA-AES, key verified before credentials), Tight (tunnels incl. Siemens quirk, sub-auth), VeNCrypt 0.2 non-TLS subtypes, Plain, XVP, Apple Remote Desktop, MSLogonII, Unix login | noVNC `rfb.js`, `ra2.js`, `crypto/*` | done | RustCrypto primitives; VNC DES key bits mirrored like d3des (checked against openssl). ARD, MSLogonII and RA2ne tested against server halves written in the tests. XVP sends an empty target (Muxus has no target field; noVNC asked again forever). |
| VeNCrypt TLS/X509 subtypes, RA2 with session encryption (types 5/129) | — | missing | noVNC lacks them too; the "may require TLS" sentence applies, as in the TypeScript build. |
| VNC encodings: Raw (incl. 8-bit AMT), CopyRect, RRE, Hextile, Zlib, Tight (fill/copy/palette/gradient/JPEG), TightPNG, ZRLE, JPEG (with RealVNC table reuse) | noVNC `core/decoders/*` | done | JPEG via zune-jpeg, PNG via png. |
| VNC H.264 encoding | noVNC `decoders/h264.js` (WebCodecs) | missing | Not advertised, so servers fall back to Tight. |
| VNC pseudo-encodings: LastRect, DesktopSize, ExtendedDesktopSize, DesktopName, cursor, VMware cursor, QEMU extended key + LED state, extended mouse buttons, fences, continuous updates, XVP, quality/compression levels | noVNC `rfb.js` | done | Cursor pixels decoded in the negotiated pixel format (R, G, B); noVNC swaps red and blue there. |
| VNC keyboard: DOM key → keysym (DOMKeyTable, keysymdef), XT scancodes for QEMU, held keys keep their keysym, release all on blur, Caps/Num Lock sync from the LED state, Windows AltGr merge, Japanese IM keys, macOS Alt/Cmd shuffle and quirks | noVNC `core/input/{util,keyboard,keysym*,domkeytable,xtscancodes,fixedkeys}.js`, `rfb.js` (`_handleKeyEvent`, `sendKey`) | done | Tables generated from noVNC 1.7.0 (`keysym_tables.rs`, generator kept out of the repo: a Node script importing those modules). Platform selectable (`Platform`) for tests. |
| VNC pointer: button mask incl. back/forward (extended pointer events), 17 ms move throttle, wheel accumulation (50 px step, 19 px lines) | noVNC `rfb.js` (`_handleMouse`, `_handleWheel`) | done | Touch gestures and viewport dragging are not used by Muxus. |
| VNC remote resize (`resizeSession`), rate limited, follows ExtendedDesktopSize | noVNC `rfb.js` (`_requestRemoteResize`) | done | Repeated same-size updates keep the picture (noVNC's display does the same). |
| VNC clipboard: classic Latin-1 and extended (caps, notify/request/provide/peek, deflate), view-only and sharing switches, repeat suppression | noVNC `rfb.js`, `vnc-client.ts` | done | |
| VNC view-only | `vnc-client.ts`, noVNC `viewOnly` | done | No keyboard, pointer, Ctrl+Alt+Del or clipboard in either direction. |
| VNC failures: `describeVncFailure`, security failure vs refusal vs rejected login, key not trusted, connect failure sentences | `vnc-client.ts`, view `onDisconnect`, `server/src/util/dial.ts` | done | Tests ported. A clean server close while connected is `Completed`; a reset or protocol error is `Lost` (the relay made both look clean in TypeScript). |
| VNC server key fingerprint, challenge, pinned subject | `vnc-client.ts` (`vncServerKey`), `desktop-socket.ts` (`acceptServerKey`) | done | `vnc_server_key`, `server_key_challenge`, `server_key_subject`; tests ported. |
| View helpers: fit rectangle, view → desktop point | `RemoteDesktopViewImpl.tsx` (`fitRect`, `desktopPoint`) | done | In both crates' `input` modules. Toolbar, overlays, auto-reconnect and dialogs are UI work. |

## API notes for the next wave

- Both crates: `connect(options, transport, handler) -> Result<Session, Failure>`; call inside a
  tokio runtime. The session runs on its own task; `events()` gives an `async_channel::Receiver`
  (take it from one place), `framebuffer()` a shared buffer. On every `Frame` event call
  `take_dirty()` and copy those rectangles; `Resized` is followed by a `Frame`.
- Input is in desktop pixels; map with `fit_rect` + `desktop_point`. `KeyEvent` mirrors DOM
  `KeyboardEvent` (`code`, `key`, modifiers incl. `alt_graph` and lock states): the GPUI view has
  to produce DOM `code`/`key` names from its keystrokes.
- `muxus_rdp::RdpSession::key` returns `false` for keys RDP cannot send (let the view handle
  them); `muxus_vnc::VncSession::key` returns `false` in view-only sessions.

## Attribution

- `muxus-vnc`: protocol logic, decoders, keyboard handling and the RSA-AES, ARD and MSLogonII
  exchanges adapted from [noVNC](https://github.com/novnc/noVNC) 1.7.0 `core/` (MPL-2.0, Copyright
  The noVNC authors). `src/keysym_tables.rs` is generated from noVNC's
  `core/input/{keysym,domkeytable,keysymdef,xtscancodes,fixedkeys}.js` (MPL-2.0; `keysymdef` and
  `xtscancodes` themselves derive from X.Org's keysymdef.h and the keymap-gen database). The
  MPL-2.0 is file-level: the adapted files (`crypto.rs`, `decoders.rs`, `input.rs`,
  `keysym_tables.rs`, `protocol.rs`, `session.rs`) carry the MPL notice, and the crate's
  `license` is `MIT AND MPL-2.0`.
- `muxus-rdp`: the session loop, input handling, clipboard flow and cursor handling follow
  IronRDP's `ironrdp-web` and `ironrdp-client` crates (MIT OR Apache-2.0, Copyright Devolutions
  Inc.); the integration tests use `ironrdp-server`.
