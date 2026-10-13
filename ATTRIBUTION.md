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

## Design and code

| What | Source | License |
|---|---|---|
| Theme tokens and component geometry in `crates/muxus-ui` (palette augmentation, action colors, typography scale, shadows, Button/IconButton/TextField/Menu/Tooltip/Dialog/Tabs/Chip/Alert/Checkbox/Radio/Switch measurements) | MUI (`@mui/material`) defaults as configured by Muxus' `client/src/theme.ts`; values re-derived from MUI's documented defaults and measured in the running client. | MIT (MUI) |
| `crates/muxus-ui/src/color.rs` (`lighten`, `darken`, `alpha`, contrast ratio) | Same formulas as MUI's `colorManipulator`. | MIT (MUI) |
| Code editor colors in `crates/muxus-ui/src/syntax.rs` and `crates/muxus-app/src/util/code_editor.rs` (the `vs`/`vs-dark` token colors, bracket pair colors by depth, fold controls on hover) | Monaco Editor's built-in themes and defaults, as the Muxus client shows them; values only, no source copied. | MIT (Monaco Editor) |
| GPUI and gpui-component (`gpui-kit`, `gpui-pre`) | Used as dependencies; the unstyled text input of `gpui-base` backs `TextField`. No source is copied. | Apache License 2.0 |

## Vendored crates (`vendor/`)

| Crate | Source | License |
|---|---|---|
| `vendor/par-term-emu-core` | [par-term-emu-core-rust](https://github.com/paulrobello/par-term-emu-core-rust) 0.60.0 by Paul Robello, with the scrolling, scrollback and printing paths reworked for throughput and the parts Muxus does not use removed; see `docs/parity/terminal-perf.md`. Patched in through `[patch.crates-io]` in the root `Cargo.toml`. | MIT (`vendor/par-term-emu-core/LICENSE`) |
