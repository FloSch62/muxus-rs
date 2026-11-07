# Attribution

Muxus-rs is MIT licensed. It bundles or adapts the third-party work below.
Crates pulled in through `Cargo.toml` keep their own license files and are
not repeated here; this file lists what is copied into the repository or
derived from someone else's code or design.

## Fonts (`assets/fonts`)

| Asset | Source | License |
|---|---|---|
| `Inter-{400,450,500,550,600,650,700,800}.ttf` | Inter 4.x by Rasmus Andersson, from `@fontsource-variable/inter` 5.3.0. Static instances cut from the variable font and its script subsets merged, by `hack/build_fonts.py`. | SIL Open Font License 1.1 (`assets/fonts/Inter-LICENSE.txt`) |
| `JetBrainsMono-{400,500,700}.ttf`, `JetBrainsMono-{400,700}-Italic.ttf` | JetBrains Mono by JetBrains, from `@fontsource/jetbrains-mono` 5.3.0, script subsets merged by `hack/build_fonts.py`. | SIL Open Font License 1.1 (`assets/fonts/JetBrainsMono-LICENSE.txt`) |
| `PureNerdFont.ttf` | `@azurity/pure-nerd-font` 3.0.5 (the Nerd Fonts icon glyphs only), converted from WOFF2. | MIT; the icon sets inside keep the licenses listed in the Nerd Fonts license audit (`assets/fonts/PureNerdFont-LICENSE.txt`) |

## Icons (`assets/icons`)

| Asset | Source | License |
|---|---|---|
| `*.svg` except the files named below | Material icons as packaged by `@mui/icons-material` 9.5.0, extracted by `hack/extract_icons.mjs`. | MIT (`assets/icons/LICENSE-material-icons.txt`); the Material Design icons themselves are Apache License 2.0 by Google |
| `muxus.svg` | The Muxus logo from the Muxus client (`client/public/muxus.svg`). | MIT, same author |
| `TabPin.svg` | The pin glyph drawn inline in Muxus' `TabStrip.tsx`. | MIT, same author |
| `WindowMinimize.svg`, `WindowMaximize.svg`, `WindowRestore.svg`, `WindowClose.svg` | Drawn for this project after the window controls the Electron build shows on Linux. | MIT |
| `CodiconError.svg`, `CodiconWarning.svg`, `CodiconArrowDown.svg`, `CodiconArrowUp.svg`, `CodiconClose.svg` | Drawn for this project after the glyphs Monaco's problem widget shows (no codicon artwork copied). | MIT |

## Design and code

