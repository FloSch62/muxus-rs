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

## Scrollback reflow

xterm.js rewraps its whole buffer when the width changes; the engine rewrapped the screen only
and kept every stored line at the width it scrolled off at (clipped by a narrower window).

| File | Change |
|---|---|
| `src/grid/mod.rs` | `Grid::set_reflow_scrollback` (off by default), `ScrollbackReflow` and `Grid::take_scrollback_reflow`: for every stored line before a rewrap, the line that holds its first cell after it. |
| `src/grid/scroll.rs` | On a width change `Grid::resize` first rewraps the scrollback (`reflow_scrollback_rows`, `Rewrap`): each logical line is cut again at the new width (a wide character that does not fit moves on and leaves the last cell blank, wrapped rows full width, a line's last row compact when the scrollback is), the lines above keep their absolute numbers, zones follow their lines, lines past the capacity are evicted. A logical line that continues onto the screen is left as it is. |
| `src/terminal/mod.rs` | `Terminal::set_reflow_scrollback`, `Terminal::take_scrollback_reflow` for the primary screen. |
| `src/grid/tests.rs` | Wider joins, narrower wraps, wide characters, capacity, the line continuing onto the screen, the switch off. |

The emulator turns it on and moves its marks with the map
(`muxus_term::terminal::Emulator::resize`). A 10 000-line scrollback of 156-character lines
rewraps in 6–45 ms (release).

## Updating

Copy the new release over this folder, keep `LICENSE` and this file, re-apply the throughput
changes (`docs/parity/terminal-perf.md`) and the patches above (search for `Muxus patch`), and
update the version here and in `ATTRIBUTION.md`.
