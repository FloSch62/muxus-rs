# Parity: terminal

The terminal emulator and its view: the native counterpart of xterm.js 6.1 and its addons
(clipboard, fit, image, search, serialize, unicode11, web-links, webgl) as wrapped by
`client/src/components/TerminalViewImpl.tsx` and `client/src/terminal/`.

Crates:

- `muxus-term` (L2, no GPUI): the emulator (`par-term-emu-core-rust` behind a stream filter),
  schemes, keyboard and mouse encoding, selection, search, links, keyword highlighting,
  serialization and every pure helper of `client/src/terminal/`.
- `muxus-term-view` (L5, GPUI): `TerminalView`, which paints an emulator on the cell grid and
  drives it from any `muxus_core::stream::TransportChannels`.

Tests: `cargo test -p muxus-term -p muxus-term-view` (357 + 19; the view's pointer, IME and settings tests run in GPUI's headless windows). Example:
`MUXUS_TERM_FONTS=<dir with ttf> cargo run -p muxus-term-view --example shell`.

## Emulator decisions

`par-term-emu-core-rust` (=0.80.0, default features off) is the engine. Muxus-specific and
xterm-specific behaviour sits in `muxus_term::filter::StreamFilter`, a streaming pre-parser that
splits the output at the sequences Muxus handles itself:

- **Default colors.** The engine stores SGR 37/40 exactly like the default colors (ANSI white and
  black). The filter rewrites explicit 37, 40, 38;5;7 and 48;5;0 to truecolor stand-ins and the
  emulator's pen uses a stand-in default foreground, so explicit white/black follow the palette
  and blank cells are told apart from written spaces (xterm's "has content": trailing written
  spaces survive selection and trimming, erased cells do not).
- **ED 2** scrolls the screen into the scrollback (`scrollOnEraseInDisplay`) instead of blanking it.
- **OSC 7/133/633** feed the ported `CwdTracker`/`CommandTracker`; **OSC 52** is write-only
  (queries answered empty on the same transport); **OSC 10/11/12** are answered from the theme and
  set overrides; **XTWINOPS** answers only 14/16/18 t like xterm with Muxus's `windowOptions`;
  **DECSET 9/1000/1002/1003/1006/1016** drive xterm's mouse protocols; **DECSCUSR** overrides the
  cursor preference; **OSC 7501** (program status) is parsed and passed on as
  `EmulatorEvent::ProgramStatus`, its support query answered inline. An OSC reply first flushes
  the engine's own pending answers, so a query fenced with DA1 is answered in order.
