# Parity: quick launcher, workspaces, session history, command buttons

`muxus-app/src/features/{launcher,workspaces,history,command_buttons}`: the quick launcher and
quick connect (`QuickLauncherDialog.tsx`, `quick-launcher.ts`, `HostPickerPopover.tsx`,
`session-launch-guard.ts`, `host-sessions.ts`), workspaces (`WorkspaceDialog.tsx`,
`workspace-list.ts`, `workspace-sync.ts`, `state/workspaces.ts`), session history
(`SessionHistoryDialog.tsx`, `session-history-matches.ts`) and command buttons with
multi-execution (`CommandButtonsDialog.tsx`, `CommandButtonMenu.tsx`, `ActionBar.tsx`,
`command-button-groups.ts`, `MultiExecControl.tsx`, `state/multi-exec.ts`). Every read and
write goes through `muxus_session::Backend`; the command button list logic itself is
`muxus_store::prefs::command_buttons` (its 21 ports of `command-buttons.test.ts` live in
`crates/muxus-store/tests/prefs_command_buttons.rs`).

```bash
CARGO_TARGET_DIR=target-ui-c cargo test -p muxus-app --lib -- features::launcher \
  features::workspaces features::history features::command_buttons          # 72 tests
CARGO_TARGET_DIR=target-ui-c cargo build -p muxus-app --example launcher --example workspaces \
  --example history --example command_buttons
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env TZ=UTC --env MUXUS_THEME=dark \
  --env MUXUS_COMMAND_SCENE=dialog out.png -- target-ui-c/debug/examples/command_buttons
```

The examples run on the documentation's demo sandbox (`features::launcher::demo`, which
seeds `hack/demo-env.mjs` through `host_editor::demo` into a scratch data folder and HOME)
with the screenshot preferences of `hack/capture.mjs`, recorded demo sessions and saved
workspaces. Every example takes `MUXUS_THEME=light|dark`, `MUXUS_WINDOW=fullscreen` and
`MUXUS_DATA_DIR`; and:

