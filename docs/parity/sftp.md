# Parity: sftp (file browser, SFTP window, remote editor)

Feature folder: `crates/muxus-app/src/features/sftp/` (`pub mod sftp` in
`features/mod.rs`). Every file operation goes through `muxus_session::Backend`
(`sftp_list`, `sftp_download`, `sftp_drag_download`, `sftp_read_file`,
`sftp_save_file`, `sftp_upload`, `sftp_upload_drop`, `sftp_mkdir`,
`sftp_rename`, `sftp_delete`, `sftp_home`, `hold_sftp_window`,
`read_local_file`, `save_local_file`).

```bash
CARGO_TARGET_DIR=target-ui-b cargo test -p muxus-app --lib sftp::    # 232 tests
CARGO_TARGET_DIR=target-ui-b cargo test -p muxus-app --test ui_sftp   # the panel on a live SFTP server
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
| Size, speed, mtime formatting, parent path | components/TransferProgress.tsx, SftpPanel.tsx | done | `format.rs`; JS `toFixed` rounding |
| Column sort (name, size, modified; folders first, ties by name; a click reverses, another column starts A to Z or largest and newest) | client/src/sftp-sort.ts | done | `sort.rs`; sftp-sort.test.ts ported; names in `localeCompare` order |
| Rows on show (hidden files, name filter) and the empty-listing label | client/src/sftp-panel-state.ts | done | `placement.rs` (`visible_sftp_entries`, `empty_sftp_listing_label`); test file ported |
| Browser placement (pane/sidebar, show/toggle/move, unavailable reasons) | client/src/file-browser.ts | done | `placement.rs` over a `FileBrowserStores` trait the app implements; file-browser.test.ts ported |
| Panel width, initial path | sftp-panel-width.ts, sftp-panel-state.ts | done | both test files ported |
| Transfer progress state and labels | transfer-progress.ts, TransferProgress.tsx | done | `transfer.rs` |
| Why a transfer stopped (Cancel, a lost connection, the closing browser, a failure) | transfer-progress.ts (`TRANSFER_CANCELLED`, `CONNECTION_LOST`, `reportStoppedTransfer`), state/tabs.ts (`tabLostConnection`) | done | `transfer.rs`: `TransferStop` carries the reason an `AbortSignal` does, `stopped_transfer` says what to show; transfer-progress.test.ts ported. `state::tabs::tab_lost_connection` with its test |
| Continuing interrupted downloads (what a download received, by remote path, 256 MiB in all, newest kept; dropped by Cancel) | sftp-download.ts (`downloadResume`), api/transfers.ts (`downloadBlobWithProgress`) | done | `download.rs`: `InterruptedDownloads` in `SftpServices` (the window's memory, as in the client), `DownloadResume`, `download_with_progress`; the cases of sftp-download.test.ts and transfers.test.ts ported. The bytes are kept when the download's task is dropped too, as when its browser closes |
| Folder downloads (the walk, 10,000 entries and 10 GiB limits, the question with totals, skipped links, special files and names, numbered folders, one progress for the job, Cancel keeping what was saved, summaries) | sftp-download.ts (`downloadFromRemote`, `downloadSummary`) | done | `download.rs` over a `DownloadDeps` trait; sftp-download.test.ts ported against real folders. See "Known differences" for refused names and browsers without a folder picker |
| Terminal uploads (one per connection, phases, cancel, settle, browser count) | terminal-upload.ts, state/sftp-uploads.ts | done | `terminal_upload.rs` + `services::SftpUploads`; the "uploads started from a terminal" cases of sftp-upload.test.ts ported (the upload itself is muxus-ssh's, tested there) |
| Local copy sync (ask, auto-upload box, conflict, connection gone, cancel, fold saves) | local-copy-sync.ts, state/local-copies.ts | done | `local_copies.rs`; all of local-copy-sync.test.ts ported |
| Local open: chooser per platform | client/src/local-open.ts, electron/src/main.ts (`choose-program`) | done | Windows system chooser, the Linux program list, the macOS program picker (`chooseLocalProgram`: the native file picker, then `open -a <app> <file>`). On Linux, Browse… takes a `.desktop` file as well as an executable, with the Electron messages for one that cannot be read or describes no program (`check_picked_program`, tested); picked executables start in the home folder with the launch environment. The macOS branch is runtime `cfg!` code that builds and is type-checked on Linux but has not run on a Mac; GPUI's picker cannot start in `/Applications` or filter to `.app` bundles as Electron's dialog does |
| Local copies on disk (safe names, private folders, purge, launch env, opener, change detection) | electron/src/local-open.ts | done | `local_open.rs`; electron local-open.test.ts ported (names, copies, private root, purge, env, opener, signature, save detection). Saves are found by polling size/mtime every 700 ms, then hashing |
| Linux "Open with" catalog (desktop entries, Exec field codes, globs2, subclasses/aliases, mimeapps.list ranking, program icons) | electron/src/linux-applications.ts | done | `linux_apps.rs`; tests for entries, Exec, file types, ranking, the catalog and icon themes ported. Icons: `index.theme` program folders closest to 48px (`parse_icon_theme_index`), the desktop's theme (kdeglobals, `gsettings`, GTK 3 `settings.ini`), what it inherits, then hicolor/Adwaita/breeze, user icons first, pixmaps last; PNG or SVG drawn at 24px in the dialog (SVGs past 256 KiB left out, as `programIconDataUrl` does) |
| Language detection, Monaco labels | editor/language-detection.ts | done | `editor/language.rs`; test file ported. Tree-sitter grammar per Monaco id |
| Remote editor registry | editor/remote-editor-registry.ts | done | `editor/registry.rs`; test file ported |
| Terminal drop intent and path quoting | terminal/file-drop.ts, terminal/drop-paths.ts | done | rules are `muxus-term`'s (tested there); `terminal_drop.rs` adds the native intent (Shift from the window's modifiers) |
| Confirm and prompt dialogs | components/DialogHost.tsx, state/dialogs.ts | done | `dialogs.rs`: per-view `DialogQueue` and a window `DialogHost`; prompt rules tested |

## Views

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| File browser panel (header, toolbar, path field, actions, table, empty state, error, spinner, follow box, context menu with Compare with… (v0.15), rename/new folder/delete, upload picker, drop-in upload with overlay, download with progress and cancel, resize grip) | components/SftpPanel.tsx | done | `panel.rs`. Download asks where to save after the transfer (Electron's download dialog). Following the terminal's folder is idempotent: the shell syncs it on every render, which used to cancel the listing in flight and leave the old rows (`tests/ui_sftp.rs`). Without a file chooser portal, saves fall back to a free path in Downloads and pickers say so (`util/file_dialog.rs`) |
| Sortable column headers (arrow on the sorted column, a faint one on hover showing where a click starts) | SftpPanel.tsx (`TableSortLabel`) | done | `muxus_ui::widgets::table::TableSortLabel` with the panel's 14px arrow, before the label in the right-aligned Size column; the sort is the panel's state, kept across refreshes and folders |
| Name filter and the hidden-files button (Esc and a folder change clear the filter; hidden rows leave the selection and close their menu; the filter gets a row of its own in a narrow panel; the drag hint needs 480px) | SftpPanel.tsx, state/prefs.ts (`showHiddenFiles`) | done | `showHiddenFiles` is in `muxus-store` (default on, migration and backup with their tests) and shared by every file browser through `SettingsPrefs`. Actions in the client's order: New folder, Upload, Download, Open in editor |
| Downloading folders from the toolbar, the row menu and a selection ("Download 3 selected items") | SftpPanel.tsx (`downloadFiles`) | done | one file still asks where to save it; anything else asks for a folder once |
| Stopped transfers: Cancel says cancelled, a lost connection says so (*Upload stopped: connection lost*), a closing browser says nothing | SftpPanel.tsx, layout/AppShell.tsx, layout/Sidebar.tsx (`tabId`) | done | the shell and the docked browser tell the panel of the lost connection before it goes with its session (`SftpPanel::connection_lost`) |
| Drag out to download | SftpPanel.tsx (`DownloadURL`) | done | the file is fetched by its drag ticket when the drag starts and offered to the desktop as a file when the pointer leaves the window, finished or not: the download goes to a hidden `.<name>.part` beside the copy and takes the copy's name once complete (`create_pending_local_copy`, tested), so a drop made before then finds no file instead of half of one (Chromium's `DownloadURL` let the target wait for the download; GPUI hands the desktop a path). A ticket lasts as long as its connection, one per remote file across listings, all dropped when the connection closes, a system OpenSSH login as well as a built-in connection (6f67638; `muxus-ssh` `SftpService`, the sftp-upload tests ported) |
| Selecting several files (click, Ctrl/Cmd-click, Shift-click run from the anchor, Ctrl/Cmd+A, Esc, click below the rows, Delete / Cmd+Backspace), "N selected" caption, row menu acting on the selection (Compare with… only for a single file), bulk download into one folder with numbered names, one confirmation for a bulk delete | sftp-selection.ts, sftp-download.ts, SftpPanel.tsx (98411a3) | done | `selection.rs`, `download.rs` (both test files ported), `panel.rs`; the folder is picked with the desktop's folder chooser, files are written with `create_new` so nothing is replaced. `tests/ui_sftp.rs` drives the selection, the sort, the filter, a folder download and a download that continues on the next connection |
| Listing cache and refresh | api/queries.ts `useSftpList` | done | `services::SftpListings`: 15 s stale time, one retry, refresh per connection |
| File type icons | components/FileTypeIcon.tsx | done | Inter 800 stands in for the folder labels' 850 |
| Transfer progress bar | components/TransferProgress.tsx | done | indeterminate bar animated |
| Docked browser and sidebar rail | layout/Sidebar.tsx | done | `docked.rs`: `DockedFileBrowser` (held session, per-connection panel, unavailable state) and `sidebar_rail` (arrow keys, tooltips) |
| SFTP window | layout/SftpWindow.tsx | done | `window.rs`: lease held while open, files open beside the browser, Ctrl/Cmd+W closes the file in front, then the window. Client decorations with the window controls (`app_window_options`, as every window); without them GNOME on Wayland drew no frame and the window could not be closed |
| Open with dialog | components/OpenWithDialog.tsx | done | `open_with.rs`: search, Recommended/Other subheaders, arrows, Enter, double-click, Browse… |
| Remote editor workspace (strip with terminal entry, dirty dot, middle-click close, header, reload/download/save/save all, conflict alert, error and retry, loading) | components/RemoteEditorWorkspace.tsx | done | `editor/workspace.rs` |
| Editor (Monaco) | components/MonacoTextEditor.tsx | done | gpui-component code editor with tree-sitter and Monaco's `vs`/`vs-dark` token and editor colors; Ctrl/Cmd+S, Ctrl/Cmd+K S, Ctrl/Cmd+W, Ctrl/Cmd+PageDown/PageUp, Alt+Z, Ctrl/Cmd+G, Shift+Alt+F, F8/Shift+F8 (and with Alt), Escape for the problem widget; status bar (position opening Go to Line, selection, problem count, Wrap, `{ }`, indentation, EOL, UTF-8, language pickers). Bracket pair colors per kind (Monaco's three) and fold chevrons on hover. The open file takes the keyboard when it comes to the front, not on every sync (`tests/ui_editor.rs`). The language features below are `editor/features.rs`. The caret is the kit's: 1px wide since gpui-kit 0.9 (Monaco's `line` cursor is 2px) and blinking on and off (Monaco's `cursorBlinking: 'smooth'` fades); the kit offers neither a caret width nor a fade |
| Current-line box and overview ruler | Monaco `renderLineHighlight: 'all'`, `currentLineHighlight.ts`, `decorationsOverviewRuler.ts` | done | `editor/overlays.rs`: the 2px `editor.lineHighlightBorder` box around gutter and text (the text part only without a selection), ending where Monaco keeps the minimap's room (its `fit` width formula, tested against the reference's 86px); the ruler's half-pixel border, two lanes (occurrences left, problems right), six-pixel minimum marks merged per color, the cursor at 70%; lines mapped by the scroll height as Monaco maps them. Drawn from the caret of the same frame, under the text (the gutter part over the opaque gutter). With soft wrap the ruler maps model lines, not wrapped rows |
| Minimap | Monaco `minimap: { autohide: 'mouseover', renderCharacters: false, showSlider: 'mouseover', size: 'fit', maxColumn: 90 }` (right, scale 1), `minimap.ts` | done | `editor/minimap.rs` (tested): `_computeMinimapLayout` (`fit`, turning `fill` when the lines do not fit, sampling one line per device row past that), the lines as blocks in their tokens' colors (the highlighter's own runs, read through `HighlighterProbe`) drawn into an image once per change, the problem, occurrence and selection decorations, the slider (`minimapSlider.*` colors) where `MinimapLayout.create` puts it. Hidden until the pointer is over it, then faded in over 0.5s with the slider; a press anywhere moves the slider's middle there and drags it, as Monaco does for `fit`. The text area ends where it starts (the editor's right padding). With word wrap on, it maps model lines, not wrapped rows; the horizontal-scroll shadow on its left edge is not drawn |
| Occurrence and selection highlights | Monaco `occurrencesHighlight: 'singleFile'`, `selectionHighlight` | done | `editor/occurrences.rs` (tested): the word at the cursor under Monaco's default word definition, every whole-word case-sensitive occurrence (`editor.wordHighlightTextBackground`), and other occurrences of a one-line selection ignoring case (`editor.selectionHighlightBackground`); both marked in the ruler. Languages with a symbol highlighter in Monaco (TypeScript, CSS, HTML) get the textual one here. Documents past 4 MiB are not searched |
| Link detection | Monaco `links: true`, `linkComputer.ts` | done | `editor/links.rs` (Monaco's state machine ported with its tests' cases): `http(s)://` and `file://` links underlined in the text's color; Ctrl/Cmd+hover shows `editorLink.activeForeground` and a click opens the link with the desktop (the editor's go-to-definition gesture). Monaco's "Follow link (ctrl + click)" hover is not shown |
| Go to Line | Monaco `gotoLineQuickAccess.ts` | done | `editor/goto_line.rs` (tested): `line`, `line:col` / `,` / `#`, negative numbers from the end, `::offset`; Monaco's messages; the line previewed in the middle of the view with `editor.rangeHighlightBackground`; Enter goes there, Escape or leaving the field puts the view back. Drawn as Monaco's quick input (0.62 of the editor, at most 600px) |
| Format Document | Monaco `editor.action.formatDocument` | partial | `editor/formatting.rs`, with the file's tab size, spaces or tabs and line ending, as one undo step. JSON: `jsonc-parser`'s formatter (`editor/json.rs`). CSS, SCSS and Less: js-beautify's CSS beautifier as `vscode-css-languageservice` runs it, with its Monaco options (`editor/css_format.rs`, `editor/beautify.rs`; 36 cases checked against the bundled js-beautify under Node). Not ported: HTML (js-beautify's HTML beautifier with 120-column wrapping, `<style>` through the CSS beautifier and `<script>` left alone; about 1,700 lines of tokenizer and beautifier; `markup_fmt` formats Prettier-style, so it would not give Monaco's result) and JavaScript/TypeScript (TypeScript's own formatter, which only re-indents and re-spaces, needs TypeScript's parser; the one pure-Rust candidate, `dprint-plugin-typescript`, adds about 110 crates with swc, 5.4 MB to a release binary and 48 s to a clean debug build, and reflows code Prettier-style, changing quotes and semicolons). Those files are left as they are, as every other language is in Monaco |
| Problem markers | Monaco markers from its language workers | partial | Squiggles in `editorError`/`editorWarning` colors, the hover message, overview ruler and minimap marks, the status bar's ⓧ/△ count. JSON: `vscode-json-languageservice`'s syntax errors with Monaco's settings (trailing commas and comments are errors, duplicate keys warnings) and its codes (`editor/json.rs`, `editor/markers.rs`, tested). Other languages: the syntax errors of the tree-sitter parse the editor highlights with (`editor/syntax_problems.rs`, tested): a token the parser inserted is "expected", tokens it skipped are unexpected, an unclosed string is unterminated, worded as TypeScript's diagnostics (JavaScript, TypeScript and the other checked grammars) or the CSS service's (CSS). Not checked: SCSS and Less (shown with the CSS grammar), HTML (browsers accept what does not parse, and Monaco reports nothing), the SQL dialects, C and C++ (macros), Markdown. Not ported: TypeScript's semantic diagnostics and the CSS lint warnings (they need the TypeScript checker and the CSS property data) |
| Marker navigation widget | Monaco `editor.action.marker.next`/`prev`, `nextInFiles`/`prevInFiles` (F8, Shift+F8, Alt+F8, Shift+Alt+F8), `gotoErrorWidget.ts` | done | `editor/markers.rs` (`MarkerList`'s order and steps, tested), `editor/problem_widget.rs`: the peek view under the problem's line with its arrow, the frame and header in the severity's color, the file name and `N of M problems`, next/previous/close buttons, the message with its faded `source(code)`, stale (italic) when the text changed under it; the cursor goes to the problem and the view centers it when it was out of sight; Escape closes it (`tests/ui_editor.rs`). The editor has no view zones, so the widget lies over the lines below the problem instead of pushing them down; F8 walks the file in front only (the standalone editor cannot open another file either) |
| General Text tokenizer | client/src/editor/monaco-setup.ts | done | `editor/general_text.rs` (tested): the Monarch rules matched as Monarch does (against the rest of the line, `^` rules at its start), Monaco's token colors (`muxus_ui::syntax::monarch_color`), bracket colors in every token that is not a string, comment or regular expression, indentation folds; lines retokenized where an edit touched them |
| Terminal file drop overlay and handler | components/TerminalFileDrop.tsx | done | `terminal_drop.rs`; needs hooking into the terminal view |
| Corner bar for terminal uploads without a browser | terminal-upload.ts | done | `terminal_drop::terminal_upload_corner` |

## Screenshots (device pixels differing by more than 24/255)

Crops of the reference: the panel is `1040,93,380,787`, the editor
`251,93,789,787` (CSS px of the 1420×880 shots).

| Screen | Light | Dark | Main differences |
|---|---|---|---|
| sftp.png, panel | 1.66% | 2.44% | the drag hint is `textDisabled` as the current client draws it (the reference predates that); text ±1 device px; the dark reference has other timestamps |
| remote-editor.png, editor | 1.81% | 1.60% | the cursor (not drawn in the capture), text ±1 device px; the `yaml` label is `textDisabled`. Measured under Xvfb with lavapipe (`GPUI_X11_SCALE_FACTOR=2`), as this machine has no GNOME Shell; the minimap stays hidden as in the reference (no pointer over it); the earlier 2.09% (GNOME Shell) still lacked the current-line box, occurrence highlight, link underlines and overview ruler |
| remote-editor.png, panel | 1.67% | — | as sftp.png |

The sidebar and window scenes have no reference shots; they were checked by eye.

## Known differences

- **Names the local folder refuses.** Chromium's File System Access API turns down `aux.c`,
  `con.txt`, `desktop.ini` and `x.lnk` in a picked folder on every system, and the client skips
  them. A native folder takes these names except where Windows keeps them for devices
  (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`, whatever follows the first dot).
  Those are refused before anything is opened (`reserved_on_windows`), and a name the system
  itself turns down (too long, for one) is skipped the same way, listed as *name not allowed
  locally*.
- **Browsers without a folder picker.** Firefox and Safari cannot write into a folder, so the
  client sends files to the browser's downloads there and leaves folders out. The desktop
  always has a folder chooser; without a portal it says so and nothing is downloaded.
- **The download's cache headers** (`Cache-Control: no-store`) and Electron's answer to
  `file-system-access-restricted` have nothing to port: downloads are read straight from the
  SFTP session, and folders are picked with the desktop's chooser.
- **An upload's error is shown at once** without the `Connection: close` the HTTP route needed.

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

The remote editor's language features port Monaco Editor 0.57's link
computer, word definition, go-to-line parsing, current-line and overview
ruler drawing, minimap, marker navigation and problem widget, the
`jsonc-parser` scanner and formatter and `vscode-json-languageservice` syntax
checks it bundles (all MIT, Microsoft), and js-beautify's CSS beautifier (MIT)
as `vscode-css-languageservice` bundles it; see `ATTRIBUTION.md`. The General Text rules are Muxus's
own (`client/src/editor/monaco-setup.ts`).
