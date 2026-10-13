# Parity: host editor (`muxus-app/src/features/host_editor`)

Host editing screens: the host editor for every connection kind (SSH, Telnet,
serial, RDP, VNC, gNMI, NETCONF), the persistent tunnel editor, bulk edit,
organize, the folder dialog and the launch-group dialog.

```bash
CARGO_TARGET_DIR=target-ui-a cargo test -p muxus-app --lib host_editor
# screens (see the doc comment at the top of each example for the scenes)
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_EDITOR_SCENE=route out.png \
  -- target-ui-a/debug/examples/host_editor
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_DIALOG_SCENE=launch out.png \
  -- target-ui-a/debug/examples/host_dialogs
```

Both examples build the documentation's demo sandbox (`demo::DemoSandbox`: the
same `~/.ssh/config` and `config.d/lab.conf` that `hack/demo-env.mjs` writes,
in a scratch HOME) and seed it through `muxus_session::Backend`
(`demo::seed`: metadata, saved Telnet/serial/RDP/VNC hosts, logging policy,
three tunnels).

## Layout

| Path | Contents |
|---|---|
| `logic/` | Pure drafts and plans, no GPUI: `draft` (SSH), `native_draft` (Telnet/serial), `desktop_draft` (RDP/VNC), `management_draft` (gNMI/NETCONF), `advanced_options`, `session_logging`, `bulk_edit`, `forwards` (rule text + ForwardDiagram model), `tunnel`, `folder` (FolderDialog plan), `help` (field help texts), `js` (JS number/trim semantics) |
| `services.rs` | Every backend write in the TypeScript mutation order (save SSH/native/desktop/management host, preview, delete, metadata, bulk apply, folder moves/settings/password, tunnel save) |
| `widgets/` | Feature composites only: the editor shell and dialog frames (`shell`), section headings (`section`), host colour picker and ForwardDiagram (`visuals`), form state (`inputs`). Generic controls come from `muxus_ui::widgets` (see below) |
| `view/` | `HostEditor` (all kinds), sections in `ssh.rs`, `ssh_access.rs`, `terminal.rs`, `native.rs`, `desktop.rs`, `management.rs`, shared editors in `shared.rs` (LoginSequenceEditor, VaultSecretField, SessionLoggingPolicyFields, colour override, paste delay, keyword rules) |
| `dialogs/` | `tunnel`, `bulk`, `organize`, `folder`, `launch_group` |
| `palette.rs` | Content colours: colour-input choices, a new highlight rule's colours, demo data colours |
| `demo.rs` | Demo sandbox + seed used by the examples and the service tests |

### Shared widget kit

The generic controls this feature first drew itself now live in muxus-ui,
and the views use them:

| muxus-ui | What moved / was added |
|---|---|
| `widgets/form_field.rs` (new) | `FormField` + `FieldBody`: the full outlined form control (floating or shrunk label with notch, required mark, helper/error text, disabled, medium size, select arrow, adornments, custom select body), `end_adornment`, `helper_text` |
| `widgets/typography.rs` (new) | Variant accessors (`body1()`, `caption()`, …, `button(size)`, `rem`), `VariantExt` sx tweaks, `text`/`para`/`mono`/`inline_caption`, `weight::*` and `code_size::*` tokens |
| `widgets/listbox.rs` (new) | `listbox_popup` (select/autocomplete option list under a field), `list_subheader`, `filter_options` |
| `widgets/menu.rs` | `MenuItem::detail` (two-line option) and `MenuItem::highlighted` (keyboard focus) |
| `widgets/form_control.rs` | `FormControl::Switch`, `FormControlLabel::{label_element, small, indeterminate, flush, align_start, tooltip}`, `label_with_caption` |
| `widgets/controls.rs` | `Switch` disabled state as MUI draws it (track 0.12/0.2, grey 100/600 or tinted primary thumb) |
| `widgets/toggle_button.rs` | `ToggleButton::content`, `toggle_ink` |
| `widgets/display.rs` | `Alert` outlined variant (`Alert::outlined`, `AlertVariant::Outlined`); `Chip` clipped and centred so outlined chips keep their width in a wrapping row |
| `widgets/tabs.rs` | `Tab::{icon, vertical, fill, min_width, min_height, padding, indicator_offset}` |
| `theme.rs` | `components.{host_swatch_check, color_input_frame, outline_on_light, outline_on_dark}` |

Buttons, chips and dialog parts use the existing `Button`, `Chip` and
`dialog_paper`/`dialog_title`/`dialog_content`/`dialog_actions`. Spacing on
MUI's 8px scale goes through `Theme::spacing` (`stack`/`row` take spacing
units like `<Stack spacing>`), radii through `theme.radius`, weights and code
sizes through the typography tokens. What stays literal is off-scale MUI
geometry (the 14px input inset, 3px helper gap), component dimensions from the
client's `sx` (rail width, dialog sizes, named as constants) and the forward
diagram's own type sizes; the only literal colours left in the feature are
content (`palette.rs`) and test data.

