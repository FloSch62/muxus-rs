# Muxus patches to par-term-emu-core

This is `par-term-emu-core` 0.80.0, the engine member of
[par-term-emu-core-rust](https://github.com/paulrobello/par-term-emu-core-rust) 0.80.0 (MIT,
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

The git history keeps them apart: the commit "Vendor par-term-emu-core 0.80.0 as published"
holds the crate as released, and each commit after it that starts with "Engine:" is one patch.
The manifest is the crate's normalized `Cargo.toml` (what cargo builds from the registry): from
0.80.0 on, `Cargo.toml.orig` inherits from upstream's workspace (`{ workspace = true }`), which
does not resolve here.

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

Upstream's store-wide placement budget (since 0.79.0, `enforce_placement_budget`, 256 MiB) is a
different rule: it drops scrollback images before visible ones instead of the oldest image,
counts bytes per shared buffer, includes virtual placements and runs only when an image is
added, where the addon also evicts when the limit shrinks. Muxus's limits (64 MB, 16 MB in the
background) are lower, so the engine's budget never triggers first.

## xterm.js buffer resize

xterm.js keeps the scrollback and the screen in one list of lines and resizes that list as a
whole (`Buffer.resize`, `_reflowLarger`, `_reflowSmaller`, `BufferReflow.ts`). The engine
reflows the screen on its own (still so in 0.80.0): content stays top-aligned, the cursor keeps
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

## Kitty animation frames

Upstream took every `a=f` without `r=`, and every `a=f,r=1`, as a new image: it deleted the
image's placements and placed it again, at its own pixel size, at the cursor, and moved the
cursor below it. A program that animates one image by editing its root frame (kitty-doom sends
`a=T` sized in cells with `C=1`, then `a=f,r=1` for every frame) drew each frame small and one
image height lower until it scrolled. On `a=f` it also read `c=` as the composition mode, which
is `X=`. As kitty does now: frame 1 is the image itself; `a=f,r=N` edits frame N in place,
without `r=` a frame is appended, the data is a rectangle composed onto the frame at `x=`,`y=`
(blended, or copied with `X=1`), a new frame starts from frame `c=` or the color `Y=`; no
placement is made and the cursor stays; placements show the image's current frame, which
`a=a,c=N` sets (`a=a,r=N,z=G` sets a frame's gap). A frame for an image that does not exist, or
an edit of a frame that does not exist, is an error.

| File | Change |
|---|---|
| `src/graphics/frames.rs` | New: `GraphicsStore::edit_kitty_frame`, `set_kitty_current_frame`, `set_kitty_frame_gap`; the rectangle's composition. The root frame joins an animation by sharing the image's buffer, so it is budgeted once. |
| `src/graphics/kitty/dispatch.rs` | `apply_frame_command` hands the decoded rectangle to `edit_kitty_frame` and returns no graphic; `apply_animation_control` applies `c=` and `r=`/`z=`. |
| `src/graphics/kitty/parse.rs`, `mod.rs` | On `a=f`: `c=` is `base_frame`, `X=` `frame_composition`, `Y=` `background_color`; on `a=a`: `c=` is `current_frame`, `r=` and `z=` a frame and its gap. |
| `src/graphics/animation.rs` | `AnimationFrame::new_shared`. |
| `src/graphics/kitty/tests.rs`, `src/terminal/tests/kitty_apc.rs` | Upstream's frame tests that built an image from `a=f` alone now transmit it first; new tests for edits in place, appended frames, base frames and background, the current frame, gaps, and kitty-doom's sequence end to end (`root_frame_edits_repaint_the_placement_in_place`). |

Frames are not played on a timer: nothing in Muxus calls `update_animations`, as before.

## The pen around the alternate screen (DECSET 1049)

xterm (and xterm.js, `saveCursor`/`restoreCursor` around `activateAltBuffer`) saves the cursor
as DECSC does when 1049 switches to the alternate screen and restores it as DECRC does when it
switches back, the SGR pen with it. The engine swapped the position only, so a program that
leaves colors set on exit kept them on the shell's screen, and the next erase (with background
color erase) painted them over it: DOOM-fire-zig leaves black on light yellow, and `clear` made
the whole screen yellow. The position stays with the screen swap (which a resize moves along
with the lines); the pen saved on entering comes back on leaving. 47 and 1047 do not save it,
as in xterm.

| File | Change |
|---|---|
| `src/terminal/sequences/csi/mode.rs`, `src/terminal/mod.rs` | `Terminal::alt_entry_attrs`: set by DECSET 1049 on the primary screen, restored by DECRST 1049 when it leaves the alternate screen. |
| `src/terminal/tests/modes.rs` | `test_alt_screen_1049_restores_the_pen`. |

## Kitty keyboard pop without a count

`CSI < u` pops one entry off the keyboard flags stack when it has no count, as kitty and
xterm.js (`params[0] || 1`) do. vte reports the missing count as 0, so the engine popped
nothing: kitty-doom pushes `CSI > 11 u` and pops with a bare `CSI < u` on exit, and the flags
stayed on, every key then reaching the shell as a kitty key report.

