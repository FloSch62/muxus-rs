# Parity: sftp (file browser, SFTP window, remote editor)

Feature folder: `crates/muxus-app/src/features/sftp/` (`pub mod sftp` in
`features/mod.rs`). Every file operation goes through `muxus_session::Backend`
(`sftp_list`, `sftp_download`, `sftp_drag_download`, `sftp_read_file`,
`sftp_save_file`, `sftp_upload`, `sftp_upload_drop`, `sftp_mkdir`,
`sftp_rename`, `sftp_delete`, `sftp_home`, `hold_sftp_window`,
`read_local_file`, `save_local_file`).

```bash
CARGO_TARGET_DIR=target-ui-b cargo test -p muxus-app --lib sftp      # 89 tests
CARGO_TARGET_DIR=target-ui-b cargo build -p muxus-app --example sftp
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=sftp out.png -- target-ui-b/debug/examples/sftp
```

The example starts the in-process SSH/SFTP server of `muxus_ssh::testing`
with web-01's `/home/deploy` from `hack/demo-env.mjs` (entries dated
2026-07-25 20:17), a sandboxed `Backend`, signs in and shows a scene:
`MUXUS_SCENE=sftp` (beside the terminal), `remote-editor`, `sidebar`,
`window`; `MUXUS_THEME=dark`.

## Logic (UI-free, tested)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| File and folder icon resolver | client/src/file-icon-resolver.ts | done | `icons.rs`; test file ported plus pattern edge cases |
| Size, speed, mtime formatting, parent path, entry order | components/TransferProgress.tsx, SftpPanel.tsx | done | `format.rs`; JS `toFixed` rounding, `localeCompare` order |
| Browser placement (pane/sidebar, show/toggle/move, unavailable reasons) | client/src/file-browser.ts | done | `placement.rs` over a `FileBrowserStores` trait the app implements; file-browser.test.ts ported |
| Panel width, initial path | sftp-panel-width.ts, sftp-panel-state.ts | done | both test files ported |
| Transfer progress state and labels | transfer-progress.ts, TransferProgress.tsx | done | `transfer.rs` |
| Terminal uploads (one per connection, phases, cancel, settle, browser count) | terminal-upload.ts, state/sftp-uploads.ts | done | `terminal_upload.rs` + `services::SftpUploads`; the "uploads started from a terminal" cases of sftp-upload.test.ts ported (the upload itself is muxus-ssh's, tested there) |
| Local copy sync (ask, auto-upload box, conflict, connection gone, cancel, fold saves) | local-copy-sync.ts, state/local-copies.ts | done | `local_copies.rs`; all of local-copy-sync.test.ts ported |
| Local open: chooser per platform | client/src/local-open.ts | partial | Windows system chooser and the Linux program list; the macOS program picker is missing |
| Local copies on disk (safe names, private folders, purge, launch env, opener, change detection) | electron/src/local-open.ts | done | `local_open.rs`; electron local-open.test.ts ported (names, copies, private root, purge, env, opener, signature, save detection). Saves are found by polling size/mtime every 700 ms, then hashing |
| Linux "Open with" catalog (desktop entries, Exec field codes, globs2, subclasses/aliases, mimeapps.list ranking) | electron/src/linux-applications.ts | partial | `linux_apps.rs`; tests for entries, Exec, file types, ranking and the catalog ported. Program icons (icon themes) missing |
| Language detection, Monaco labels | editor/language-detection.ts | done | `editor/language.rs`; test file ported. Tree-sitter grammar per Monaco id |
| Remote editor registry | editor/remote-editor-registry.ts | done | `editor/registry.rs`; test file ported |
| Terminal drop intent and path quoting | terminal/file-drop.ts, terminal/drop-paths.ts | done | rules are `muxus-term`'s (tested there); `terminal_drop.rs` adds the native intent (Shift from the window's modifiers) |
| Confirm and prompt dialogs | components/DialogHost.tsx, state/dialogs.ts | done | `dialogs.rs`: per-view `DialogQueue` and a window `DialogHost`; prompt rules tested |

