# Muxus patches to par-term-emu-core

This is [par-term-emu-core-rust](https://github.com/paulrobello/par-term-emu-core-rust) 0.60.0
(MIT, `LICENSE`), patched into the workspace through `[patch.crates-io]` in the root
`Cargo.toml`. Two kinds of changes sit on top of the published crate:

- **Throughput.** The scrolling, scrollback and printing paths were reworked, and parts Muxus
  does not use removed. `docs/parity/terminal-perf.md` lists every change, the findings behind
  it and the tests that keep the behaviour.
- **Features Muxus needs from a running terminal**, below. Each change carries a `Muxus patch`
  comment.

## Scrollback size of a running terminal

xterm.js applies `options.scrollback` to a live terminal; the engine only took the size when
the terminal was created.

| File | Change |
|---|---|
| `src/grid/mod.rs` | `Grid::set_max_scrollback`: a smaller size drops the oldest lines the way a scroll at capacity evicts them (`total_lines_scrolled` kept, zones below the new floor evicted, the scroll log invalidated, every row marked). |
| `src/terminal/mod.rs` | `Terminal::set_max_scrollback` for the primary screen (the alternate screen keeps none). |
| `src/grid/tests.rs` | `a_smaller_scrollback_drops_the_oldest_lines_as_trimmed`. |

## Image storage limit

The image addon keeps at most `storageLimit` MB of image data per terminal and drops the oldest
images first. The engine's store could remove a placement on screen but not one in the
scrollback.

| File | Change |
|---|---|
| `src/graphics/mod.rs` | `GraphicsStore::remove_graphic_everywhere` removes an image from the placements and the scrollback. The limit itself is `muxus_term::terminal::Emulator::set_image_storage_limit`. |

## Updating

Copy the new release over this folder, keep `LICENSE` and this file, re-apply the throughput
changes (`docs/parity/terminal-perf.md`) and the patches above (search for `Muxus patch`), and
update the version here and in `ATTRIBUTION.md`.