- Unicode 11 widths (the unicode11 addon), kitty keyboard flags from the engine.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Built-in schemes, exact colors, host overrides, font/background color overrides | `terminal/palette.ts` | done | All 15 schemes; `ResolvedPalette` adds the 256-color table and opaque selection. |
| Custom schemes: validation, copy, Muxus export, Windows Terminal and iTerm2 import, merge | `terminal/custom-schemes.ts` | done | The WeakMap conversion cache is not needed (conversion is pure). |
| Key encoding (legacy xterm, kitty keyboard protocol, KP_Enter fix, keydown/keypress flow) | `terminal/keyboard-input.ts`, xterm `Keyboard.ts`, `KittyKeyboard.ts`, `CoreBrowserTerminal._keyDown` | done | Ported from xterm.js; GPUI keystrokes are mapped to DOM `key`/`code`/`keyCode`. Win32 input mode (off in Muxus) is not ported. |
| Option+Left/Right word jumps on macOS (ESC b / ESC f; Kitty event-type encodings untouched) | `terminal/keyboard-input.ts` (c6d0a8f) | done | `normalize_terminal_keyboard_input`; the terminal-keyboard tests are ported. Not run on a Mac. |
| Program status (OSC 7501): parsing, records, clears by id, eviction, prompt/RIS/session end, summary, labels, support query | `terminal/program-status.ts` (783bb5f) | done | `muxus_term::program_status`; `program-status.test.ts` ported (parsing, records, summary, wiring). The query is answered only while `TerminalSettings::program_status` is on. The tab marks are the app's (`docs/parity/ui-shell.md`). |
| Box Drawing and Block Elements (U+2500–259F) drawn on the cell grid, clipped to the cell, edges on whole device pixels | `TerminalViewImpl` box-drawing face (1bb4f3c, e4312ab), xterm `customGlyphs` | done | `muxus-term-view/src/box_drawing.rs` draws them as the WebGL renderer does (xterm's definitions, MIT), with Muxus' pixel-rounding patch; nothing reaches the next row. Lines and blocks are quads; arcs and diagonals are antialiased paths. The shades ░▒▓ are 25/50/75 % fills instead of xterm's 2×2 dither. |
| Muxus key bindings (copy, paste, find, select all, clear, zoom, page scroll) | `keymap/bindings.ts`, `keymap/commands.ts` | done | The app's window keymap (`ui/shortcuts.rs` over `keymap/*`, rebindable) runs the `terminal.*` commands on the focused terminal (`run_ui_command`); Shift+PageUp/PageDown and Cmd+A on macOS are xterm's own keys in the view. The view's GPUI actions in the `Terminal` key context remain for embedding (the example binds them). |
| Terminal fit, cell metrics | `terminal/terminal-fit.ts`, xterm `DomRenderer`, `FitAddon` | done | Font height as Chromium measures it (rounded ascent + descent rounded up), DOM renderer device-pixel rounding, scrollbar width reserved, 90 ms resize settle except the first fit. |
| Font catalog and fallback stack | `terminal/font-catalog.ts`, `state/prefs.ts` (`terminalFontStack`) | done | Font enumeration (`listLocalFontFamilies`) is the app's. |
| Keyword highlighting: matching, limits, rule order, decorations | `terminal/keyword-matching.ts`, `keyword-highlighting.ts` | done | JavaScript regexes are translated to `fancy-regex` (ASCII `\d \w \b`, lookaround). Not highlighted on the alternate screen, as with xterm decorations. |
| Slow regex handling | `terminal/keyword-match-client.ts`, `keyword-match-worker.ts` | done (replaced) | No worker: the regex engine's backtrack limit pauses a catastrophic pattern for the session and notifies listeners (`subscribe_slow_keyword_patterns`), the behaviour the worker watchdog produced. |
| Highlight profiles (file format v1/v2, JSON rule editor) and built-in Nokia profiles | `highlight-profiles.ts`, `builtin-highlight-profiles.ts` | done | All built-in profile tests pass on the translated regexes. |
| Paste safety, line-number gutter, pacing helpers, bracketed paste data | `terminal/paste-safety.ts`, `paste-line-numbers.ts`, `paste-pacing.ts`, `shared/src/paste-pacing.ts` | done | Shared pacing types come from `muxus-core`. The paste preview dialog and paced sending are app/session work; the view emits `ConfirmPaste`. `host_paste_pacing` takes the host's prompt waiting. |
| Invisible and look-alike characters in pastes | `terminal/paste-safety.ts` (`flagPasteCharacters`, `replaceFlaggedCharacters`) | done | Positions in UTF-16 units as the TypeScript; a single line with one asks for the preview when `pasteWarnLookalikes` is on. `\p{L}` is `char::is_alphabetic`. |
| Per-host keys, implicit CR and local echo | `terminal/input-modes.ts`, `TerminalViewImpl.tsx` (`sendKeyboardInput`, `echoInput`, `convertEol`) | done | `TerminalSettings::input_modes`: keystrokes go out with the host's Backspace and Enter; local echo feeds `local_echo_text` to the emulator; implicit CR is the emulator's LNM (`Emulator::set_implicit_cr`, kept across a reset). Pastes use the host's Enter outside bracketed paste. |
| Typed input (saved text at the paste pace) | `terminal/typed-input.ts` | done | `typed_input::typed_input`; all of `typed-input.test.ts`. |
| Send text file (content checks) | `terminal/send-text-file.ts` | done | `send_text_file::{text_file_content, text_file_outcome}`; asking for the file and typing it are the app's. |
| Clipboard paste queue (ordering, limits, abort) and image paste targets | `terminal/clipboard-paste.ts` | done | Executor-agnostic futures. Image upload (SFTP/local) is the app's: the view emits `ClipboardWithoutText`. |
| OSC 52 write-only clipboard | `terminal/osc52-clipboard*.ts` | done | |
| Selection: character, word, line, column, select all, copy text, copy on select | xterm `SelectionService`, `terminal/selection-text.ts` | done | Written trailing spaces kept by construction (the TS repairs an xterm cache bug). Selection follows scrollback trimming. Auto-scroll while dragging is per mouse move, not timed. |
| Right-click (copy/paste, paste, menu), macOS word selection | `terminal/right-click.ts`, `TerminalViewImpl.onContextMenu`, xterm `rightClickSelect` | done | With the menu preference on macOS, a right click without a selection first selects the word under the pointer (`rightClickSelectsWord`), and the menu gets it. The menu itself is the app's (`ContextMenu` event). View test `a_right_click_on_macos_selects_the_word_for_the_menu`. Not run on a Mac. |
| Web links, file links, OSC 8 hyperlinks, activation gestures | `terminal/web-links.ts`, `file-links.ts`, `file-link-activation.ts`, WebLinksAddon | done | Path resolution (`resolve_terminal_file_path`) and opening the editor/browser are the app's (`OpenFile`, `OpenUrl`). OSC 8 links open with the same gesture (xterm's default confirm dialog is not reproduced). |
| Alt+click moves the cursor | xterm `altClickMovesCursor`, `MoveToCell.ts`, `SelectionService._handleMouseUp` | done | `muxus_term::move_to_cell` (left/right arrows along the shell line on the normal buffer; rows, then columns on the alternate screen; application cursor keys), sent as typed input when a quick (< 500 ms) Alt+click selects at most one character, the viewport is at the bottom and Alt is not the link gesture (`alt_click_moves_cursor`). Like xterm, a leftward move on one row of the alternate screen sends nothing. |
| Dropped paths quoting, drop intent and overlay text | `terminal/drop-paths.ts`, `file-drop.ts`, `TerminalFileDrop.tsx` | done | GPUI's external file drops on a terminal (`ui/sftp.rs` `render_session_body`, `features/sftp/terminal_drop.rs`): the dashed overlay with the intent's icon and text while files are over it, Shift at the drop inserts the shell-quoted local paths (local shells always), otherwise SSH uploads into the shell's directory. Differences: the overlay shows the intent without Shift, since GPUI's external drags report no modifiers until the drop; an SSH shell that reported no directory says "the home folder" instead of its path. DOM drag tracking (`isExternalFileDrag`, `trackInternalDrags`) has nothing to port: GPUI tells file drops from its own drags. |
| Send BREAK | `terminal/send-break.ts` | done | `supports_break`/`can_send_break`; `TerminalView::send_break` sends `TransportInput::Break`. The command and multi-exec rule live in the app. |
| Shell integration (failed-command marks, cwd from OSC 7 / 133 / 633) | `terminal/shell-integration.ts` | done | Marks on absolute lines, painted in the overview ruler's left lane. |
| SSH session summary | `terminal/ssh-session-summary.ts` | done | Uses `muxus_core::protocol::SshSessionSummary`. |
| Scrollback snapshots (serialize, ladder, dividers, legacy migration, body budget) | `terminal/scrollback-snapshots.ts`, SerializeAddon | done | Serializer ported from SerializeAddon, so stored data replays in both apps. Differences: no closing SGR to the live pen (not exposed by the engine); nothing is stored while a full-screen program runs (the engine exposes the active screen only). Fetch/PUT and timers are the app's. |
| Export as text / HTML, recent lines | `TerminalViewImpl` (`bufferText`, `recentLines`, `serializeAsHTML`), `terminal/buffer-text.ts` (v0.15 74aab72) | done | `Emulator::buffer_text` (`buffer::buffer_text`: soft-wrapped rows are joined back into one line, buffer-text.test.ts ported; Copy all output, Export as text and Compare with… use it), `recent_lines`, `serialize::serialize_as_html`. |
| Search (literal, case, whole word, regex; next/previous; incremental; counts) | SearchAddon | done | Whole word also finds later matches on a line where the first candidate is not a whole word (xterm misses those). |
| Search box UI and overview-ruler marks | `TerminalViewImpl` search Paper | done | The kit's widgets: Paper (elevation 8, divider border, 8px corners) at 11/13px from the top right, `TextField` 240px with the 17px search icon and the caption count, borderless small toggle buttons (Aa, W, .*; 12px, weight 550) and small icon buttons (KeyboardArrowUp/Down, Close) with the client's tooltips. Enter / Shift+Enter / Escape as in the client. Ruler marks as xterm draws them. |
| Rendering: colors, bold-as-bright, dim, inverse, invisible, blink, underline styles, strikethrough, overline, wide characters, contrast floor | xterm `DomRendererRowFactory` | done | Ligatures follow the font (JetBrains Mono `calt`), as in the DOM renderer. Double/dotted/dashed underlines are drawn with quads. |
| Cursor: block/bar/underline, blink (500 ms), inactive outline, DECSCUSR | xterm DOM renderer | done | |
| Scrollbar (overlay slider, hover/scroll reveal, fade, dragging, track presses) | xterm `Viewport`, VS Code `ScrollableElement`/`ScrollbarState`/`AbstractScrollbar` | done | `scrollbar.rs`: the slider is VS Code's (at least 20px, the floored share of the content, rounded position over a track as tall as the rows); a press on the slider drags it, a press on the track centres the slider on the pointer and drags on, positions round to whole rows (`Viewport._handleScroll`), Windows snaps back past 140px sideways. Foreground at 20% / 40% under the pointer / 50% while dragged. The kit's scrollbar (`muxus_ui::widgets::scrollbar`) is Chrome's (track presses page) and drives a `ScrollHandle`, so it is not used here. View test `the_scrollbar_drags_and_jumps_to_a_press_on_its_track`. |
| Mouse reporting (X10, VT200, drag, any; default, SGR, SGR-pixels), wheel to arrows on the alternate screen | xterm `MouseService`, `MouseStateService` | done | UTF-8 (1005) and urxvt (1015) encodings are ignored as in xterm. |
| Focus reporting, bracketed paste, application cursor | xterm | done | |
| IME composition | xterm `CompositionHelper` | done | Through GPUI's input handler: the composition is drawn over the cursor cell as xterm's composition view (white on black, the terminal font, at most to the right edge, its end shown when longer), the IME reads it back (`text_for_range`, caret at its end) and places its window at the cursor; committed text goes to the session as typed. View test `an_ime_composition_shows_at_the_cursor_and_is_sent_when_committed`. Not tried with a real IME (IBus, fcitx, macOS). |
| Kitty graphics, Sixel, iTerm2 images | ImageAddon | done | All three placed by the engine (tests), kitty verified against `kitty-graphics-dark.png`. Images keep their line in the scrollback and are scaled bilinearly like the addon's canvas. Storage limit as the addon's `storageLimit`: 64 MB while the tab is shown in the focused pane, 16 MB in the background (`TerminalView::set_active`, set by the app); the oldest images go first (`Emulator::set_image_storage_limit`, an engine patch to remove scrollback images). |
| Bell | xterm `onBell` | done | `TerminalViewEvent::Bell`; how to signal it is the app's. |
| Zoom (Ctrl+wheel, actions, percent) | `TerminalViewImpl.applyZoom` | done | |
| Live preference changes | `TerminalViewImpl` effects | done | Font, size, line height, theme, cursor, links, OSC 52, keyword rules and the scrollback size apply live; a smaller scrollback drops the oldest lines as xterm trims them (`Emulator::set_scrollback_limit`, an engine patch: `vendor/par-term-emu-core/PATCHES.md`). |
| Scrollback reflow on resize | xterm buffer reflow (`Buffer.resize`, `_reflowLarger`, `_reflowSmaller`, `BufferReflow.ts`) | done | xterm.js's resize, ported into the engine (`vendor/par-term-emu-core/src/grid/reflow.rs`, `PATCHES.md`; its `Buffer.test.ts` and `BufferReflow.test.ts` cases ported): the scrollback and the screen reflow as one buffer, so a line running from the scrollback onto the screen rewraps as one, a wider window joins what the old width wrapped and pulls the lines back down, a narrower one wraps them, and the cursor (and the saved cursor) follows its line; the logical line the cursor is on is left to the program (`reflowCursorLine` off) and cut at the new width. A taller screen pulls lines back from the scrollback while the cursor is on the last line; a shorter one drops the lines below the cursor first. A pending wrap ends at a resize, as xterm clamps its cursor. The viewport moves by xterm's `ydisp` rules: at the bottom it stays there, scrolled back it keeps its index (one line up per row the screen gains). Failed-command marks and the running command follow their lines and are disposed with a row a join empties, as xterm's markers; images keep their lines; the selection is cleared on a size change. The alternate screen resizes as xterm's alternate buffer (no reflow, top rows dropped to keep the cursor's row); its rows are cut to the width where xterm.js leaves them long. Emulator tests `the_screen_and_the_scrollback_reflow_as_one_buffer`, `a_taller_screen_brings_lines_back_from_the_scrollback`, `a_scrolled_back_viewport_keeps_its_index_through_a_rewrap_as_xterm_does`, `a_mark_on_a_row_that_joins_the_line_above_is_disposed`. |
| WebGL renderer preference | `@xterm/addon-webgl` | n/a | GPUI renders on the GPU; the preference has nothing to switch. |
| Terminal registry / handle | `terminal/terminal-registry.ts` | n/a | Replaced by the `TerminalView` entity API. |
| Session lifecycle (connect, reconnect, notices, type-ahead, transfer, logging) | `TerminalViewImpl` socket effect, `connection-recovery.ts` | done | In the app (`ui/terminal.rs`, `session/recovery.rs`, `muxus_session`): see `app.md` (session end, reconnect, auto-reconnect, tmux/screen reattach, scrollback kept across restarts, tab transfer) and `session.md` (transferable sessions, session logging, login sequences). The view offers `write_output`, `send_input`, `InputAfterExit` and `Exited`. |
| Connection diagnostics report text | `terminal/connection-diagnostics.ts` | done | `muxus_app::session::diagnostics` (`format_connection_diagnostics` with `terminal_notice`; D on an ended SSH or Telnet tab); connection-diagnostics-format.test.ts ported. |
| `palette-color-props` test | `tests/unit/client/palette-color-props.test.ts` | n/a | A lint over MUI component props. |

## Attribution

- xterm.js (MIT, © The xterm.js authors, SourceLair, Christopher Jeffrey): key encoding
  (`Keyboard.ts`, `KittyKeyboard.ts`), mouse protocols and encodings (`MouseStateService.ts`,
  `MouseService.ts`), contrast adjustment (`Color.ts`), DOM renderer color rules
  (`DomRendererRowFactory.ts`), cell metrics (`DomRenderer.ts`, `FitAddon.ts`), selection rules
  (`SelectionService.ts`), search (`addon-search`), serializer (`addon-serialize`), URL matching
  (`addon-web-links`), overview ruler geometry (`OverviewRulerRenderer.ts`).
- xterm.js `MoveToCell.ts` (MIT, © 2018 The xterm.js authors): Alt+click cursor moves.
- xterm.js `Buffer.ts` and `BufferReflow.ts` with their tests (MIT, © 2017-2019 The xterm.js
  authors): the buffer resize and reflow, in the vendored engine.
- VS Code `scrollbarState.ts`, `abstractScrollbar.ts` as bundled in xterm.js 6 (MIT, © Microsoft
  Corporation): the scrollbar's slider geometry and track presses.
- par-term-emu-core-rust (MIT, Paul Robello): the emulator engine, vendored and patched
  (`vendor/par-term-emu-core/PATCHES.md`).