## Status

| Feature | TypeScript source | State | Note |
|---|---|---|---|
| HostEditorDialog shell: type tabs, section rail, title/caption, Save / Save & connect / Delete with confirm, Escape/Enter, storage caption, dirty tracking, problem text | `HostEditorDialog.tsx`, `host-editor/*` | done | |
| SSH General (alias, display name, hostname, user, port, folder autocomplete, file select for new hosts, description, colour, storage OpenSSH vs Muxus) | `host-editor/GeneralSection.tsx` | done | Description is a single-line field (TS multiline grows) |
| SSH Authentication (default/key/password/agent/keyboard-interactive, key files with Browse, certificates, IdentitiesOnly, agent source + detection, forward agent) | `host-editor/AuthSection.tsx` | done | Browse uses the native file picker (`prompt_for_paths`) |
| SSH Route (direct, jump hops incl. saved-host hops, ProxyCommand) | `host-editor/RouteSection.tsx` | done | |
| SSH Forwards (rule list, add/edit form, ForwardDiagram) + X11 | `host-editor/ForwardsSection.tsx`, `ForwardDiagram.tsx` | done | |
| SSH Advanced (remote command, RequestTTY, host verification, keepalive, compression, legacy algorithm preset, extra options with unsupported warnings, live preview of the Host block) | `host-editor/AdvancedSection.tsx`, `advanced-options.ts` | done | |
| Appearance (scheme select with previews and custom schemes, font/background colour overrides, command button group, paste delays, SFTP / console compatibility) | `host-editor/AppearanceSection.tsx` | done | Colour override is a swatch popover, not the OS colour input |
| Login sequence editor | `LoginSequenceEditor.tsx`, `muxus_session::login_sequence::editor` | done | |
| Session logging policy fields (inherit switch, enable, capture input, retention, log to file) | `SessionLoggingPolicyFields.tsx` | done | |
| Keyword highlighting rules | `KeywordHighlightRulesEditor.tsx` | partial | Rule list editor only; the JSON mode is missing |
| Telnet / serial editors (port autocomplete with refresh, baud/data/parity/stop/flow) | `NativeHostEditor.tsx`, `native-draft.ts` | done | |
| RDP / VNC editors (general, logon, gateway route, options) | `RemoteDesktopHostEditor.tsx`, `desktop-draft.ts` | done | |
| gNMI / NETCONF editors (general, login, TLS security cards, files with Browse, route, options) | `ManagementHostEditor.tsx`, `management-draft.ts` | done | |
| VaultSecretField | `VaultSecretField.tsx` | partial | Field, missing-secret state and helper texts done; "New secret…" is listed but does not open the vault secret dialog yet |
| TunnelEditorDialog (saved host or custom connection, rule, diagram, auto-start/reconnect, Save / Save & start) | `TunnelEditorDialog.tsx` | done | |
| HostBulkEditDialog + plan | `HostBulkEditDialog.tsx`, `host-bulk-edit.ts` | done | |
| HostOrganizationDialog | `HostOrganizationDialog.tsx`, `host-organization.ts` | done | helpers already in `hosts::organization` (shell) |
| FolderDialog (new, edit with rename/re-parent/merge note, move hosts, style, shared credentials, folder password with master password, inherited login sequence) | `sidebar/FolderDialog.tsx` | done | |
| LaunchGroupDialog (layout cards, live-session warning, launch toast) | `sidebar/LaunchGroupDialog.tsx` | done | |
| Saved hosts helpers | `saved-hosts.ts` | done | already in `hosts::saved` (shell) |

## Tests

91 tests in the feature (`cargo test -p muxus-app --lib host_editor`).

| TypeScript test | Cases | Rust |
|---|---|---|
| `host-editor-draft.test.ts` | 14 | `logic::draft` (15) |
| `native-host-draft.test.ts` | 15 | `logic::native_draft` (16) |
| `host-bulk-edit.test.ts` | 18 | `logic::bulk_edit` (20) |
| `advanced-options.test.ts` | 6 | `logic::advanced_options` (6) |
| `host-organization.test.ts` | 5 | `hosts::organization` (shell) |
| `saved-hosts.test.ts` | 2 | `hosts::saved` (shell) |

Added: `session_logging` (5), `desktop_draft` (3), `management_draft` (2),
`folder` (6), `forwards` (3), `tunnel` (3), `help` (2), `js` (1), `demo` (1)
and `services` (8), which drives a sandboxed `Backend` over the demo data:
new OpenSSH host with forwards and metadata, alias rename, Muxus-owned SSH
profile create + update with a logging override, Telnet save and deletes,
bulk edit over two OpenSSH hosts, folder rename with shared credentials, tunnel
save, `shortenSshPath`.

## Screens

