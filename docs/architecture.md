# Muxus in Rust: architecture

This workspace rebuilds Muxus (`../muxus`, TypeScript: React client, Node server, Electron shell) as a
single native Rust application. The goal is a 1:1 recreation: the same features, the same behaviour
and the same look. The TypeScript sources are the specification. When this document and the
TypeScript disagree, the TypeScript wins.

## Stack

- **UI:** [GPUI](https://www.gpui.rs) through `gpui-component` (pinned: `gpui-component = "=0.7.3"`,
  which pins `gpui = { package = "gpui-pre", version = "=0.3.9" }`). No webview.
- **Async backend:** tokio, owned by `muxus-session`. The UI never blocks on I/O. It talks to the
  backend through executor-agnostic channels (`async-channel`), so GPUI's executor and tokio never need
  to share a runtime.
- **SSH:** `russh` (with its `dsa` and `des` features for legacy network gear).
- **Terminal emulation:** `par-term-emu-core-rust` (kitty graphics, Sixel, iTerm2 images, kitty
  keyboard). Rendering is ours, in `muxus-term-view`.
- **Storage:** SQLite through `rusqlite` with the `bundled` feature.
- **TLS:** rustls everywhere. Never depend on `openssl-sys`.

## Crate layers

A crate may depend only on crates in lower layers. `cargo xtask layering` enforces this.

| Layer | Crates | Ported from |
|---|---|---|
| L0 | `muxus-core` | `shared/src` |
| L1 | `muxus-store` | `server/src/persistence`, `server/src/security` (secret storage), settings/prefs |
| L2 | `muxus-ssh`, `muxus-local`, `muxus-telnet`, `muxus-serial`, `muxus-rdp`, `muxus-vnc`, `muxus-mgmt`, `muxus-import`, `muxus-term` | `server/src/{ssh,forwards,x11,local,telnet,serial,remote-desktop,management}`, client import modules, `client/src/terminal` |
| L3 | `muxus-session` | `server/src/{ws,routes,session-logging,login-sequence,diagnostics,stats}` |
| L4 | `muxus-ui` | `client/src/theme.ts`, the shared MUI look |
| L5 | `muxus-term-view` | `client/src/components/TerminalViewImpl.tsx` and its addons |
| L6 | `muxus-app` | `client/src` components and state, `electron/src` |

L2 crates depend on `muxus-core` only. Terminal transports speak the byte-stream contract in
`muxus_core::stream` (`TransportChannels`: input of bytes, resize, break and close; events of bytes,
exit and error). They take their settings as arguments; they never read the
store themselves. `muxus-session` wires the store and the transports together.

## The UI ↔ backend contract

In TypeScript the client talks to the server over HTTP routes (`server/src/routes`) and the
`/ws/terminal` socket (`shared/src/ws-protocol.ts`). In Rust both become in-process calls:

- Route handlers become async methods on `muxus_session::Backend`.
- The terminal socket becomes a `SessionHandle`: an input channel (bytes, resize, prompt answers,
  close) and an event channel (bytes, status, host-key and auth prompts, exit). The message set is the
  one in `ws-protocol.ts`, ported to Rust enums in `muxus-core`.

## Data compatibility

Muxus-rs reads and writes the same on-disk data as Muxus: the SQLite database, its schema, the
`~/.ssh/config` edits, `known_hosts` and keyring entries. Both apps must be able to run on the same
data. Muxus-rs never runs a schema migration that Muxus does not know.

Tests and manual runs never touch the real home directory. Point `HOME` (and the `XDG_*` variables)
at a scratch directory, the way `../muxus/hack/demo-env.mjs` does.

## One widget kit, one theme

Generic controls (buttons, fields, selects, switches, progress, menus, dialogs, tooltips) live
in `muxus-ui` only; `TextField` and `Select` are shorthands for its one outlined field,
`FormField`. Colors come from the theme's tokens (`muxus_ui::theme`, including MUI's grey scale
and the code editors' Monaco theme in `muxus_ui::syntax`); content colors (host color choices,
file-type badges, demo data) live in a `palette.rs`. `cargo xtask style` fails on color
literals outside those files and on generic widget types defined outside `muxus-ui`.

## Look

The reference is the Electron app. `../muxus/docs/assets/screenshots/*.png` holds 66 screenshots
(1420×880 at 2×, light and `-dark` variants), produced by `../muxus/hack/capture.mjs` from the demo
sandbox in `../muxus/hack/demo-env.mjs`. Tokens come from `client/src/theme.ts` and the MUI defaults
it builds on. Fonts: Inter (variable), JetBrains Mono and the Nerd Font symbols the client bundles.

Chrome type is dense: weight 450, -0.1 tracking, 26 px sidebar rows with 13 px labels (folders 550),
softer light-mode ink (`#3f3f49`). No colored row stripes; a host color only tints its icon. The
active tab is not bolded.

## Conventions

- Port behaviour, not just shapes: carry the TypeScript tests over as Rust tests (`../muxus/tests/unit`).
- Each area tracks its status in `docs/parity/<area>.md`: one row per feature, with the TypeScript
  source it comes from and its state (`done`, `partial`, `missing`).
- Comments explain why, as in the TypeScript code. Keep the TypeScript comments that still apply.
- Do not copy code from GPL-licensed sources (the Zed application crates other than `gpui` are GPL)
  or from repositories without a license. MIT and Apache-2.0 code may be adapted with attribution in
  `ATTRIBUTION.md`.

## Building on Linux

```bash
cargo run                       # the app (default member: muxus-app)
cargo test -p muxus-ssh         # one crate
cargo xtask ci                  # fmt, clippy, tests, layering, style
```

`.cargo/config.toml` caps build jobs. Use `CARGO_TARGET_DIR` to give independent builds their own
target directory.