| File | Change |
|---|---|
| `src/terminal/sequences/csi/keyboard.rs` | A count of 0 pops one entry. |
| `src/terminal/tests/terminal_tests.rs` | `test_kitty_keyboard_pop_without_a_count_pops_one`. |

## The underline color in the semicolon form (SGR 58)

SGR 58 read the colon form only (`58:5:n`, `58:2::r:g:b`). In the semicolon form its arguments
were taken as SGR codes: `CSI 4;58;5;7 m` turned on blink and inverse, and `58;2;255;0;0`
ended in an SGR 0. It now goes through `parse_extended_color` as 38 and 48 do.

| File | Change |
|---|---|
| `src/terminal/sequences/csi/style.rs` | SGR 58 parses both forms. The test is Muxus's `reads_the_semicolon_form_of_the_underline_color` (`crates/muxus-term/src/terminal.rs`). |

## The color space in colon truecolor (`38:2::r:g:b`)

ITU T.416 puts a color-space id before the components: `38:2:cs:r:g:b`, usually written with the
id left empty, `38:2::r:g:b` (Muxus's own serializer writes underline colors that way). The
engine read the components from the id's slot, so `58:2::1:2:3` was `Rgb(0, 1, 2)` and restored
scrollback had the wrong underline colors. With six or more sub-parameters the components now
follow the id, as in xterm; `38:2:r:g:b` is read as before.

| File | Change |
|---|---|
| `src/terminal/sequences/csi/style.rs` | `parse_extended_color_subparams` skips the color-space slot. The tests are Muxus's `reads_colon_truecolor_with_and_without_the_color_space` (`crates/muxus-term/src/terminal.rs`) and the underline colors in `round_trips_through_a_replay` (`crates/muxus-term/src/serialize.rs`). |

## Semicolons in OSC 8 URIs

vte splits an OSC at every `;`, and the hyperlink handler took the third piece as the URI, so
`OSC 8 ; ; https://h/p;a=1` linked to `https://h/p`. Everything after the parameters is the URI,
`;` included, as in xterm.js.

| File | Change |
|---|---|
| `src/terminal/sequences/osc/mod.rs` | `handle_osc_hyperlink` joins the pieces after the parameters with `;`. The test is Muxus's `keeps_semicolons_in_hyperlink_urls` (`crates/muxus-term/src/terminal.rs`). |

## Escape sequences with intermediates

The two-byte escape sequences (DECSC, DECRC, HTS, RI, IND, NEL, RIS, SPA, EPA, DECPAM, DECPNM)
matched any intermediates, so `ESC # 8` (DECALN) ran DECRC and `ESC ( =` (a character set
designation) turned on the application keypad. They now match only without intermediates.
DECALN itself is still not implemented and does nothing.

| File | Change |
|---|---|
| `src/terminal/sequences/esc.rs` | Those arms match `[]` instead of `_`. The test is Muxus's `escape_sequences_with_intermediates_are_not_their_plain_forms` (`crates/muxus-term/src/terminal.rs`). |

## SGR 21

xterm and xterm.js take SGR 21 as doubly underlined (ECMA-48); the engine ignored it.

| File | Change |
|---|---|
| `src/terminal/sequences/csi/style.rs` | SGR 21 turns on the double underline; SGR 24 turns it off. The test is Muxus's `sgr_21_is_a_double_underline` (`crates/muxus-term/src/terminal.rs`). |

OSC 4, 104 and 110–112 no longer reach the engine: Muxus answers and applies them itself
(`crates/muxus-term/src/filter.rs`, `muxus_term::terminal::Emulator`), on top of the theme.

## A wide character in the last column

A character that ends in the last column leaves the cursor there with the wrap pending (DECAWM),
so the next character starts the next row. The engine did this for narrow characters only: after
a wide character it clamped the cursor onto the spacer without the pending wrap, and the next
character overwrote the spacer (6 columns, `abcd中x`: the `x` landed in the sixth column and was
lost from copies). xterm and xterm.js wrap there.

| File | Change |
|---|---|
| `src/terminal/write.rs` | The pending wrap is set whenever a character ends at the last column, whatever its width. |
| `src/terminal/write.rs` (tests), `crates/muxus-term/tests/engine_write.rs` | `a_wide_character_ending_in_the_last_column_leaves_the_wrap_pending`, `a_wide_character_in_the_last_column_wraps_the_next_character`. |

## Combining marks during the pending wrap

A combining mark, variation selector, ZWJ or ZWJ continuation attaches to the character written
last. While a wrap is pending the cursor still sits on that character in the last column, but the
engine looked one cell to the left: with 3 columns, `abc` + U+0301 accented the `b` (NFD text
such as macOS file names). A regional indicator completing a flag looked there too and could
write its spacer over the last column.

| File | Change |
|---|---|
| `src/terminal/write.rs` | `Terminal::last_written_cell`: the cursor's own cell while a wrap is pending, else the one before it, a spacer resolved to its wide character; the combining and ZWJ paths use it. A regional indicator during the pending wrap starts a new pair on the next row. |
| `src/terminal/write.rs` (tests), `crates/muxus-term/tests/engine_write.rs` | `a_combining_mark_during_the_pending_wrap_joins_the_last_column`, `a_regional_indicator_during_the_pending_wrap_starts_a_new_pair`, `a_combining_mark_after_the_last_column_joins_the_character_there`. |

## Unicode 11 widths for emoji sequences

The engine widened a narrow cell to two columns when a variation selector 16 or a ZWJ
continuation joined it, without moving the cursor (`❤️xyz`: the `x` landed on the heart's second
column and was lost), and folded an emoji after a ZWJ, or a skin tone modifier, into the previous
cell, swallowing its two columns. Either way the cursor ended up left of where the program writing
the text puts it.

Two ways out: widen and also advance the cursor (kitty, WezTerm with a newer `unicode_version`,
Ghostty's grapheme clustering, mode 2027), or keep every character's own width. This copy keeps
the widths. Shells, readline, editors and ncurses count columns with `wcwidth`, where VS16 and ZWJ
are 0 and every emoji has its own width, so `❤️` is 1 column and `👨‍💻` 4; xterm, VTE, Alacritty,
WezTerm's default and xterm.js with the unicode11 addon (what Muxus mirrors) draw them that way.
A terminal that widens disagrees with those programs about every following column, which breaks
line editing and full-screen redraws, unless both sides agree on mode 2027.

| File | Change |
|---|---|
| `src/terminal/write.rs` | Only zero-width characters (combining marks, variation selectors, ZWJ) attach to the previous cell, and attaching never changes its width. An emoji after a ZWJ and a skin tone modifier are written as characters of their own. Regional indicator pairs stay one two-column cell (1 + 1 columns). |
| `src/cell.rs` | `Cell::combining_has` (unused now) removed. |
| `src/terminal/tests/screen.rs` | `test_get_word_at_emoji_zwj_line` expects the laptop in its own cells. |
| `src/terminal/write.rs` (tests), `crates/muxus-term/tests/engine_write.rs` | `emoji_sequences_keep_unicode_11_widths`, `emoji_sequences_keep_unicode_11_widths_so_the_cursor_follows_the_program`. |

## Kept as upstream has it

**OpenTelemetry (`src/otel.rs`, `src/otel/`, 0.80.0).** Always compiled, but without the
engine's `otel` feature it is plain std: the `OtelProtocol` enum, an OTLP header parser and a
stub `init` that returns `None`. Nothing in the engine calls it, it reads no environment and
opens nothing. The SDK-backed provider and its dependencies (`opentelemetry`,
`opentelemetry_sdk`, `opentelemetry-otlp` with tonic and reqwest) compile only with that
feature, which Muxus never enables: `muxus-term` depends on `par-term-emu-core-rust` with
default features off, and none of those crates are in the workspace's `Cargo.lock` (they are in
this folder's own lockfile, which resolves every optional dependency for the standalone test
run). Even with the feature on, metrics go out only from a binary that calls `init` with its
own runtime switch on. So it stays unpatched.

**The tmux parser's par-mux roster notifications.** The 0.60.0 copy also removed the par-mux pane-roster notifications (`%agent-state-changed`,
`%agent-released`, `%agent-telemetry-changed`) from the tmux control-mode parser. Muxus never
turns tmux control mode on, so they never ran, and 0.79.0 split the parser into
`src/tmux_control/` with more par-mux extensions; cutting them again would be a re-port on every
update for no change in behaviour, so the parser is upstream's.

## Updating

1. Download the new release of `par-term-emu-core` (for example `cargo fetch` in a scratch crate
   that depends on `par-term-emu-core = "=X.Y.Z"`).
2. Commit it as published: replace everything in this folder but `LICENSE`, `PATCHES.md` and
   `.gitignore` with the crate's `src`, `proptest-regressions`, `DESIGN.md`, `Cargo.lock` and
   normalized `Cargo.toml`.
3. Cherry-pick the "Engine:" commits that follow the previous "as published" commit, oldest
   first, resolving conflicts (`git log --oneline -- vendor/par-term-emu-core` lists them). The
   manifest edits (`memchr` in; `portable-pty` and the readme out) go in by hand.
4. Run the engine's tests from a copy outside the workspace with its own `[workspace]` table
   (not under `/tmp`: two kitty file tests refuse it), update its `Cargo.lock` from that copy,
   then pin `par-term-emu-core-rust` in `crates/muxus-term/Cargo.toml` to the same version and
   run `cargo xtask ci` and the `throughput` example (`docs/parity/terminal-perf.md`).
5. Update the version here, in `docs/parity/terminal.md` and in `ATTRIBUTION.md`.