## Views

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| File browser panel (header, toolbar, path field, actions, table, empty state, error, spinner, follow box, context menu, rename/new folder/delete, upload picker, drop-in upload with overlay, download with progress and cancel, resize grip) | components/SftpPanel.tsx | done | `panel.rs`. Download asks where to save after the transfer (Electron's download dialog). Following the terminal's folder is idempotent: the shell syncs it on every render, which used to cancel the listing in flight and leave the old rows (`tests/ui_sftp.rs`). Without a file chooser portal, saves fall back to a free path in Downloads and pickers say so (`util/file_dialog.rs`) |
| Drag out to download | SftpPanel.tsx (`DownloadURL`) | partial | the file is fetched by its drag ticket when the drag starts and offered to the desktop as a file when the pointer leaves the window; a file still downloading at that moment is not offered |
| Listing cache and refresh | api/queries.ts `useSftpList` | done | `services::SftpListings`: 15 s stale time, one retry, refresh per connection |
| File type icons | components/FileTypeIcon.tsx | done | Inter 800 stands in for the folder labels' 850 |
| Transfer progress bar | components/TransferProgress.tsx | done | indeterminate bar animated |
| Docked browser and sidebar rail | layout/Sidebar.tsx | done | `docked.rs`: `DockedFileBrowser` (held session, per-connection panel, unavailable state) and `sidebar_rail` (arrow keys, tooltips) |
| SFTP window | layout/SftpWindow.tsx | done | `window.rs`: lease held while open, files open beside the browser, Ctrl/Cmd+W closes the file in front, then the window. Client decorations with the window controls (`app_window_options`, as every window); without them GNOME on Wayland drew no frame and the window could not be closed |
| Open with dialog | components/OpenWithDialog.tsx | done | `open_with.rs`: search, Recommended/Other subheaders, arrows, Enter, double-click, Browse… |
| Remote editor workspace (strip with terminal entry, dirty dot, middle-click close, header, reload/download/save/save all, conflict alert, error and retry, loading) | components/RemoteEditorWorkspace.tsx | done | `editor/workspace.rs` |
| Editor (Monaco) | components/MonacoTextEditor.tsx | partial | gpui-component code editor with tree-sitter and Monaco's `vs`/`vs-dark` token and editor colors; Ctrl/Cmd+S, Ctrl/Cmd+K S, Ctrl/Cmd+W, Ctrl/Cmd+PageDown/PageUp, Alt+Z; status bar (position, selection, Wrap, indentation, EOL, UTF-8, language pickers). Bracket pair colors per kind (Monaco's three) and fold chevrons on hover. The open file takes the keyboard when it comes to the front, not on every sync (`tests/ui_editor.rs`). Missing: Monaco's current-line box, occurrence highlight, link detection, minimap and overview ruler, go to line, format document, problem markers, the General Text tokenizer |
| Terminal file drop overlay and handler | components/TerminalFileDrop.tsx | done | `terminal_drop.rs`; needs hooking into the terminal view |
| Corner bar for terminal uploads without a browser | terminal-upload.ts | done | `terminal_drop::terminal_upload_corner` |

## Screenshots (device pixels differing by more than 24/255)

Crops of the reference: the panel is `1040,93,380,787`, the editor
`251,93,789,787` (CSS px of the 1420×880 shots).

| Screen | Light | Dark | Main differences |
|---|---|---|---|
| sftp.png, panel | 1.66% | 2.44% | the drag hint is `textDisabled` as the current client draws it (the reference predates that); text ±1 device px; the dark reference has other timestamps |
| remote-editor.png, editor | 2.09% | 1.98% | Monaco's current-line box, cursor, occurrence highlight, link underline, bracket colors and overview ruler; the `yaml` label is `textDisabled` |
| remote-editor.png, panel | 1.67% | — | as sftp.png |

The sidebar and window scenes have no reference shots; they were checked by eye.

## Needs from other crates

- `muxus-session`: `Backend::sftp_upload` cannot be cancelled (it spawns the
  work and dropping the future leaves it running). The local-copy upload's
  Cancel therefore only stops waiting. It needs a `CancelSignal` argument (as
  `sftp_upload_drop` has) or abort-on-drop.
- `muxus-ui`: `CircularProgress`, an indeterminate `LinearProgress`, a
  `TextField` with a floating label, helper text and a font override, and a
  table; local versions live in `widgets.rs`, `dialogs.rs` and `panel.rs`.

## Attribution

`editor/highlight.rs` adapts the tree-sitter highlighter adapter and fold
range extraction of gpui-component 0.7.3
(`src/highlighter/input_adapter.rs`, Apache-2.0). Monaco's language aliases
and theme colors (MIT) were used as data.

## Integration pass

- Wired into the window (`app.md`): the panel beside SSH terminals, the browser docked in the
  sidebar with its rail, the SFTP window, the remote editor above its terminal, file links
  from terminals, drop to upload or type paths, unsaved-file questions before tabs close.
- `widgets.rs` is gone: the spinner is `muxus_ui::widgets::progress::CircularProgress`, text
  widths `muxus_ui::text::measure_text`, the path field and the prompt field are `FormField`.
- The editor's Monaco colors are `muxus_ui::syntax` (shared with the management workbench);
  the tree-sitter highlighter is `util::code_editor`. File-type badge colors are in
  `sftp/palette.rs`; `placement::FileBrowserPosition` is the store's.
