//! How fast the terminal emulator takes output, without any UI:
//!
//! ```text
//! cargo run --release -p muxus-term --example throughput [-- [STREAM…] [--engine|--emulator]
//!     [--chunk BYTES] [--size COLSxROWS] [--scrollback LINES] [--runs N] [--resize]]
//! ```
//!
//! Feeds each stream in 16 KiB pieces (what the app's output pump hands the emulator) into a
//! 200×50 terminal with 10 000 lines of scrollback, twice: straight into the engine
//! (`par-term-emu-core-rust`), and through [`Emulator::write`] (the Muxus stream filter and its
//! bookkeeping, what a tab runs). Streams: `seq` (`seq 1 2000000`), `log` (the 50 MB of plain log
//! lines `hack/perf.sh` cats), `ansi-log` (50 MB of colored log lines), `ls` (a colored
//! `ls -la`), `htop` and `vim` (full-screen redraws on the alternate screen). The best of
//! `--runs` runs is reported.
//!
//! `--resize` instead fills an emulator with each stream and reports what it holds and what a
//! settled window resize costs it: the resident memory the filled emulator added (run one
//! stream per process for a clean number), and the time to resize it to three quarters of the
//! width and back (the screen and the scrollback reflow as one buffer, as xterm.js does).

mod streams;

use std::time::Instant;

use muxus_term::palette::terminal_scheme;
use muxus_term::terminal::{Emulator, EmulatorConfig};

struct Options {
    streams: Vec<String>,
    engine: bool,
    emulator: bool,
    chunk: usize,
    cols: usize,
    rows: usize,
    scrollback: usize,
    runs: usize,
    resize: bool,
}

fn options() -> Options {
    let mut options = Options {
        streams: Vec::new(),
        engine: true,
        emulator: true,
        chunk: 16 * 1024,
        cols: 200,
        rows: 50,
        scrollback: 10_000,
        runs: 1,
        resize: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| panic!("{name} needs a value"))
        };
        match arg.as_str() {
            "--engine" => options.emulator = false,
            "--emulator" => options.engine = false,
            "--chunk" => options.chunk = value("--chunk").parse().expect("--chunk BYTES"),
            "--scrollback" => {
                options.scrollback = value("--scrollback").parse().expect("--scrollback LINES")
            }
            "--runs" => options.runs = value("--runs").parse().expect("--runs N"),
            "--resize" => options.resize = true,
            "--size" => {
                let size = value("--size");
                let (cols, rows) = size.split_once('x').expect("--size COLSxROWS");
                options.cols = cols.parse().expect("--size COLSxROWS");
                options.rows = rows.parse().expect("--size COLSxROWS");
            }
            "-h" | "--help" => {
                println!(
                    "throughput [seq|log|ansi-log|ls|htop|vim …] [--engine|--emulator] \
                     [--chunk BYTES] [--size COLSxROWS] [--scrollback LINES] [--runs N] \
                     [--resize]"
                );
                std::process::exit(0);
            }
            name => options.streams.push(name.to_string()),
        }
    }
    if options.streams.is_empty() {
        options.streams = ["seq", "log", "ansi-log", "ls", "htop", "vim"]
            .map(String::from)
            .to_vec();
    }
    options
}

fn stream(name: &str, cols: usize, rows: usize) -> Vec<u8> {
    match name {
        "seq" => streams::seq(),
        "log" => streams::plain_log(),
        "ansi-log" => streams::ansi_log(),
        "ls" => streams::ls_la(),
        "htop" => streams::htop(cols, rows),
        "vim" => streams::vim(cols, rows),
        other => panic!("unknown stream {other}"),
    }
}

/// Seconds to feed `data` straight into the engine, set up for scrollback as the emulator sets
/// it up (compact lines).
fn engine(options: &Options, data: &[u8]) -> f64 {
    let mut core = par_term_emu_core_rust::terminal::Terminal::with_scrollback(
        options.cols,
        options.rows,
        options.scrollback,
    );
    core.set_compact_scrollback(true);
    let started = Instant::now();
    for chunk in data.chunks(options.chunk) {
        core.process(chunk);
        let _ = core.drain_responses();
    }
    let took = started.elapsed().as_secs_f64();
    std::hint::black_box(core.active_grid().scrollback_len());
    took
}

