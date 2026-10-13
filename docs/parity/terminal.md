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

Tests: `cargo test -p muxus-term -p muxus-term-view` (278 + 1). Example:
`MUXUS_TERM_FONTS=<dir with ttf> cargo run -p muxus-term-view --example shell`.

## Emulator decisions

`par-term-emu-core-rust` (=0.60.0, default features off) is the engine. Muxus-specific and
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
  cursor preference.
- Unicode 11 widths (the unicode11 addon), kitty keyboard flags from the engine.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Built-in schemes, exact colors, host overrides, font/background color overrides | `terminal/palette.ts` | done | All 15 schemes; `ResolvedPalette` adds the 256-color table and opaque selection. |
| Custom schemes: validation, copy, Muxus export, Windows Terminal and iTerm2 import, merge | `terminal/custom-schemes.ts` | done | The WeakMap conversion cache is not needed (conversion is pure). |
| Key encoding (legacy xterm, kitty keyboard protocol, KP_Enter fix, keydown/keypress flow) | `terminal/keyboard-input.ts`, xterm `Keyboard.ts`, `KittyKeyboard.ts`, `CoreBrowserTerminal._keyDown` | done | Ported from xterm.js; GPUI keystrokes are mapped to DOM `key`/`code`/`keyCode`. Win32 input mode (off in Muxus) is not ported. |
| Muxus key bindings (copy, paste, find, clear, zoom, page scroll) | `keymap/bindings.ts` | partial | Exposed as GPUI actions in the `Terminal` key context; the app binds them from the keymap (the example binds Ctrl+Shift+C/V/F/K and Ctrl+=/-/0). |
| Terminal fit, cell metrics | `terminal/terminal-fit.ts`, xterm `DomRenderer`, `FitAddon` | done | Font height as Chromium measures it (rounded ascent + descent rounded up), DOM renderer device-pixel rounding, scrollbar width reserved, 90 ms resize settle except the first fit. |
| Font catalog and fallback stack | `terminal/font-catalog.ts`, `state/prefs.ts` (`terminalFontStack`) | done | Font enumeration (`listLocalFontFamilies`) is the app's. |
| Keyword highlighting: matching, limits, rule order, decorations | `terminal/keyword-matching.ts`, `keyword-highlighting.ts` | done | JavaScript regexes are translated to `fancy-regex` (ASCII `\d \w \b`, lookaround). Not highlighted on the alternate screen, as with xterm decorations. |
| Slow regex handling | `terminal/keyword-match-client.ts`, `keyword-match-worker.ts` | done (replaced) | No worker: the regex engine's backtrack limit pauses a catastrophic pattern for the session and notifies listeners (`subscribe_slow_keyword_patterns`), the behaviour the worker watchdog produced. |
| Highlight profiles (file format v1/v2, JSON rule editor) and built-in Nokia profiles | `highlight-profiles.ts`, `builtin-highlight-profiles.ts` | done | All built-in profile tests pass on the translated regexes. |
| Paste safety, line-number gutter, pacing helpers, bracketed paste data | `terminal/paste-safety.ts`, `paste-line-numbers.ts`, `paste-pacing.ts`, `shared/src/paste-pacing.ts` | done | Shared pacing types come from `muxus-core`. The paste preview dialog and paced sending are app/session work; the view emits `ConfirmPaste`. |
| Clipboard paste queue (ordering, limits, abort) and image paste targets | `terminal/clipboard-paste.ts` | done | Executor-agnostic futures. Image upload (SFTP/local) is the app's: the view emits `ClipboardWithoutText`. |
| OSC 52 write-only clipboard | `terminal/osc52-clipboard*.ts` | done | |
| Selection: character, word, line, column, select all, copy text, copy on select | xterm `SelectionService`, `terminal/selection-text.ts` | done | Written trailing spaces kept by construction (the TS repairs an xterm cache bug). Selection follows scrollback trimming. Auto-scroll while dragging is per mouse move, not timed. |
| Right-click (copy/paste, paste, menu) | `terminal/right-click.ts`, `TerminalViewImpl.onContextMenu` | partial | Logic done; the context menu itself is the app's (`ContextMenu` event). macOS right-click word selection not wired. |
| Web links, file links, OSC 8 hyperlinks, activation gestures | `terminal/web-links.ts`, `file-links.ts`, `file-link-activation.ts`, WebLinksAddon | done | Path resolution (`resolve_terminal_file_path`) and opening the editor/browser are the app's (`OpenFile`, `OpenUrl`). OSC 8 links open with the same gesture (xterm's default confirm dialog is not reproduced). |
| Alt+click moves the cursor | xterm `altClickMovesCursor` | missing | `alt_click_moves_cursor` decides when; the arrow-key sequence is not sent yet. |
| Dropped paths quoting, drop intent and overlay text | `terminal/drop-paths.ts`, `file-drop.ts` | partial | Pure logic done; the drag-and-drop wiring and overlay are app/view work not done. DOM drag tracking (`isExternalFileDrag`, `trackInternalDrags`) has no GPUI equivalent to port. |
| Send BREAK | `terminal/send-break.ts` | done | `supports_break`/`can_send_break`; `TerminalView::send_break` sends `TransportInput::Break`. The command and multi-exec rule live in the app. |
| Shell integration (failed-command marks, cwd from OSC 7 / 133 / 633) | `terminal/shell-integration.ts` | done | Marks on absolute lines, painted in the overview ruler's left lane. |
| SSH session summary | `terminal/ssh-session-summary.ts` | done | Uses `muxus_core::protocol::SshSessionSummary`. |
| Scrollback snapshots (serialize, ladder, dividers, legacy migration, body budget) | `terminal/scrollback-snapshots.ts`, SerializeAddon | done | Serializer ported from SerializeAddon, so stored data replays in both apps. Differences: no closing SGR to the live pen (not exposed by the engine); nothing is stored while a full-screen program runs (the engine exposes the active screen only). Fetch/PUT and timers are the app's. |
| Export as text / HTML, recent lines | `TerminalViewImpl` (`bufferText`, `recentLines`, `serializeAsHTML`) | done | `Emulator::buffer_text`, `recent_lines`, `serialize::serialize_as_html`. |
| Search (literal, case, whole word, regex; next/previous; incremental; counts) | SearchAddon | done | Whole word also finds later matches on a line where the first candidate is not a whole word (xterm misses those). |
| Search box UI and overview-ruler marks | `TerminalViewImpl` search Paper | partial | Paper, field, count, toggles and buttons in the Muxus palette with Lucide icons (MUI icons and exact MUI metrics pending the shared UI kit); ruler marks as xterm draws them. |
| Rendering: colors, bold-as-bright, dim, inverse, invisible, blink, underline styles, strikethrough, overline, wide characters, contrast floor | xterm `DomRendererRowFactory` | done | Ligatures follow the font (JetBrains Mono `calt`), as in the DOM renderer. Double/dotted/dashed underlines are drawn with quads. |
| Cursor: block/bar/underline, blink (500 ms), inactive outline, DECSCUSR | xterm DOM renderer | done | |
| Scrollbar (overlay slider, hover/scroll reveal, fade) | xterm `Viewport`/scrollable | partial | Slider painted and faded; dragging the slider and clicking the track are not implemented. |
| Mouse reporting (X10, VT200, drag, any; default, SGR, SGR-pixels), wheel to arrows on the alternate screen | xterm `MouseService`, `MouseStateService` | done | UTF-8 (1005) and urxvt (1015) encodings are ignored as in xterm. |
| Focus reporting, bracketed paste, application cursor | xterm | done | |
| IME composition | xterm `CompositionHelper` | partial | Committed text and the preedit at the cursor through GPUI's input handler; not tested with a real IME. |
| Kitty graphics, Sixel, iTerm2 images | ImageAddon | partial | All three placed by the engine (tests), kitty verified against `kitty-graphics-dark.png`. Images keep their line in the scrollback and are scaled bilinearly like the addon's canvas. Storage limits per active/background tab are not wired. |
| Bell | xterm `onBell` | done | `TerminalViewEvent::Bell`; how to signal it is the app's. |
| Zoom (Ctrl+wheel, actions, percent) | `TerminalViewImpl.applyZoom` | done | |
| Live preference changes | `TerminalViewImpl` effects | partial | Font, size, line height, theme, cursor, links, OSC 52 and keyword rules apply live; scrollback size applies to new terminals only (the engine cannot resize its scrollback). |
| Scrollback reflow on resize | xterm buffer reflow | partial | The engine reflows the screen; scrollback lines keep their width. |
| WebGL renderer preference | `@xterm/addon-webgl` | n/a | GPUI renders on the GPU; the preference has nothing to switch. |
| Terminal registry / handle | `terminal/terminal-registry.ts` | n/a | Replaced by the `TerminalView` entity API. |
| Session lifecycle (connect, reconnect, notices, type-ahead, transfer, logging) | `TerminalViewImpl` socket effect | missing | Session work for `muxus-session`/the app; the view offers `write_output`, `send_input`, `InputAfterExit` and `Exited`. |
| Connection diagnostics report text | `terminal/connection-diagnostics.ts` | missing | Depends on `terminalNotice` from `connection-recovery.ts`. |
| `palette-color-props` test | `tests/unit/client/palette-color-props.test.ts` | n/a | A lint over MUI component props. |

## Attribution

- xterm.js (MIT, © The xterm.js authors, SourceLair, Christopher Jeffrey): key encoding
  (`Keyboard.ts`, `KittyKeyboard.ts`), mouse protocols and encodings (`MouseStateService.ts`,
  `MouseService.ts`), contrast adjustment (`Color.ts`), DOM renderer color rules
  (`DomRendererRowFactory.ts`), cell metrics (`DomRenderer.ts`, `FitAddon.ts`), selection rules
  (`SelectionService.ts`), search (`addon-search`), serializer (`addon-serialize`), URL matching
  (`addon-web-links`), overview ruler geometry (`OverviewRulerRenderer.ts`).
- par-term-emu-core-rust (MIT, Paul Robello): the emulator engine (dependency, not copied).