| Example | Scenes |
|---|---|
| `launcher` | `MUXUS_LAUNCHER_QUERY` (default `web`; empty shows the suggestions over seeded workspaces, use `--wait 12`), `MUXUS_LAUNCHER_SCENE=picker` (the empty pane's host picker) |
| `workspaces` | `MUXUS_WORKSPACES_SCENE=catalog` (default, unsaved window) or `active` |
| `history` | `MUXUS_HISTORY_QUERY` (search with highlighted hits) |
| `command_buttons` | `MUXUS_COMMAND_SCENE=bar` (default), `group-menu`, `menu` (Ctrl+Space at the caret), `dialog`, `multi-exec`, `multi-exec-off` |

## Hook points for the app

| API | What it does |
|---|---|
| `launcher::open_quick_launcher(backend, LauncherContext { tabs, active_id, shown_command_group, wsl_shell_profiles, workspaces }, window, cx)` | The Ctrl+K launcher as an overlay entity. `set_context` refreshes the tabs while open, `set_query` pre-fills it. It emits `LauncherEvent::{Close, ActivateTab { tab_id, reconnect }, ActivateEditor, ConnectHost(ManagedHost), ConnectTarget(String), OpenLocalShell(profile), OpenHistory { query, selected_id }, Action(LauncherAction), RunKeyCommand(id), StartTunnel(record)}`; workspaces open through the passed `Workspaces` entity, command buttons run through `CommandHooks`, tunnels stop through the backend. |
| `launcher::open_host_picker(backend, position, replace_tab_id, window, cx)` | The empty pane's "Saved host" popover; `HostPickerEvent::{Close, Connect { host, replace_tab_id }, AddHost}`. |
| `launcher::sessions::{host_session_tabs, open_local_shell_profile, open_default_local_terminal, local_shell_launch}` | Which tabs belong to a host, and opening local shells into `TabsState` (ports of host-sessions / session-launch-guard; the guard itself is core's `LaunchGuard`). |
| `workspaces::Workspaces::new(backend, &WorkspaceBus, Rc<dyn WorkspaceLayoutHost>, initial)` then `start(cx)` | One per window (`useWorkspacesStore` + `workspace-persistence.ts`). The window calls `layout_changed(cx)` after every layout change (350 ms debounced autosave, retry, locked/404 handling), `window_focused()`, `flush_on_unload(cx)` and `stop(cx)` on close. `save_as / save / rename / open / set_startup / set_locked / delete / refresh_catalog / focus_open_workspace / request_presence` return tasks. |
| `WorkspaceLayoutHost` (the window implements it) | `snapshot` (use `workspaces::layout::window_snapshot(&tabs, &multi_exec_state)`), `is_blank`, `restore(layout, groups, connect_remote, cx)` (restore synchronously, including `MultiExec::set_groups`), `active_workspace_changed`, `focus_window`, `connect_remote_on_restore`. `layout::{layout_to_core, layout_from_core}` convert layouts. |
| `workspaces::WorkspaceBus` | The in-process stand-in for `BroadcastChannel` that keeps windows' catalogs, presence and focus in sync: one per process, shared by every window's `Workspaces`. |
| `workspaces::open_workspaces(workspaces, WorkspaceDialogContext { tabs }, window, cx)` | The dialog; `WorkspaceDialogEvent::{Close, NewWindow { name }, OpenInNewWindow { id, name }, Reconnect(ids), ReconnectAll, ForceReconnectAll}`. |
| `history::open_history(backend, initial_query, initial_selected_id, window, cx)` | The session history dialog; `HistoryEvent::Close`. Exports use `crate::export` and the file save prompt. |
| `command_buttons::CommandButtons::new(backend, cx)` + `set_tabs(&tabs, cx)` | One per window: active tab, the window-local per-tab group choice (#210), host groups, vault state. |
| `command_buttons::action_bar(model, cx)` | The bar; `ActionBar::visible(cx)` / `ActionBar::position(cx)` say whether and where (top or bottom) the window mounts it; `ActionBarEvent::ManageCommands`. |
| `command_buttons::open_command_menu(model, window, cx)` | The Ctrl+Space menu at the caret (`CommandHooks::cursor_anchor`, else the window centre); `CommandMenuEvent::{Close, ManageCommands}`. |
| `command_buttons::open_command_buttons(model, window, cx)` | The editor dialog; `CommandButtonsEvent::{Close, OpenPasswordSettings}`. |
| `command_buttons::set_command_hooks(CommandHooks { send_input, focus_terminal, send_secret, cursor_anchor }, cx)` | How a command reaches a terminal. `run::send_secret_to_terminals` is the shared secret path (the vault feature uses it). |
| `command_buttons::MultiExec::new()` + `set_tabs(&tabs, cx)`, `toggle(&tabs, cx)`, `broadcast_input / broadcast_paste` (with a `MirrorTargets` implementation), `pane_ids(cx)` | Multi-execution state (`state/multi-exec.ts`). `toggle` returns its toast text through the toast hook. |
| `command_buttons::multi_exec_control(model, window, cx)` | The top bar button with its popover (`open`, `set_tabs`). |
| `launcher::kit::set_toast_hook(fn, cx)` | Where these screens' toasts go (stderr until set). |
| `launcher::kit::AskDialog` | The confirm/prompt dialog the screens use (`AskRequest`, `PromptField`, `AskAnswer`). |

## Status

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Quick launcher: catalog (tabs, editors, hosts, ad-hoc `user@host`, workspaces, local shells and WSL, commands of the shown group, tunnels, actions, shortcuts), ranking and the 40-result limit, suggestions when empty, history hits (≥2 chars, 250 ms), keyboard (search ↔ list, Enter, Esc), busy states, unsaved-workspace alert | QuickLauncherDialog.tsx, quick-launcher.ts | done | `search.rs`, `catalog.rs`, `view.rs` |
| Host picker of an empty pane | HostPickerPopover.tsx | done | |
| Launch guard, host sessions, local shell launch | session-launch-guard.ts, host-sessions.ts | done | |
| Workspaces dialog: current workspace actions, new / save / save as / lock / rename / restore, startup, delete (confirm), search, sort, open, open in new window, other-window presence, reconnect list | WorkspaceDialog.tsx | done | discarding open remote editors on open (`confirmDiscardRemoteEditors`) is not asked yet |
| Workspace persistence: autosave, catalog, presence and focus across windows | state/workspaces.ts, workspace-persistence.ts, workspace-sync.ts | done | the model is unit tested; the GPUI entity is exercised by the examples only |
| Session history: filters (search, host, connection, dates), paging, detail with transcript and highlighted hits, exports (HTML replay, clean log, raw log), copy, pin, rename, delete | SessionHistoryDialog.tsx, session-history-matches.ts | done | dates are en-US |
| Command bar: buttons with accents, missing-secret state, group picker (counts, "Opens with", per-host group), context menu (move bar, manage) | ActionBar.tsx, command-button-groups.ts | done | |
| Ctrl+Space menu: search, sections, keyboard, secrets | CommandButtonMenu.tsx | done | |
| Command buttons dialog: bar switch and position, groups (add, rename, move, delete with confirm), cards (label and color, command or secret, run immediately, group, move, duplicate, delete), new secret | CommandButtonsDialog.tsx | partial | the Command field is one line; the TypeScript field grows to three (`multiline maxRows={3}`) |
| Multi-execution: control, popover (this split / visible / all live, targets, groups save/activate/delete, clear), mirroring | MultiExecControl.tsx, state/multi-exec.ts | done | |

## Tests

| TypeScript test | Cases | Rust |
|---|---|---|
| quick-launcher.test.ts | 4 | `launcher::search` (5), `launcher::catalog` (7) |
| session-launch-guard.test.ts, host-sessions.test.ts | 10 + 3 | `launcher::sessions` (14) |
| workspace-list.test.ts | 3 | `workspaces::list` (5) |
| workspace-sync.test.ts | 2 | `workspaces::sync` (3) |
| session-history-matches.test.ts | 5 | `history::matches` (7) |
| multi-exec.test.ts | 14 | `command_buttons::multi_exec` (14) |
| command-buttons.test.ts | 24 | muxus-store (21); here `command_buttons::{groups, run, dialog}` (5) |

Plus `launcher::time_format` (3), `history::format` (4), `history::view` (1),
`workspaces::{layout, model}` (4).

## Measured against the reference

References are fresh captures of the TypeScript app (Chromium, 1420×880 at 2×, `TZ=UTC`) by
`hack/capture.mjs` plus six extra shots in the same style (empty launcher, host picker,
command dialog, command menu, group menu, multi-exec off). `hack/compare.py --crop` on the
screen's region, light / dark:

| Screen | Crop (CSS px) | Light | Dark |
|---|---|---|---|
| Quick launcher (`web`) | 409,88,600,469 | 2.24% | 2.27% |
| Quick launcher (empty) | 409,88,600,530 | 2.26% | 2.11% |
| Host picker | 740,360,365,520 | 2.40% | 2.17% |
| Workspaces | 255,176,910,530 | 1.87% | 1.72% |
| Session history | 110,44,1200,792 | 2.23% | 2.24% |
| Command bar | 0,53,1420,35 | 3.86% | 3.85% |
| Group picker | 10,80,310,200 | 3.39% | 3.17% |
| Ctrl+Space menu | 405,160,310,365 | 2.22% | 2.51% |
| Command buttons dialog | 260,80,900,720 | 3.18% | 3.08% |
| Multi-execution | 676,40,370,375 | 4.99% | 4.50% |
| Multi-execution (off) | 676,40,370,215 | 4.61% | 4.16% |

What remains is glyph rasterization (GPUI rounds glyph advances, so long labels end up to
1px wider: the bar buttons drift by a pixel each), timestamps (the examples seed at run
time), the reference pointer resting on "All live" in the multi-execution shot (hover
tint), and the backgrounds behind popovers (the examples have no terminal). The command
dialog's caption lines under "Run immediately" sit 1.5px lower than Chrome's (muxus-ui
`inline_caption`).

## Added to muxus-ui

- `widgets/list.rs` (new): `ListItemButton` (theme margins and corners, collapsed row
  margins, dividers, selection), `list_item_icon`, `list_item_text`, `pitch_correction`,
  `ListSubheader` (`compact`, `caps`).
- `dialog.rs`: `modal_at_top`, `dialog_title`, `dialog_actions`. `display.rs`:
  `Alert::square`. `progress.rs`: `linear_progress_indeterminate`. `text_field.rs`:
  `strong_placeholder`. `select.rs`: the label sits inside an empty, closed field.
- `button.rs`: `ButtonTint`, `Button::{tint, end_icon, padding_x, padding_y,
  start_icon_style, dashed, max_width}`, `IconButton::background`. `controls.rs`:
  `Checkbox::checked_color`. `tabs.rs`: `Tab::{content, font_size}`, `LINE_HEIGHT`.
- `menu.rs`: `popover_paper`, `MenuItem::{prefix, focused}`; `popup` now keeps MUI's 16px
  margin on the sides it opens from, and takes back the frame's client inset in a
  maximized or fullscreen window (GPUI adds it to the snap margin although no shadow is
  drawn there, so menus stopped 20px short of the bottom and right edges).
- `theme.rs`: typography `chord`, `chord_small`, `search_input`, `tag`, `code`;
  `fonts::SYSTEM_MONO`. `typography.rs`: `baseline(variant)`.

Two things for other screens: `tooltip::bounds_probe` in a parent that is not a flex box
records the bounds *below* the control (its static position follows the control), and a
popup placed from bounds of the window's first frames stays there unless something
re-renders (`launcher::kit::follow_anchor` asks for one more frame when the anchor moved).

## Attribution

No third-party code was adapted. `unicode-normalization` (MIT/Apache-2.0) is a dependency
for the history search's accent folding.

## Integration pass

The launcher, host picker, workspaces, history and command buttons are wired into the window
(`app.md`). `launcher::kit::follow_anchor` moved into `muxus_ui::widgets::tooltip` (re-exported
for compatibility), and `bounds_probe` now pins itself to its parent's top-left corner, which
fixes the bounds recorded inside a parent that is not a flex container.
