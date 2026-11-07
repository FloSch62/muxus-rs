# Parity: terminal performance

How fast a terminal tab takes output, against xterm.js 6.1 in the Electron build
(`client/src/components/TerminalViewImpl.tsx` feeding xterm's write buffer). The
target: within 2× of xterm.js (`seq 1 2000000` in 2 s or less, a 50 MB `cat` in
3 s or less) with frames still painting during the flood, and no feature loss.

```bash
# The emulator alone, per stream (no UI), release:
CARGO_TARGET_DIR=target-release cargo run --release -p muxus-term --example throughput -- --runs 3
# What a filled emulator holds, and what a settled resize costs it (one stream per process):
CARGO_TARGET_DIR=target-release cargo run --release -p muxus-term --example throughput -- log --resize
# The app end to end (headless GNOME Shell, lavapipe):
hack/perf.sh --binary target-release/release/muxus
# The engine's own tests (it is excluded from the workspace):
cd vendor/par-term-emu-core && cargo test --release
```

The `throughput` example feeds six streams in 16 KiB pieces (what the app's
output pump hands a terminal) into a 200×50 terminal with 10 000 lines of
scrollback: `seq` (the output of `seq 1 2000000`), `log` (the 50 MB of plain log
lines `hack/perf.sh` cats), `ansi-log` (50 MB of colored log lines: dim, bold,
256-color and truecolor SGRs, underlines, lines that wrap, wide characters and
emoji), `ls` (a colored `ls -la`), and `htop` and `vim` (full-screen redraws on
the alternate screen: cursor addressing, meters, a full-width header,
scroll-region scrolling, delete-line, erase-line). Each runs straight into the
engine and through `Emulator::write` (the Muxus stream filter and its
bookkeeping, what a tab runs).

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| `seq 1 2000000` within 2× of xterm.js | xterm.js `WriteBuffer`, `InputHandler.print` | done | 1.03–1.08 s in the app (xterm.js: 1.04 s), was 22.4 s |
| 50 MB `cat` within 3 s | same | done | 1.06–1.65 s in the app, was 9.8 s |
| Frames during a flood | xterm.js render debouncer | done | frame gaps while output flows: p95 22 ms (`seq`), 34 ms (`cat`); the longest output batch on the UI thread is under 1 ms (8 ms once) |
| Idle CPU after a flood | xterm.js | done | below what `hack/perf.sh` can resolve (under 0.3% of a core over 3 s), one tab and ten tabs |
| Memory | xterm.js `CircularList` of `BufferLine`s | partial | scrollback lines store their written cells only, and a cell is 32 bytes (was 40; xterm.js: 12): its cached width moved into the byte its flags padded to (`vendor/par-term-emu-core/PATCHES.md`, "Cell size"). Measured with `throughput <stream> --resize` (an emulator filled at 200×50 with 10 000 lines of scrollback, release, the resident memory it added): plain log 46.1 → 37.2 MB, colored log 57.8 → 46.9 MB, `ls -la` 28.6 → 23.3 MB, `seq` 5.3 → 4.7 MB, 19–20% less; output is as fast or faster (interleaved runs on a loaded machine: log 87–91 → 109–130 MB/s, colored log 61–76 → 70–83 MB/s, `seq` 55–59 → 64–74 MB/s). `hack/perf.sh` was not rerun (this machine has no GNOME Shell). Not taken, see below: 24 bytes (the combining marks, the underline color and the hyperlink id out of line together) and xterm.js's 12 |
| Engine benchmark | none | done | `crates/muxus-term/examples/throughput` |
| Benchmark of the app | none | done | `hack/perf.sh`, now with the process's CPU per step |
| Same behaviour as before | all of the above | done | the engine's 1890 tests plus 7 new ones (property tests against the per-character path), 285 + 2 terminal tests, and the final state after every benchmark stream is identical to the original engine's (the engine has 1998 tests now on 0.80.0, 1932 of them upstream's) |

## Results

### The emulator without UI

`throughput --runs 3` (best of three), release, the same machine. "Before" is
the crates.io engine 0.60.0 with the previous stream filter; "after" is this
tree (the engine with compact scrollback, as the emulator sets it up).

| Stream | MB | Engine before | Engine after | Emulator before | Emulator after |
|---|---|---|---|---|---|
| `seq` | 16.9 | 0.8 MB/s | 53.0 MB/s | 0.7 MB/s | 52.7 MB/s (×75) |
| `log` | 52.4 | 9.2 MB/s | 89.9 MB/s | 8.1 MB/s | 88.6 MB/s (×11) |
| `ansi-log` | 52.4 | 11.9 MB/s | 75.8 MB/s | 9.8 MB/s | 64.2 MB/s (×6.6) |
| `ls` | 21.0 | 6.2 MB/s | 80.6 MB/s | 5.8 MB/s | 72.3 MB/s (×12) |
| `htop` | 21.0 | 56.5 MB/s | 177.3 MB/s | 35.8 MB/s | 115.1 MB/s (×3.2) |
| `vim` | 21.0 | 7.9 MB/s | 126.1 MB/s | 7.1 MB/s | 105.1 MB/s (×15) |

The before engine was slower still without scrollback (`seq` at 0.3 MB/s): the
alternate screen and scroll regions rotated every cell of the region on each
scrolled line.

### In the app

`hack/perf.sh`, release build, headless GNOME Shell with lavapipe (software
rendering, so frame times are pessimistic), one plain bash tab, on a laptop
shared with other builds (runs vary; the ranges are from four runs):

| Run | Before | After |
|---|---|---|
| `seq 1 2000000` (16.9 MB) | 22.4 s, 0.8 MB/s | 1.03–1.08 s, 16 MB/s |
| `cat` of 50 MB of log lines | 9.8 s, 5.4 MB/s | 1.06–1.65 s, 32–50 MB/s |
| frame gaps while output flows, p95 / worst | 52 ms / 211 ms (`seq`), 50 ms / 50 ms (`cat`) | 22 ms / 23 ms (`seq`), 34 ms / 35 ms (`cat`) |
| longest output batch on the UI thread | 34 ms | under 1 ms (8 ms in one run) |
| CPU of the app process during the flood | | 127–136% (`seq`), 75–127% (`cat`): the UI thread plus the pty reader and session threads |
| CPU after the flood, ten idle tabs | | under 0.3% |
| resident memory, one tab / ten tabs | 194 / 198 MiB | 187 / 189 MiB |

`seq` is now bound by `seq` itself: it writes each line with its own `write`
into the pty, which costs it about 0.9 s of system time, and the app keeps up
(the pty reader hands the window about 300 bytes per event, some 33 000 events
a second). The frame gaps above were taken by timing the painted frames between
which output arrived; `hack/perf.sh`'s once-a-second ticks give the same numbers
(p95 23–24 ms and 34–40 ms) but also carry the first frame after the idle prompt
(up to 65 ms) into the `seq` row.

## What changed

### The engine: `vendor/par-term-emu-core`

`par-term-emu-core` (MIT; 0.60.0 when this work was done, 0.80.0 now, see
"Rebased onto engine 0.79.0" and "… and 0.80.0" below) is vendored and patched in through
`[patch.crates-io]` in the root `Cargo.toml`; `par-term-emu-core-rust` (the
crate `muxus-term` depends on) re-exports it unchanged. The profile (a sampling
profiler over the benchmark streams) showed where the time went; each change
answers one finding:

| Finding (share of the profile) | Change |
|---|---|
| `seq`: 87% scrolling one line. The screen was one flat `Vec<Cell>`: every scrolled line drained the top row out (moving the other 49 rows), copied it into a fresh allocation, and evicted the oldest scrollback line with `Vec::remove(0)` on a 10 000-entry `Vec` | The screen is one boxed row per line (`Grid::screen`), the scrollback a `VecDeque`. Scrolls, scroll regions, IL and DL rotate row pointers; a line leaving the screen moves into the scrollback by pointer; eviction is O(1) |
| `seq` then: 75% blanking the evicted 8 KB row (cold memory) to reuse it | Compact scrollback (`Terminal::set_compact_scrollback`, off by default, on in Muxus): a line enters the scrollback without its trailing default cells, so only its written cells are copied and the screen row is blanked up to its written part (`Grid::row_used`, kept by every mutator). Wrapped lines keep their full width. A short line reads exactly like one padded with default cells, the form scrollback lines already took after the screen got wider |
| `log`: per character, three colors packed, the row marked damaged, the debug level checked, CR and LF run through the combining-mark tables | Printable ASCII is collected by `print` and written a row segment at a time (`Terminal::write_ascii_run`, `Grid::write_ascii`): the pen is packed once and the row marked once per segment. Every other parser callback writes the pending run first, and so does the end of every parse pass. Charsets, insert mode, left/right margins, autowrap off and trace logging keep the per-character path. C0 controls skip the grapheme checks |
| Every scrolled line stamped all 50 rows' damage twice | A full-screen damage mark stamps one generation for all rows (`all_rows_gen`); the queries read `max(row, all)` |
| `vim`: EL marked the row damaged once per erased cell | Erases (EL, ECH, ED, row clears) reset a span with one mark, and with the default background only the written part |
| ANSI streams: the OSC length guard and the kitty APC pre-filter copied every byte | Both skip to the next ESC (or APC terminator) with `memchr` and copy runs at once; the guard copies nothing unless it drops a byte |
| Every scrolled line rebuilt the zone list | Zone eviction returns early when no zone reaches below the floor |

Behaviour kept: the engine's own 1890 tests pass (the two kitty file tests that
need a checkout outside the temp directory included). New tests in
`src/terminal/tests/fast_paths.rs`: property tests that random streams (text,
controls, wide and combining characters, SGR, cursor addressing, scroll
regions, IL/DL/SU/SD, autowrap and insert mode, left/right margins, DEC
graphics, hyperlinks, protection, erases, the alternate screen, reports), cut at
random points, leave the terminal exactly as the per-character path does; that
compact scrollback reads like full-width lines; that cells past a row's written
part are blank after any stream and resize; and the wrap flag and damage cases
below. `src/terminal/apc_filter.rs` checks the APC runs against one byte at a
time. Beyond the tests, the final state of both screens (cells, attributes,
wrap flags, scrollback padded to the width, cursor) after each of the six
benchmark streams hashes identically for the crates.io engine and this one,
with compact scrollback on and off.

One deliberate difference: a line's wrap flag now moves with the line when a
scroll region scrolls or IL/DL shift lines (it stayed at its screen position,
describing whatever line moved there; xterm keeps `isWrapped` on the line).

**Upstream.** The changes are self-contained and meant to be offered to
`par-term-emu-core-rust`: `src/grid/{mod,scroll,edit,erase,zone}.rs`,
`src/terminal/{write,perform,action,apc_filter}.rs`, the OSC guard and
`set_compact_scrollback` in `src/terminal/mod.rs` (its flag in
`src/terminal/state.rs` since 0.79.0), `memchr` in `Cargo.toml`, and
`src/terminal/tests/fast_paths.rs`. Local to this copy and not for upstream: the
PTY backend (`pty_session`, which Muxus does not use; the feature now stops the
build) and the design notes are left out.

### `muxus-term`

- `Emulator::new` turns on compact scrollback.
- `StreamFilter` (the profile showed half of `Emulator::write` in it for colored
  output): text, string payloads and CSI parameters are copied in runs found with
  `memchr` instead of byte by byte; an SGR is only re-serialized when it holds
  one of the colors it rewrites (an allocation-free check runs first; 31% of the
  colored-log profile was formatting unchanged SGRs); the CSI buffer keeps its
  capacity; a DECSET/DECRST only splits the output when it holds a mode the
  emulator follows (mouse protocols and encodings, alternate screens), so the
  cursor toggles of full-screen programs no longer cut the stream into pieces.
  Tests: the bulk runs against stepping byte by byte on random streams cut at
  random points, the rewrite check against the rewrite on random parameter
  lists, long CSI and OSC sequences split anywhere.
- `Emulator::write` no longer allocates the title on every write.

### `muxus-term-view`

- Painting builds a text run (and its font, with the fallback list) per style
  change instead of per cell. A test checks that the cheap run key agrees with
  comparing the runs.

Measured in the app with timing probes (since removed): building a frame's rows
takes about 1.4 ms during a flood (every row is new each frame), painting
0.5–6 ms (new lines are shaped as they arrive; GPUI reuses shaped lines across
frames), and feeding the emulator about half of the UI thread during `seq`.

### `hack/perf.sh`

- Samples the app process's CPU time from `/proc` (found by its executable and
  scratch `HOME`, never another Muxus) and reports it per step, right after
  `seq` and with ten idle tabs.