| What | Source | License |
|---|---|---|
| Theme tokens and component geometry in `crates/muxus-ui` (palette augmentation, action colors, typography scale, shadows, Button/IconButton/TextField/Menu/Tooltip/Dialog/Tabs/Chip/Alert/Checkbox/Radio/Switch measurements) | MUI (`@mui/material`) defaults as configured by Muxus' `client/src/theme.ts`; values re-derived from MUI's documented defaults and measured in the running client. | MIT (MUI) |
| `crates/muxus-ui/src/color.rs` (`lighten`, `darken`, `alpha`, contrast ratio) | Same formulas as MUI's `colorManipulator`. | MIT (MUI) |
| Code editor colors in `crates/muxus-ui/src/syntax.rs` and `crates/muxus-app/src/util/code_editor.rs` (the `vs`/`vs-dark` token colors, bracket pair colors by depth, fold controls on hover) | Monaco Editor's built-in themes and defaults, as the Muxus client shows them; values only, no source copied. | MIT (Monaco Editor) |
| The remote editor's language features in `crates/muxus-app/src/features/sftp/editor/` (`links.rs`: the link state machine and end rules; `occurrences.rs`: the default word definition and whole-word match rule; `goto_line.rs`: the go-to-line parsing and messages; `overlays.rs`: the current-line box, the overview ruler's lanes and marks, the minimap width formula) | Ported from Monaco Editor 0.57 (`linkComputer.ts`, `wordHelper.ts`, `gotoLineQuickAccess.ts`, `currentLineHighlight.ts`, `decorationsOverviewRuler.ts`, `editorOptions.ts`). Copyright (c) Microsoft Corporation. | MIT (Monaco Editor) |
| JSON problem markers and Format Document in `crates/muxus-app/src/features/sftp/editor/json.rs` | Ported from `jsonc-parser` (`scanner.ts`, `format.ts`) and `vscode-json-languageservice` (`jsonParser.ts`'s syntax errors, `jsonValidation.ts`'s severities), as bundled by Monaco Editor 0.57. Copyright (c) Microsoft Corporation. | MIT |
| The remote editor's minimap, marker navigation and problem widget in `crates/muxus-app/src/features/sftp/editor/` (`minimap.rs`: the minimap layout, line sampling, slider mapping, block drawing and decorations; `markers.rs`: the marker order and navigation; `problem_widget.rs`: the peek view's zone, head and message layout) | Ported from Monaco Editor 0.57 (`editorOptions.ts`'s `_computeMinimapLayout`, `minimap.ts`, `minimapCharRenderer.ts`, `markerNavigationService.ts`, `gotoError.ts`, `gotoErrorWidget.ts`, `peekView.ts`, `zoneWidget.ts`). Copyright (c) Microsoft Corporation. | MIT (Monaco Editor) |
| Syntax error messages in `crates/muxus-app/src/features/sftp/editor/syntax_problems.rs` | The wording and codes of TypeScript's diagnostics (`';' expected.`, `Unterminated string literal.`…) and of `vscode-css-languageservice`'s parse errors (`cssErrors.ts`); messages only, no source copied. | Apache-2.0 (TypeScript), MIT (vscode-css-languageservice) |
| CSS, SCSS and Less Format Document in `crates/muxus-app/src/features/sftp/editor/css_format.rs` and `beautify.rs` | Ported from js-beautify 1.15 (`css/beautifier.js`, `core/output.js`, `core/inputscanner.js`) as `vscode-css-languageservice` bundles it in Monaco Editor 0.57, with `cssFormatter.ts`'s options. Copyright (c) 2007-2018 Einar Lielmanis, Liam Newman, and contributors. | MIT (js-beautify) |
| Box Drawing and Block Elements shapes in `crates/muxus-term-view/src/box_drawing.rs` (the line paths, light and heavy weights, double-line gaps, arcs and eighth-cell blocks) | Adapted from xterm.js's `CustomGlyphDefinitions.ts` and `CustomGlyphRasterizer.ts` (`@xterm/addon-webgl`), with Muxus' patch that rounds block edges to whole device pixels. Copyright (c) 2021 The xterm.js authors. | MIT (xterm.js) |
| Alt+click cursor moves in `crates/muxus-term/src/move_to_cell.rs` | Adapted from xterm.js's `MoveToCell.ts`. Copyright (c) 2018 The xterm.js authors. | MIT (xterm.js) |
| The terminal scrollbar's slider geometry and track presses in `crates/muxus-term-view/src/scrollbar.rs` | Adapted from VS Code's `scrollbarState.ts` and `abstractScrollbar.ts` as bundled in xterm.js 6. Copyright (c) Microsoft Corporation. | MIT (VS Code, xterm.js) |
| Serial port listing in `crates/muxus-serial/src/ports.rs` and `ports/windows.rs` (the udev parsing, the Windows SetupAPI walk and USB container serial number lookup, the instance-ID serial number parser, the macOS dial-in and `locationId` rules) | Ported from @serialport/bindings-cpp 13 (`linux-list.js`, `serialport_win.cpp`, `win32.js`, `win32-sn-parser.js`, `darwin_list.cpp`). Copyright 2010 Christopher Williams. | MIT (node-serialport) |
| Machine readings in `crates/muxus-os/src/machine` (CPU times, Windows version, uptime, memory, drive space) | The system calls and unit conversions libuv makes for Node's `os` module (`uv_cpu_info`, `uv_os_uname`, `uv_uptime`, `uv_fs_statfs`); reimplemented, no source copied. Copyright Joyent, Inc. and other Node contributors. | MIT (libuv) |
| Taskbar jump list in `crates/muxus-os/src/jump_list` | Follows Electron's `shell/browser/ui/win/jump_list.cc` (its calls, their order and its result codes); reimplemented, no source copied. Copyright (c) Electron contributors. | MIT (Electron) |
| GPUI and gpui-component (`gpui-kit`, `gpui-pre`) | Used as dependencies; the unstyled text input of `gpui-base` backs `TextField`. No source is copied. | Apache License 2.0 |

## Vendored crates (`vendor/`)

| Crate | Source | License |
|---|---|---|
| `vendor/par-term-emu-core` | [par-term-emu-core-rust](https://github.com/paulrobello/par-term-emu-core-rust) 0.60.0 by Paul Robello, with the scrolling, scrollback and printing paths reworked for throughput and the parts Muxus does not use removed (see `docs/parity/terminal-perf.md`), and a running terminal's scrollback size, scrollback reflow and image removal added (see `vendor/par-term-emu-core/PATCHES.md`). Patched in through `[patch.crates-io]` in the root `Cargo.toml`. | MIT (`vendor/par-term-emu-core/LICENSE`) |
| `vendor/russh` | [russh](https://github.com/warp-tech/russh) 0.64.1 by Pierre-Étienne Meunier and the russh contributors, unchanged except for the RFC 4335 `break` channel request (client request, server handler hook); see `vendor/russh/PATCHES.md`. Patched in through `[patch.crates-io]` in the root `Cargo.toml`. | Apache License 2.0 (`vendor/russh/LICENSE`) |