Compared with `hack/compare.py` on the dialog paper (device pixels, 2x).
After the move to the shared widget kit every scene was also diffed against
its own earlier shot: all are pixel-identical except sub-pixel chip shifts
(route, advanced), field-label truncation that now matches the client
(forwards) and the connection-type tabs, whose icon gap now matches it too
(new host).
"TS" is the current TypeScript client captured on the same demo sandbox and
scene; "docs" is the published screenshot in `docs/assets/screenshots`
(older captures, so content drift adds to the number).

| Scene | vs TS light | vs TS dark | docs reference | vs docs light | vs docs dark |
|---|---|---|---|---|---|
| SSH General | 2.16% | 2.02% | host-editor | 4.10% | 4.05% |
| SSH Route | 2.38% | 2.35% | host-editor-route | 3.82% | 4.11% |
| SSH Forwards | 2.60% | 2.47% | host-editor-forwards | 2.98% | 2.89% |
| SSH Authentication | 4.15% | 4.00% | | | |
| SSH Advanced | 4.15% | 3.96% | | | |
| Appearance | 3.17% | 2.98% | | | |
| Login sequence | 1.73% | 1.58% | | | |
| Session logging | 2.87% | 2.68% | | | |
| Highlighting | 1.65% | 1.50% | | | |
| Serial | 1.09% | 0.99% | serial-editor | 2.03% | 2.00% |
| Telnet | 1.50% | 1.42% | telnet-editor | 3.56% | 3.55% |
| RDP | 1.19% | 1.13% | | | |
| VNC | 1.43% | 1.38% | | | |
| New host | 2.27% | 2.17% | | | |
| Launch group | 2.92% | 2.79% | launch-group | 3.44% | 3.38% |
| Organize | 4.16% | 3.71% | | | |
| Folder (edit) | 4.02% | 3.90% | | | |
| Bulk edit | 1.27% | 1.16% | | | |
| Tunnel editor | 3.00% | 2.89% | | | |

`tunnels.png` in the docs is the forwarding panel, which belongs to another
feature. The remaining difference is glyph rasterization and 1–3 device pixels
of vertical drift that accumulates down long forms; Authentication and
Advanced are higher because the TS web capture has no Browse button next to
key files (the desktop build does).

## Hook points

Every entry point returns an entity that renders as a full-window overlay
(backdrop + dialog) and emits its events through `EventEmitter`; the owner
mounts it and drops it on `Close`.

| Call | Events | Owner does |
|---|---|---|
| `view::open_host_editor(window, cx, backend, HostEditorTarget::{New(NewHost), Edit(host), Duplicate(host)})` | `HostEditorEvent::{Close, Saved { connect }, Deleted, Toast}` | refresh host lists; open `ConnectTarget::{Alias, SavedHost}` for "Save & connect" |
| `dialogs::bulk::open_bulk_edit(window, cx, backend, host_keys)` | `BulkEditEvent::{Close, Toast}` | refresh host lists |
| `dialogs::organize::open_organize(window, cx, backend, host)` | `OrganizeEvent::{Close, Saved, Toast}` | refresh |
| `dialogs::folder::open_folder_dialog(window, cx, backend, FolderDialogState, FolderPrefs)` | `FolderDialogEvent::{Close, Submitted { prefs, toast }, Toast}` | apply each `FolderPrefsChange` to the folder prefs in order (`FolderPrefsChange::apply`) |
| `dialogs::launch_group::open_launch_group(window, cx, LaunchTarget, live_sessions)` | `LaunchGroupEvent::{Close, Launch { hosts, layout, label }}` | start the hosts in the `SessionSetLayout`, then show `launch_toast` |
| `dialogs::tunnel::open_tunnel_editor(window, cx, backend, TunnelEditorState)` | `TunnelEditorEvent::{Close, Saved { record, start }, Toast}` | start or restart the tunnel when `start` |

`HostEditor::set_section` / `set_kind` open a specific section or kind (the
examples use them); `services` can be called directly for writes without UI.

## Known differences

- "New secret…" in VaultSecretField does not open the vault secret dialog
  (`features::vault::open_vault_secret_dialog` exists; needs a callback from
  the editor that mounts it and writes the new id back).
- KeywordHighlightRulesEditor has no JSON mode.
- Colour override uses a swatch popover instead of the browser's colour input.
- Description is single-line.
- Autocomplete fields have no clear (×) button; select menus have no arrow-key
  navigation (autocomplete popups do).

## Attribution

No third-party code was adapted.

## Integration pass

- Every entry point is wired (`ui/host_actions.rs`): sidebar host/folder/panel menus, the Add
  host card and button, Save & connect, deletes with the client's questions, order writes, drag
  and drop, the launch group dialog, "New secret…" (the vault's dialog; the field takes the new
  secret and the editor its vault status).
- `TextField` and `Select` now build `FormField`, so the shrunk label sits where FormField puts
  it (the 1px offset noted above is gone).
- The session logging draft is the shared `model::session_logging`; the demo colors live in
  `model::palette`.