- Steps are built from the ticks that lie mostly inside them, and the tick that
  covers the opening of the nine tabs (a stall of 300–400 ms, every run) is no
  longer counted as a `cat` frame gap.

## Left on the table

- **The pty event size.** During `seq` the local transport delivers about 300
  bytes per event and the window takes some 33 000 updates a second. Merging
  queued output before it reaches the UI thread (in `muxus-local`/
  `muxus-session`, or a short coalescing wait in the app's output pump) would cut
  the CPU of a flood; the wall time is already the writer's.
- **Opening tabs stalls frames** for 300–400 ms (nine local shells in
  `hack/perf.sh`), in `muxus-app`.
- **Per-character parsing.** `vte` still decodes and dispatches every character;
  the printable run is batched after it. Batching inside the parser needs a
  `vte` fork.
- **Row building per frame.** During a flood every visible row is rebuilt each
  frame (1.4 ms). Reusing rows that only moved (the engine's scroll-aware damage,
  `Grid::scroll_damage_since`) would make that proportional to the new lines.
- **Cell size.** A cell is 32 bytes (xterm.js packs one into 12); compact
  scrollback keeps short lines small, but full lines still cost 6.4 KB at 200
  columns. The 32 come from the 8-byte pointer to the combining marks (which
  sets the alignment), four 4-byte words (character, foreground, background,
  underline color) and 8 bytes of flags (attribute bits, underline style,
  width, hyperlink id). 24 bytes would take the combining marks, the underline
  color and the hyperlink id out of line into one shared, reference-counted
  record: rare per cell, but the write paths would clone it for every cell
  written while a link or an underline color is set, and every user of
  `CellFlags::hyperlink_id` (34 in the engine, the emulator's styling) and of
  the underline color changes. 12 bytes is xterm.js's layout (the character
  and its width in one word, colors and attributes in two, extended
  attributes and combined characters in per-line maps), which reworks every
  reader and writer of the grid. Both are left: the first saves a further
  quarter for a broad, risky change to the write paths.
- **Buffer reflow.** A resize now treats the scrollback and the screen as one
  buffer, as xterm.js does (`vendor/par-term-emu-core/src/grid/reflow.rs`), so it
  costs in proportion to the scrollback: resizing a filled 200×50 emulator with
  10 000 lines to 150 columns, or back, takes about 1 ms with plain log lines
  and 7–14 ms with the colored log's wrapped lines (release,
  `throughput --resize`), once per settled resize. Rows the reflow does not
  touch move by pointer.