/// Seconds to feed `data` through a Muxus emulator.
fn emulator(options: &Options, data: &[u8]) -> f64 {
    let theme = terminal_scheme(None, &[]).theme.clone();
    let mut emulator = Emulator::new(
        EmulatorConfig {
            cols: options.cols as u16,
            rows: options.rows as u16,
            scrollback: options.scrollback,
        },
        &theme,
    );
    let started = Instant::now();
    for chunk in data.chunks(options.chunk) {
        emulator.write(chunk);
        let _ = emulator.take_events();
    }
    let took = started.elapsed().as_secs_f64();
    std::hint::black_box(emulator.content_generation());
    took
}

/// The process's resident memory in bytes (`VmRSS`), where `/proc` tells it.
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// An emulator filled with `data`: the resident MB it added, then the milliseconds to resize
/// it to three quarters of the width and back.
fn resize(options: &Options, data: &[u8]) -> (Option<f64>, f64, f64) {
    let before = resident_bytes();
    let theme = terminal_scheme(None, &[]).theme.clone();
    let mut emulator = Emulator::new(
        EmulatorConfig {
            cols: options.cols as u16,
            rows: options.rows as u16,
            scrollback: options.scrollback,
        },
        &theme,
    );
    for chunk in data.chunks(options.chunk) {
        emulator.write(chunk);
        let _ = emulator.take_events();
    }
    let held = before
        .zip(resident_bytes())
        .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6);
    let (cols, rows) = (options.cols as u16, options.rows as u16);
    let started = Instant::now();
    emulator.resize(cols * 3 / 4, rows);
    let narrower = started.elapsed().as_secs_f64() * 1e3;
    let started = Instant::now();
    emulator.resize(cols, rows);
    let wider = started.elapsed().as_secs_f64() * 1e3;
    std::hint::black_box(emulator.content_generation());
    (held, narrower, wider)
}

fn best(runs: usize, mut run: impl FnMut() -> f64) -> f64 {
    (0..runs.max(1))
        .map(|_| run())
        .fold(f64::INFINITY, f64::min)
}

fn main() {
    let options = options();
    if options.resize {
        println!(
            "{}x{}, {} lines of scrollback",
            options.cols, options.rows, options.scrollback
        );
        println!(
            "{:<10} {:>12} {:>14} {:>12}",
            "stream", "resident MB", "narrower ms", "wider ms"
        );
        for name in &options.streams {
            let data = stream(name, options.cols, options.rows);
            let (held, narrower, wider) = resize(&options, &data);
            let held = held.map_or("-".to_string(), |mb| format!("{mb:.1}"));
            println!("{name:<10} {held:>12} {narrower:>14.1} {wider:>12.1}");
        }
        return;
    }
    println!(
        "{}x{}, {} lines of scrollback, {} byte pieces, best of {}",
        options.cols, options.rows, options.scrollback, options.chunk, options.runs
    );
    println!(
        "{:<10} {:>8} {:>10} {:>10} {:>12} {:>12}",
        "stream", "MB", "engine s", "engine MB/s", "emulator s", "emulator MB/s"
    );
    for name in &options.streams {
        let data = stream(name, options.cols, options.rows);
        let mb = data.len() as f64 / 1e6;
        let cell = |seconds: Option<f64>| match seconds {
            Some(s) => (format!("{s:.2}"), format!("{:.1}", mb / s)),
            None => ("-".into(), "-".into()),
        };
        let (engine_s, engine_rate) = cell(
            options
                .engine
                .then(|| best(options.runs, || engine(&options, &data))),
        );
        let (emulator_s, emulator_rate) = cell(
            options
                .emulator
                .then(|| best(options.runs, || emulator(&options, &data))),
        );
        println!(
            "{name:<10} {mb:>8.1} {engine_s:>10} {engine_rate:>10} {emulator_s:>12} {emulator_rate:>12}"
        );
    }
}
