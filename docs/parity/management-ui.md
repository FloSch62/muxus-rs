# Parity: management-ui (the NETCONF/gNMI workbench in `muxus-app`)

Folder: `crates/muxus-app/src/features/management/`. Ports
`client/src/components/ManagementViewImpl.tsx` and
`client/src/components/management/*` (including `tools/*`) on top of
`muxus-mgmt`'s workbench logic, plus the backend wiring in
`crates/muxus-session/src/management.rs`.

```bash
CARGO_TARGET_DIR=target cargo test -p muxus-app --lib features::management   # 74 tests
CARGO_TARGET_DIR=target cargo test -p muxus-session --lib management          # 3 tests
MUXUS_DATA_DIR=<scratch> cargo run -p muxus-app --example management
cargo run -p muxus-app --example management_device -- 57400                   # the fake device alone
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MGMT_SCENE=get \
  --env MUXUS_DATA_DIR=<scratch> out.png -- target/debug/examples/management
```

The `management` example starts a sandboxed `Backend`, serves the fake gNMI
device (`examples/support/device.rs`: an SR Linux-flavoured tree with 15
`srl_nokia` models, JSON_IETF/ASCII/PROTO, depth extension, wildcard keys,
Set, Subscribe stream/once/poll with ticking counters, SR Linux-style
"unknown element" errors; gNOI/gNSI answer `UNIMPLEMENTED`, as on a device
without them) and frames the view where the main window puts a session pane
(x 250, y 93 at 1420×880). `MGMT_SCENE=workbench|get|table|raw|history|device|live|set|review|operations|security|error|tool-<id>`
puts it into the state of a reference screenshot; `MGMT_DEVICE=host:port`
uses a real plain-text gNMI device instead.

## Hook points for the app shell

- `open_management_view(backend, profile, title, connect, window, cx) -> Entity<ManagementView>`
  opens the workbench for a `ManagementProfile` (gNMI or NETCONF).
- Subscribe to `ManagementViewEvent`: `Status { status: TabStatus, failure_reason, disconnect_reason }`
  for the tab dot, `Prompt(ManagementPrompt::{Auth, HostKey, Certificate})`
  for the shared login / host-key / certificate dialogs, `PromptsClosed`,
  and `Toast { status, message }`.
- Answer prompts with `answer_auth(Option<AuthPromptResponse>)`,
  `answer_host_key(bool)`, `answer_certificate(bool)`; `prompts()` lists
  the open ones.
- `set_active`, `set_title`, `set_auto_reconnect`, `reconnect`, `disconnect`.
- Backend: `Backend::open_management_session()`, `management_requests(protocol, profile_id)`,
  `save_management_request(body)`, `delete_management_request(id)`
  (`muxus_session::management`; vault-backed passwords, SSH leases from the
  muxed connection pool for NETCONF).

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Session lifecycle: connect, auth prompt, auto-reconnect with backoff, disconnect, end screens | `ManagementViewImpl.tsx` | done | `view.rs`; reconnect delays from `muxus_session::recovery` |
| Header: protocol tile, title/address, area switch, gNMI/NETCONF chips, TLS tooltip, reconnect/disconnect | `Workbench.tsx` | done | `workbench.rs` |
| Navigator: Explore (lazy tree, filter, scope), Saved, History, Device (gNMI info, models; NETCONF capabilities, notifications, get-schema) | `Navigator.tsx`, `DataTreeView.tsx` | done | `navigator.rs`, `tree_view.rs` (virtualized rows, keyboard) |
| Request editor: gNMI Get/Set/Subscribe forms, path completion from the explorer, NETCONF forms, code editors, Ctrl/Cmd+Enter | `RequestEditor.tsx`, `GnmiForm.tsx`, `NetconfForm.tsx`, `PathField.tsx`, `CodeEditor.tsx` | done | `editor.rs`, `actions.rs` |
| Results: run header, Tree/Table/JSON, NETCONF reply/XML/schema, RPC errors, error hints and option chips, empty state with quick starts, compare | `ResultPane.tsx` | done | `results.rs` |
| Live view: streaming table, freeze, sort, filter, CSV export, sparklines, poll | `LiveView.tsx` | done | `live.rs` |
| Review dialog (side-by-side diff), save/rename/delete requests, confirmations, commit-confirmed, candidate bar, gNMI commit bar | `ReviewDialog.tsx`, `CandidateBar.tsx` | done | `overlays.rs`, `bars.rs` |
| Node menus and actions (get, watch, set value, remove, edit-config, copy as gnmic/JSON/path) | `node-actions.ts` | done | `logic/node_actions.rs` + `actions.rs` |
| Tools: Ping, Traceroute, Files, Health, Processes, BGP, System, Reboot (typed confirmation) | `tools/PingTool.tsx`, `TracerouteTool.tsx`, `FilesTool.tsx`, `SystemTools.tsx` | done | `tools/{ping,files,system}.rs`; streams outlive tool switches as `activeStreams` does |
| Tools: Authorization, Path authorization, Certificates, SSH credentials, Accounting; rotation bar (finalize / roll back) | `tools/AuthzTool.tsx`, `PathzTool.tsx`, `CertzTool.tsx`, `CredentialzTool.tsx`, `AcctzTool.tsx`, `rotation.tsx` | done | `tools/{authz,pathz,certz,credentialz,acctz,rotation}.rs`; certz checks the presented certificate over a TLS probe |
| Pure helpers with tests | `components/management/*`, `management/tool-format.ts` | done | `logic/*` (values, path text, tree, path field, live, format, device, diff, editor, results, node actions, tools) and the tools' request/parse helpers |
| Backend wiring | `management-socket.ts`, `routes/management-requests.ts` | done | `muxus-session/src/management.rs` |