- **Scrollback size changes** are done: `Grid::set_max_scrollback` drops the
  oldest lines from the ring buffer in one drain (`PATCHES.md` in the engine).
- Non-ASCII text (CJK, emoji) still takes the per-character path.

## Attribution

- par-term-emu-core (MIT, © 2025 Paul Robello): vendored and modified in
  `vendor/par-term-emu-core` (its `LICENSE` is kept there). The changes are
  listed above.

## Integration pass

- **Fewer output updates.** The window's pump (`ui/terminal.rs`) gathers streaming output over
  one or two event-loop turns before it hands a piece to the emulator (only while the previous
  update is under 2 ms old, so an echo after a pause goes in at once). A `seq` flood went from
  ~35 k UI updates a second to ~1 k. A timer-based merge (1–4 ms waits) was tried and dropped:
  waiting backs the session up and `seq` took 1.5–2.9 s.
- **Opening tabs.** `hack/perf.sh` now reports the longest time the UI thread was busy in one
  go while the nine tabs open (a 4 ms heartbeat's lateness); the frame gap it reported before
  was mostly idle time. Hidden tabs opened together dial 20 ms apart, because each local shell
  forks the app process: 59–70 ms → 35 ms.

## After the end-to-end pass (interface zoom, scrollback snapshots)

`hack/perf.sh`, release, two runs (the first on a machine still busy, load 3–5): `seq 1 2000000`
1.06–1.07 s; `cat` of 50 MB 1.41–1.74 s (1.24–1.34 s before; the quiet run is within noise of
it); worst p95 frame gap 20.6–26.5 ms; opening nine tabs blocks the UI thread for 5.6–27.1 ms
at most; RSS 195 MiB with one tab, 197 MiB with ten; CPU idle 0.0%, ten idle tabs 0.0%. Right
after `seq` the process uses 2.5% of a core (0.0% before): the tab stores its scrollback
snapshot 1.5 s after the output stops (`ui/terminal.rs`, `persist_snapshot`). Zoomed sizes cost
one multiplication per `px`; the terminal's cell layout stays in layout space.

## Rebased onto engine 0.79.0

The vendored engine moved from 0.60.0 to 0.79.0, as the release as published followed by one
commit per patch (`vendor/par-term-emu-core/PATCHES.md`). Upstream did not touch the paths this
work changed: the grid's scrolling, printing and resize code and the cell are as in 0.60.0 but
for three fixes (a full-screen erase clears the wrap flags, a snapshot truncates scrollback rows
wider than the grid, the styled scrollback export keeps the newest lines and joins wrapped rows
in order), and nothing in 0.79.0 does what a patch does. The rest of `Terminal` moved out of
`terminal/mod.rs` into topic files (`state.rs`, `host_config.rs`, …), which only moved the
patches' host-config flags into `terminal/state.rs`. Every patch was re-applied but the
trimming of the tmux parser's par-mux roster notifications (Muxus never turns tmux control mode
on).

