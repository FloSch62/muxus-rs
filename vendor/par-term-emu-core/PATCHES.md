# Muxus patches to par-term-emu-core

This is `par-term-emu-core` 0.79.0, the engine member of
[par-term-emu-core-rust](https://github.com/paulrobello/par-term-emu-core-rust) 0.79.0 (MIT,
`LICENSE`), patched into the workspace through `[patch.crates-io]` in the root `Cargo.toml`.
Three kinds of changes sit on top of the published crate:

- **Throughput.** The scrolling, scrollback and printing paths were reworked.
  `docs/parity/terminal-perf.md` lists every change, the findings behind it and the tests that
  keep the behaviour.
- **Features Muxus needs from a running terminal**, below. Each change carries a `Muxus patch`
  comment.
- **Left out.** The PTY backend (`src/pty_session`, the `portable-pty` dependency; Muxus runs
  its own processes, and the `pty_session` feature now stops the build) and the design notes
  (`DESIGN.md`).

The git history keeps them apart: the commit "Vendor par-term-emu-core 0.79.0 as published"
holds the crate as released (`Cargo.toml.orig` as `Cargo.toml`), and each commit after it that
starts with "Engine:" is one patch.

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

Upstream's store-wide placement budget (0.79.0, `enforce_placement_budget`, 256 MiB) is a
different rule: it drops scrollback images before visible ones instead of the oldest image,
counts bytes per shared buffer, includes virtual placements and runs only when an image is
added, where the addon also evicts when the limit shrinks. Muxus's limits (64 MB, 16 MB in the
background) are lower, so the engine's budget never triggers first.

## xterm.js buffer resize

xterm.js keeps the scrollback and the screen in one list of lines and resizes that list as a
whole (`Buffer.resize`, `_reflowLarger`, `_reflowSmaller`, `BufferReflow.ts`). The engine
reflows the screen on its own (still so in 0.79.0): content stays top-aligned, the cursor keeps
its row instead of its line, nothing comes back from the scrollback, and stored lines keep the
width they scrolled off at. `src/grid/reflow.rs` ports xterm.js 6.1's code (MIT, Copyright (c)
2017-2019 The xterm.js authors) statement by statement, edge cases included.

| File | Change |
|---|---|
| `src/grid/reflow.rs` | `XtermBuffer` (the lines, `ybase`, `ydisp`, the cursor and the saved cursor) with xterm.js's `resize`, `_reflowLarger`/`reflowLargerGetLinesToRemove`/`_reflowLargerAdjustViewport`, `_reflowSmaller`/`reflowSmallerGetNewLineLengths` and `getWrappedLineTrimmedLength`, `reflowCursorLine` off. A taller screen pulls lines back from the scrollback while the cursor is on the last line; a shorter one drops the lines below the cursor first, then pushes the top lines into the scrollback; a wider one joins every wrapped line but the cursor's, a narrower one wraps them (a wide character that does not fit moves on). `Grid::resize_xterm` builds the buffer from the scrollback rows (as stored, short rows read as null-padded) and the screen rows, resizes it, and stores it back: a row that continues on the next keeps the full width, a line's last row is compact when the scrollback is, the scrollback keeps its capacity. `BufferPositions` carries the cursor (its column the width while a wrap is pending), the saved cursor and the viewport's top line; `BufferReflow` (`Grid::take_buffer_reflow`) tells where every line went (`None` for a row emptied by a join, dropped or trimmed) and how many lines were trimmed, so callers keep line numbers. Zones follow their lines. Content: a cell other than a blank or erased one (a space with the default foreground and no marks), as the xterm.js null cell has none. |
| `src/terminal/state.rs` | The `xterm_resize` switch in `HostConfig`, so it survives RIS. |
| `src/terminal/mod.rs` | `Terminal::set_xterm_resize` (off by default), `Terminal::resize_with_viewport` (the host's viewport in, xterm's `ydisp` out) and `Terminal::take_buffer_reflow`. With the switch on, `resize` resizes the primary screen as xterm's normal buffer (with reflow) and the alternate screen as its alternate buffer (no reflow, rows dropped above the cursor when it is at the bottom; its rows are cut to the width, which xterm.js leaves long). Each screen's cursor follows its line, the saved cursor of the active screen too, a pending wrap ends (xterm clamps its cursor into the last column), and the images on the primary screen keep their lines. A resize to the size the screens have changes nothing, as in xterm.js. |
| `src/graphics/mod.rs` | `GraphicsStore::move_placements` for the images. |
| `src/grid/reflow/tests.rs` | xterm.js's `Buffer.test.ts` resize and reflow cases and `BufferReflow.test.ts`, ported (the ConPTY and `reflowCursorLine` cases left out), plus the grid: a line running from the scrollback onto the screen, the cursor following its line, lines coming back, the line map, compact rows, zones. |
| `src/terminal/tests/xterm_resize.rs` | The pending wrap, the saved cursor, both screens, the viewport, images, RIS, a resize to the same size, and a property test: any stream with any resizes keeps the grid's invariants. |

