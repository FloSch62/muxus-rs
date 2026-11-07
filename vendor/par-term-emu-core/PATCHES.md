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

## xterm.js buffer resize

xterm.js keeps the scrollback and the screen in one list of lines and resizes that list as a
whole (`Buffer.resize`, `_reflowLarger`, `_reflowSmaller`, `BufferReflow.ts`). The engine
reflowed the screen on its own: content stayed top-aligned, the cursor kept its row instead of
its line, nothing came back from the scrollback, and stored lines kept the width they scrolled
off at. `src/grid/reflow.rs` ports xterm.js 6.1's code (MIT, Copyright (c) 2017-2019 The
xterm.js authors) statement by statement, edge cases included.

| File | Change |
|---|---|
| `src/grid/reflow.rs` | `XtermBuffer` (the lines, `ybase`, `ydisp`, the cursor and the saved cursor) with xterm.js's `resize`, `_reflowLarger`/`reflowLargerGetLinesToRemove`/`_reflowLargerAdjustViewport`, `_reflowSmaller`/`reflowSmallerGetNewLineLengths` and `getWrappedLineTrimmedLength`, `reflowCursorLine` off. A taller screen pulls lines back from the scrollback while the cursor is on the last line; a shorter one drops the lines below the cursor first, then pushes the top lines into the scrollback; a wider one joins every wrapped line but the cursor's, a narrower one wraps them (a wide character that does not fit moves on). `Grid::resize_xterm` builds the buffer from the scrollback rows (as stored, short rows read as null-padded) and the screen rows, resizes it, and stores it back: a row that continues on the next keeps the full width, a line's last row is compact when the scrollback is, the scrollback keeps its capacity. `BufferPositions` carries the cursor (its column the width while a wrap is pending), the saved cursor and the viewport's top line; `BufferReflow` (`Grid::take_buffer_reflow`) tells where every line went (`None` for a row emptied by a join, dropped or trimmed) and how many lines were trimmed, so callers keep line numbers. Zones follow their lines. Content: a cell other than a blank or erased one (a space with the default foreground and no marks), as the xterm.js null cell has none. |
| `src/terminal/mod.rs` | `Terminal::set_xterm_resize` (off by default, in `HostConfig` so it survives RIS), `Terminal::resize_with_viewport` (the host's viewport in, xterm's `ydisp` out) and `Terminal::take_buffer_reflow`. With the switch on, `resize` resizes the primary screen as xterm's normal buffer (with reflow) and the alternate screen as its alternate buffer (no reflow, rows dropped above the cursor when it is at the bottom; its rows are cut to the width, which xterm.js leaves long). Each screen's cursor follows its line, the saved cursor of the active screen too, a pending wrap ends (xterm clamps its cursor into the last column), and the images on the primary screen keep their lines. |
| `src/graphics/mod.rs` | `GraphicsStore::move_placements` for the images. |
| `src/grid/scroll.rs` | The previous patch's scrollback-only rewrap (`reflow_scrollback_rows`) is gone; `Grid::resize` is upstream's again. |
| `src/grid/reflow/tests.rs` | xterm.js's `Buffer.test.ts` resize and reflow cases and `BufferReflow.test.ts`, ported (the ConPTY and `reflowCursorLine` cases left out), plus the grid: a line running from the scrollback onto the screen, the cursor following its line, lines coming back, the line map, compact rows, zones. |
| `src/terminal/tests/xterm_resize.rs` | The pending wrap, the saved cursor, both screens, the viewport, images, RIS, and a property test: any stream with any resizes keeps the grid's invariants. |

The emulator turns it on and moves its marks with the map
(`muxus_term::terminal::Emulator::resize`). Resizing a filled 200×50 emulator with 10 000 lines
of scrollback to 150 columns, or back, takes about 1 ms with plain log lines and 7–14 ms with
long colored lines that wrap (release, `throughput --resize`).

## Updating

Copy the new release over this folder, keep `LICENSE` and this file, re-apply the throughput
changes (`docs/parity/terminal-perf.md`) and the patches above (search for `Muxus patch`), and
update the version here and in `ATTRIBUTION.md`.