Checked: the engine's 1928 tests and the patches' 66 pass, as do the emulator's 357, the
view's 19 and `cargo xtask ci`. The final state after every benchmark stream hashes the same
on 0.60.0 and on 0.79.0 with the patches: both screens' cells with their widths and wrap flags,
the scrollback padded to the width and the cursor, for the engine with compact scrollback on
and off, for the emulator, and for the emulator after a resize to 150 columns and then to
200×43.

Release builds of `throughput` on both engines, run in turns on a shared machine (load 6–17).
Instructions are user-space instructions of one run (`perf stat`, the stream's generation
included, so the engine's own share fell a little more); MB/s are the lowest and highest best
of three over three rounds:

| Stream | Instructions, engine | Instructions, emulator | Engine MB/s | Emulator MB/s |
|---|---|---|---|---|
| `seq` | 5.08 → 4.99 G (−1.7%) | 5.10 → 5.01 G (−1.7%) | 73–76 → 76–78 | 75–76 → 74–78 |
| `log` | 6.52 → 6.31 G (−3.3%) | 6.53 → 6.32 G (−3.2%) | 121–154 → 152–158 | 147–154 → 152–156 |
| `ansi-log` | 7.26 → 7.08 G (−2.4%) | 9.12 → 8.94 G (−2.0%) | 100–112 → 108–114 | 86–88 → 87–92 |
| `ls` | 3.35 → 3.27 G (−2.6%) | 3.58 → 3.49 G (−2.5%) | 108–128 → 106–128 | 111–115 → 92–98 |
| `htop` | 2.92 → 2.86 G (−1.9%) | 3.79 → 3.73 G (−1.5%) | 261–272 → 260–266 | 160–168 → 161–173 |
| `vim` | 2.72 → 2.69 G (−1.0%) | 3.34 → 3.31 G (−0.8%) | 187–192 → 171–186 | 148–157 → 151–154 |

The `ls` emulator row is the machine's load: run on its own in turns (three rounds, best of
ten) it reads 86.5–87.7 → 82.4–94.2 MB/s, and its instruction count fell like the others'.