The emulator turns it on and moves its marks with the map
(`muxus_term::terminal::Emulator::resize`). Resizing a filled 200×50 emulator with 10 000 lines
of scrollback to 150 columns, or back, takes about 1 ms with plain log lines and 7–14 ms with
long colored lines that wrap (release, `throughput --resize`).

## Cell size

A cell was 40 bytes: 33 bytes of fields padded to the 8-byte alignment its combining-mark
pointer sets, the width byte alone past the flags. The cached width now rides in the byte
`CellFlags` already padded to, so a cell is 32 bytes: a fifth less memory for every screen and
scrollback cell, and less to copy on every write and scroll.

| File | Change |
|---|---|
| `src/cell.rs` | `CellFlags` holds the width (1 by default, so the pen's flags and `Cell::default()` are unchanged; skipped by serde, whose `CellSerde` still carries `width`, so the wire format is the same); the `Cell::width` field is gone: `Cell::width()` reads it, `Cell::set_width` and `CellFlags::with_width` set it. `cell_fits_in_32_bytes`. |
| `src/terminal/write.rs`, `src/grid/mod.rs`, `src/terminal/sequences/csi/window.rs`, `src/grid/scroll.rs` | Cells are built with `flags.with_width(…)`; a spacer copied from a wide cell's flags sets its own width of 1, and DECFRA's fill cell keeps its width when it takes the pen's flags. |

`docs/parity/terminal-perf.md` has the measurements (19–20% less resident memory for a filled
scrollback, output as fast or faster).

## Not carried over

The 0.60.0 copy also removed the par-mux pane-roster notifications (`%agent-state-changed`,
`%agent-released`, `%agent-telemetry-changed`) from the tmux control-mode parser. Muxus never
turns tmux control mode on, so they never ran, and 0.79.0 split the parser into
`src/tmux_control/` with more par-mux extensions; cutting them again would be a re-port on every
update for no change in behaviour, so the parser is upstream's.

## Updating

1. Download the new release of `par-term-emu-core` (for example `cargo fetch` in a scratch crate
   that depends on `par-term-emu-core = "=X.Y.Z"`).
2. Commit it as published: replace everything in this folder but `LICENSE`, `PATCHES.md` and
   `.gitignore` with the crate's `src`, `proptest-regressions`, `DESIGN.md` and `Cargo.lock`,
   and its `Cargo.toml.orig` as `Cargo.toml` (when that manifest inherits from upstream's
   workspace, `{ workspace = true }`, take the crate's normalized `Cargo.toml` instead).
3. Cherry-pick the "Engine:" commits that follow the previous "as published" commit, oldest
   first, resolving conflicts (`git log --oneline -- vendor/par-term-emu-core` lists them).
4. Run the engine's tests from a copy outside the workspace with its own `[workspace]` table
   (not under `/tmp`: two kitty file tests refuse it), update its `Cargo.lock` from that copy,
   then pin `par-term-emu-core-rust` in `crates/muxus-term/Cargo.toml` to the same version and
   run `cargo xtask ci` and the `throughput` example (`docs/parity/terminal-perf.md`).
5. Update the version here, in `docs/parity/terminal.md` and in `ATTRIBUTION.md`.