Tool state lives in the workbench store's `tools` map (as `useToolState`),
except the accounting records and the Files listing, which are not JSON and
live beside it in the view; both survive switching tools. Effects React runs
on mount and when the connection changes run the first time a tool renders
for a session.

## Screenshots against Electron

Electron (`../muxus/electron`) and the example run in the same headless
shell at 1420×880, scale 2; the fake device serves both. Share of differing
pixels in the session pane (1170×761 CSS px). Chrome-vs-Electron for the
same scenes is 0.86–2.42%, mostly text antialiasing.

| Scene | light | dark |
|---|---|---|
| workbench (empty result, quick starts) | 1.62% | 1.94% |
| get (tree) | 1.13% | 1.16% |
| table | 1.26% | 1.29% |
| raw (JSON editor) | 2.41% | 2.39% |
| history | 1.21% | 1.24% |
| device | 2.34% | 2.40% |
| set | 2.32% | 2.57% |
| review dialog | 2.53% | 2.51% |
| operations (Ping) | 1.39% | 1.46% |
| security (Authorization) | 1.92% | 2.28% |
| error (unknown element hint) | 1.73% | 1.80% |
| tool: Traceroute | 1.62% | 1.69% |
| tool: Files | 2.03% | 2.14% |
| tool: Health | 1.57% | 1.63% |
| tool: Processes | 1.39% | 1.46% |
| tool: BGP | 1.35% | 1.42% |
| tool: System | 2.03% | 2.40% |
| tool: Reboot | 2.15% | 2.50% |
| tool: Path authorization | 1.99% | 2.37% |
| tool: Certificates | 2.15% | 2.40% |
| tool: SSH credentials | 2.20% | 2.47% |
| tool: Accounting | 1.47% | 1.47% |
| live (values differ with timing) | 3.34% | n/a |

## Widget kit (`muxus-ui`)

The workbench draws with the shared kit; its folder keeps only workbench
compositions (`widgets.rs`: the forms' `FieldLabel`, labelled switches,
text-button tints) and the content colors of the diff view (`palette.rs`:
Monaco's diff scheme). What it needed was added to `muxus-ui`:

- `display::Chip`: `on_click`, `tooltip`, `min_width`, `opacity`,
  `label_padding`, `icon_size`, `icon_ink`, `icon_margin`, `on_delete`;
  `display::Alert::icon`.
- `button::Button`: `font_size`, `end_icon_turned`, `mono`.
- `toggle_button::ToggleButtonGroup`: `exclusive` (index-based, with
  `ToggleOption`/`toggle_options`), `inline` and `sizing` (the `py`/`px`/
  `fontSize` overrides), `mono`, `disabled`, `icon_style`. Labels now sit
  directly in the buttons: wrapped in another flex box, an inline group in a
  wrapping row measured zero wide.
- `form_field::FormField`: `compact` (`compactFieldSx`), `notch` (the
  surface behind a shrunk label off paper), `multiline`.
- `typography::{caps, inline_span}`; new `widgets/search_field.rs`
  (`input_base`, `input_base_disabled`, `search_field`) and
  `widgets/tooltip_lines.rs` (`LinesTooltip`, `TooltipExt`).
- Theme tokens `components.code_surface`, `backdrop`, `scrim`;
  `Theme::install` also switches the component layer's mode, so the code
  editor's gutter and current-line band follow dark mode.

## Differences and gaps

- Code editors use gpui-component's editor; syntax colors come from its
  global highlight theme, not Monaco's `vs`/`vs-dark`. A Monaco-like
  highlight theme in `muxus-ui` would close most of the remaining diff in
  the JSON/XML views.
- The shell owns the auth, host-key and certificate dialogs and the toasts:
  the view only emits `ManagementViewEvent::Prompt`/`Toast` (the example
  answers the login prompt with "skip").
- Downloads (Files tool, CSV/JSON exports) ask where to save with the
  platform dialog; the browser build triggers a download.
- A tool's free-solo autocomplete opens its list on focus as well as on
  typing (MUI opens it on click or typing).
- JSON errors in the authorization policy editor are serde_json's wording,
  not V8's.
- Accounting records render in a virtualized list (up to 5000 rows).
- Not verified against a live device in this pass (the fake device covers
  gNMI; NETCONF and gNOI/gNSI paths are exercised by `muxus-mgmt`'s own
  tests and the error paths here).

## Licensing

Own code. Dependencies added to `muxus-app`: `similar` (Apache-2.0/MIT)
for the review diff and `base64` (MIT/Apache-2.0) for certz and
credentialz; certz also uses `sha2` (MIT/Apache-2.0), already a dependency. The fake device uses `h2`, `http`, `prost` and the gNMI
message types from `muxus-mgmt` (dev-dependencies).

## Integration pass

- Management tabs run in the window (`ui/session_views.rs`) with the shared auth, host-key and
  certificate dialogs and toasts.
- Code editors now use the shared Monaco theme (`muxus_ui::syntax::code_editor_style`) and the
  shared tree-sitter highlighter (`util::code_editor`) on the base editor element, instead of
  gpui-component's own colors.
  The JSON result editor used to stop painting after its third line; it now shows the whole
  document in Monaco's colors (keys A31515, values 0451A5). The raw scene's share of differing
  pixels rose from 1.9% to 2.4% because twenty lines of text are compared instead of three.
  Fold chevrons now show on hover only (Monaco's `showFoldingControls: 'mouseover'`,
  `util::code_editor::mouseover_fold_icons`), and brackets are colored by depth with Monaco's
  three colors and its unexpected-bracket red (`color_brackets`, `BracketPools`). The editor
  text is no longer copied out on every frame.