`throughput <stream> --resize` (one stream per process, three rounds): resident memory of the
filled emulator plain log 37.1–37.3 → 37.1 MB, colored log 46.9–47.0 → 46.6–46.9 MB, `ls -la`
23.2–23.3 → 23.1 MB, `seq` 4.6 → 4.4–4.5 MB; the settled resize to 150 columns and back
0.9–1.3 / 0.4–0.6 ms → 0.9–1.0 / 0.4 ms (plain log) and 5.5–7.4 / 6.9–8.8 ms → 6.0–8.8 /
7.1–8.6 ms (colored log).

### … and 0.80.0

0.80.0 changes none of the patched code either: it bounds iTerm2 multipart transfers (a restart
fails the transfer it supersedes, at most eight run at once), writes the styled export's SGR
parameters in place, and adds an OpenTelemetry module that is inert without its feature
(`PATCHES.md`, "Kept as upstream has it"). The series went on unchanged; only the manifest edits
were redone, as the published manifest is now the normalized one. The engine's 1932 tests and
the patches' 66 pass, as do the emulator's 357, the view's 19 and `cargo xtask ci`, and the
final states after the benchmark streams and resizes hash the same as on 0.79.0 (so as on
0.60.0).

Instructions of one feed alone (a run with three feeds minus a run with one, so the stream's
generation cancels out), user space, release:

| Stream | Engine 0.60.0 → 0.79.0 → 0.80.0 | Emulator 0.60.0 → 0.79.0 → 0.80.0 |
|---|---|---|
| `seq` | 4.60 → 4.51 → 4.60 G | 4.61 → 4.53 → 4.61 G |
| `log` | 5.08 → 4.87 → 5.12 G | 5.09 → 4.88 → 5.14 G |
| `ansi-log` | 6.36 → 6.18 → 6.33 G | 8.22 → 8.04 → 8.19 G |
| `ls` | 2.39 → 2.30 → 2.39 G | 2.61 → 2.52 → 2.62 G |
| `htop` | 1.70 → 1.64 → 1.69 G | 2.57 → 2.51 → 2.56 G |
| `vim` | 2.10 → 2.07 → 2.12 G | 2.72 → 2.69 → 2.74 G |

0.80.0 is within 1% of the 0.60.0 copy on every stream. The dip on 0.79.0 was the compiler, not
the engine: the hot paths are the same source in 0.79.0 and 0.80.0, and a profile of `log`
shows the same work split differently between functions (on 0.80.0 `Grid::write_ascii` is
inlined into `Terminal::flush_print_run`, and freeing evicted rows moved out of
`Grid::rotate_region` into its own function). Throughput in turns with the 0.79.0 build (two
rounds, load 7–8) follows the counts, `log` a little slower than on 0.79.0 and the rest within
the noise: `seq` 66–79 → 71–76 MB/s, `log` 134–159 →
118–135 (engine) and 130–141 → 132–134 (emulator), `ansi-log` 101–110 → 101–111, `htop`
254–256 → 228–252, `vim` 167–174 → 170–182. Resident memory of a filled emulator: plain log
37.2–37.5 MB, colored log 46.9 MB, `ls -la` 23.2–23.4 MB, `seq` 4.4–4.9 MB (0.79.0 in the same
runs: 37.1, 46.9, 23.1, 4.4–4.5); resizes to 150 columns and back as before.
