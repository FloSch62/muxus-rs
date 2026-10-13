//! The byte streams the throughput benchmark feeds the emulator. Every stream is generated
//! deterministically, with the pty's CR LF line endings, so runs compare with each other.

/// A small deterministic pseudo-random sequence (64-bit LCG), so streams vary without a seed file.
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    pub fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// The output of `seq 1 2000000`: short lines, so the cost per scrolled line dominates.
pub fn seq() -> Vec<u8> {
    let mut out = Vec::with_capacity(17 << 20);
    for n in 1..=2_000_000u32 {
        out.extend_from_slice(n.to_string().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// 50 MB of plain log lines, the file `hack/perf.sh` cats (about 100 characters per line).
pub fn plain_log() -> Vec<u8> {
    const LEVELS: [&str; 4] = ["INFO", "WARN", "DEBUG", "ERROR"];
    let mut out = Vec::with_capacity(51 << 20);
    let mut i = 0u64;
    while out.len() < 50 << 20 {
        out.extend_from_slice(
            format!(
                "2026-03-12T09:{:02}:{:02}.{:03}Z {:5} edge[{}]: request id={i:08x} path=/api/v1/items/{} status=200 bytes={}\r\n",
                i / 3600 % 60,
                i % 60,
                i % 1000,
                LEVELS[(i % 4) as usize],
                1000 + i % 97,
                i % 9973,
                i % 65536
            )
            .as_bytes(),
        );
        i += 1;
    }
    out
}

/// 50 MB of colored log lines as a logging library prints them: dim timestamps, colored levels,
/// bold names, 256-color and truecolor highlights, the odd underline, long lines that wrap, and
/// now and then a line with wide characters and emoji.
pub fn ansi_log() -> Vec<u8> {
    let mut out = Vec::with_capacity(51 << 20);
    let mut rng = Lcg::new(7);
    let mut i = 0u64;
    while out.len() < 50 << 20 {
        let level = match rng.below(20) {
            0 => "\x1b[1;31mERROR\x1b[0m",
            1..=3 => "\x1b[33mWARN \x1b[0m",
            4..=7 => "\x1b[38;5;244mDEBUG\x1b[0m",
            _ => "\x1b[32mINFO \x1b[0m",
        };
        let status = match rng.below(10) {
            0 => "\x1b[38;2;255;95;95m500\x1b[39m",
            1 => "\x1b[38;2;255;175;0m404\x1b[39m",
            _ => "\x1b[38;2;95;215;95m200\x1b[39m",
        };
        out.extend_from_slice(
            format!(
                "\x1b[2m2026-03-12T09:{:02}:{:02}.{:03}Z\x1b[22m {level} \x1b[1;34medge[{}]\x1b[0m: request id=\x1b[33m{i:08x}\x1b[0m path=\x1b[4m/api/v1/items/{}\x1b[24m status={status} bytes={}",
                i / 3600 % 60,
                i % 60,
                i % 1000,
                1000 + i % 97,
                i % 9973,
                i % 65536
            )
            .as_bytes(),
        );
        match rng.below(25) {
            0 => out.extend_from_slice(
                " user=\x1b[36mユーザー登録\x1b[0m result=✅ note=\"日本語のメッセージ\" 🚀"
                    .as_bytes(),
            ),
            1 => {
                // A stack-trace-like line longer than the screen, so it wraps.
                out.extend_from_slice(b" \x1b[38;5;203mtrace=");
                for frame in 0..12 {
                    out.extend_from_slice(
                        format!(
                            "muxus::session::link::pump_{frame}@src/link.rs:{} ",
                            100 + frame
                        )
                        .as_bytes(),
                    );
                }
                out.extend_from_slice(b"\x1b[0m");
            }
            _ => {}
        }
        out.extend_from_slice(b"\r\n");
        i += 1;
    }
    out
}

/// A colored `ls -la` of large directories (GNU ls with LS_COLORS), about 20 MB.
pub fn ls_la() -> Vec<u8> {
    const NAMES: [&str; 12] = [
        "Cargo.toml",
        "README.md",
        "build.rs",
        "main.rs",
        "terminal_view.rs",
        "screenshot.png",
        "archive.tar.gz",
        "notes.txt",
        "id_ed25519.pub",
        "config.yaml",
        "Makefile",
        "LICENSE",
    ];
    let mut out = Vec::with_capacity(21 << 20);
    let mut rng = Lcg::new(11);
    let mut i = 0u32;
    while out.len() < 20 << 20 {
        if i.is_multiple_of(400) {
            out.extend_from_slice(format!("total {}\r\n", 1000 + rng.below(90_000)).as_bytes());
            out.extend_from_slice(
                b"drwxr-xr-x  12 flo  flo      4096 Mar 12 09:00 \x1b[0m\x1b[01;34m.\x1b[0m\r\n",
            );
            out.extend_from_slice(
                b"drwxr-xr-x  48 flo  flo      4096 Mar 11 17:42 \x1b[01;34m..\x1b[0m\r\n",
            );
        }
        let size = rng.below(2_000_000);
        let day = 1 + rng.below(28);
        let (hour, minute) = (rng.below(24), rng.below(60));
        let (mode, colored) = match rng.below(10) {
            0 | 1 => ("drwxr-xr-x", format!("\x1b[01;34mdir_{i:05}\x1b[0m")),
            2 => ("-rwxr-xr-x", format!("\x1b[01;32mrun_{i:05}.sh\x1b[0m")),
            3 => (
                "lrwxrwxrwx",
                format!("\x1b[01;36mlink_{i:05}\x1b[0m -> ../shared/target_{i:05}"),
            ),
            4 => (
                "-rw-r--r--",
                format!("\x1b[01;31mbackup_{i:05}.tar.gz\x1b[0m"),
            ),
            5 => ("-rw-r--r--", format!("\x1b[01;35mimage_{i:05}.png\x1b[0m")),
            _ => (
                "-rw-r--r--",
                format!("{}_{i:05}", NAMES[rng.below(NAMES.len() as u32) as usize]),
            ),
        };
        out.extend_from_slice(
            format!(
                "{mode}  1 flo  flo  {size:>8} Mar {day:2} {hour:02}:{minute:02} {colored}\r\n"
            )
            .as_bytes(),
        );
        i += 1;
    }
    out
}

/// Full-screen redraws as `htop` sends them on a `cols`×`rows` screen: the alternate screen,
/// cursor addressing per row, colored meters, a full-width header, a highlighted row and the
/// function-key bar, about 20 MB of frames.
pub fn htop(cols: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(21 << 20);
    let mut rng = Lcg::new(13);
    out.extend_from_slice(
        b"\x1b[?1049h\x1b[22;0;0t\x1b[1;50r\x1b(B\x1b[m\x1b[4l\x1b[?7h\x1b[?25l\x1b[H\x1b[2J",
    );
    let mut frame = 0u32;
    while out.len() < 20 << 20 {
        // CPU meters, two columns of four.
        let half = cols / 2 - 2;
        for cpu in 0..8 {
            let row = cpu / 2 + 1;
            let col = if cpu % 2 == 0 { 1 } else { half + 3 };
            let busy = rng.below(100) as usize;
            let bar = half.saturating_sub(12);
            let user = bar * busy / 200;
            let system = bar * busy / 400;
            out.extend_from_slice(
                format!(
                    "\x1b[{row};{col}H\x1b[36m{cpu:>3}\x1b[39m\x1b[1m[\x1b[0;32m{}\x1b[31m{}\x1b[90m{}\x1b[39m{busy:>5}.{}%\x1b[1m]\x1b[m",
                    "|".repeat(user),
                    "|".repeat(system),
                    " ".repeat(bar - user - system),
                    rng.below(10)
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(
            format!(
                "\x1b[5;1H\x1b[36m  Mem\x1b[39m\x1b[1m[\x1b[0;32m{}\x1b[34m{}\x1b[33m{}\x1b[90m{}\x1b[39m7.12G/31.2G\x1b[1m]\x1b[m\x1b[K",
                "|".repeat(40),
                "|".repeat(6),
                "|".repeat(20),
                " ".repeat(half.saturating_sub(80))
            )
            .as_bytes(),
        );
        out.extend_from_slice(
            format!(
                "\x1b[5;{}H\x1b[36mTasks: \x1b[1m{}\x1b[0;36m, \x1b[1;32m{}\x1b[0;36m running  Load average: \x1b[1m{}.{:02} \x1b[0;36m0.87 0.54  Uptime: \x1b[1m2 days, 04:{:02}:{:02}\x1b[m\x1b[K",
                half + 3,
                300 + rng.below(50),
                1 + rng.below(4),
                rng.below(4),
                rng.below(100),
                frame / 60 % 60,
                frame % 60
            )
            .as_bytes(),
        );
        // The header row.
        let header = "    PID USER       PRI  NI  VIRT   RES   SHR S  CPU% MEM%   TIME+  Command";
        out.extend_from_slice(
            format!(
                "\x1b[7;1H\x1b[30;42m{header}{}\x1b[m",
                " ".repeat(cols.saturating_sub(header.len()))
            )
            .as_bytes(),
        );
        let selected = 8 + (frame as usize % (rows - 9));
        for row in 8..rows {
            let pid = 1000 + rng.below(60_000);
            let cpu = rng.below(1000);
            let line = format!(
                "{pid:>7} flo        20   0 {:>4}M {:>4}M {:>5} S {:>3}.{} {:>3}.{} {:>2}:{:02}.{:02} /usr/lib/firefox/firefox -contentproc -childID {} -isForBrowser -prefsLen 31337 tab",
                rng.below(9000),
                rng.below(900),
                rng.below(99_999),
                cpu / 10,
                cpu % 10,
                rng.below(10),
                rng.below(10),
                rng.below(60),
                rng.below(60),
                rng.below(100),
                rng.below(400)
            );
            let line: String = line.chars().take(cols).collect();
            let pad = cols - line.chars().count();
            if row == selected {
                out.extend_from_slice(
                    format!("\x1b[{row};1H\x1b[30;46m{line}{}\x1b[m", " ".repeat(pad)).as_bytes(),
                );
            } else {
                // htop colors the user and the numbers, brightens the program and dims its
                // arguments.
                let split = line.find(" /usr").unwrap_or(line.len());
                let (head, tail) = line.split_at(split);
                let (pid, rest) = head.split_at(8.min(head.len()));
                let (program, args) = tail.split_at(tail.find(" -").unwrap_or(tail.len()));
                out.extend_from_slice(
                    format!(
                        "\x1b[{row};1H\x1b[m{pid}\x1b[36m{rest}\x1b[1;37m{program}\x1b[0;90m{args}\x1b[m\x1b[K"
                    )
                    .as_bytes(),
                );
            }
        }
        // The function-key bar.
        out.extend_from_slice(format!("\x1b[{rows};1H").as_bytes());
        for (key, label) in [
            ("F1", "Help  "),
            ("F2", "Setup "),
            ("F3", "Search"),
            ("F4", "Filter"),
            ("F5", "Tree  "),
            ("F6", "SortBy"),
            ("F7", "Nice -"),
            ("F8", "Nice +"),
            ("F9", "Kill  "),
            ("F10", "Quit  "),
        ] {
            out.extend_from_slice(format!("\x1b[m{key}\x1b[30;46m{label}").as_bytes());
        }
        out.extend_from_slice(b"\x1b[K\x1b[m");
        frame += 1;
    }
    out.extend_from_slice(b"\x1b[?25h\x1b[?1049l");
    out
}

/// Scrolling through a source file in `vim` on a `cols`×`rows` screen: syntax-colored lines with
/// line numbers, scrolls through a scroll region (DECSTBM and line feed, or delete line), the
/// status line updated every step, and a full redraw now and then. About 20 MB.
pub fn vim(cols: usize, rows: usize) -> Vec<u8> {
    const WORDS: [&str; 10] = [
        "self", "buffer", "line", "cells", "width", "cursor", "grid", "row", "col", "text",
    ];
    fn source_line(n: usize, cols: usize, rng: &mut Lcg) -> Vec<u8> {
        let mut line = format!("\x1b[33m{n:>5} \x1b[m");
        let mut width = 6usize;
        let indent = 4 * (rng.below(4) as usize);
        line.push_str(&" ".repeat(indent));
        width += indent;
        match rng.below(6) {
            0 => {
                let text = format!(
                    "// {} the {} before the {} moves",
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize]
                );
                width += text.len();
                line.push_str(&format!("\x1b[38;5;245m{text}\x1b[m"));
            }
            1 => {
                let text = format!(
                    "let {} = self.{}(\"{}\");",
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize]
                );
                width += text.len();
                line.push_str(&format!(
                    "\x1b[38;5;130mlet\x1b[m {}",
                    text[4..].replace('"', "\x1b[38;5;64m\"")
                ));
                line.push_str("\x1b[m");
            }
            2 => {
                let text = format!(
                    "pub fn {}(&mut self, {}: usize) -> Option<{}> {{",
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize],
                    "Cell"
                );
                width += text.len();
                line.push_str(&format!(
                    "\x1b[38;5;130mpub fn\x1b[m \x1b[38;5;32m{}\x1b[m",
                    &text[7..]
                ));
            }
            _ => {
                let text = format!(
                    "if {}.{} > {} {{ return {}; }}",
                    WORDS[rng.below(10) as usize],
                    WORDS[rng.below(10) as usize],
                    rng.below(1000),
                    WORDS[rng.below(10) as usize]
                );
                width += text.len();
                line.push_str(&format!(
                    "\x1b[38;5;130mif\x1b[m {}\x1b[38;5;130m",
                    &text[3..]
                ));
                line.push_str("\x1b[m");
            }
        }
        // The lines stay well inside the screen, so nothing wraps.
        debug_assert!(width < cols);
        line.push_str("\x1b[K");
        line.into_bytes()
    }
    let text_rows = rows - 1;
    let mut out = Vec::with_capacity(21 << 20);
    let mut rng = Lcg::new(17);
    out.extend_from_slice(b"\x1b[?1049h\x1b[22;0;0t\x1b[>4;2m\x1b[?1h\x1b=\x1b[H\x1b[2J");
    let mut top = 1usize;
    let mut step = 0u32;
    let redraw = |out: &mut Vec<u8>, top: usize, rng: &mut Lcg| {
        out.extend_from_slice(b"\x1b[?25l\x1b[H\x1b[2J");
        for row in 0..text_rows {
            out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
            out.extend_from_slice(&source_line(top + row, cols, rng));
        }
    };
    redraw(&mut out, top, &mut rng);
    while out.len() < 20 << 20 {
        out.extend_from_slice(b"\x1b[?25l");
        if step % 64 == 63 {
            redraw(&mut out, top, &mut rng);
        } else if step.is_multiple_of(2) {
            // Scroll the text area up one line through the scroll region.
            out.extend_from_slice(
                format!("\x1b[1;{text_rows}r\x1b[{text_rows};1H\n\x1b[r").as_bytes(),
            );
            top += 1;
            out.extend_from_slice(format!("\x1b[{text_rows};1H").as_bytes());
            out.extend_from_slice(&source_line(top + text_rows - 1, cols, &mut rng));
        } else {
            // Delete the top line and draw the new last line.
            out.extend_from_slice(format!("\x1b[1;{text_rows}r\x1b[1;1H\x1b[M\x1b[r").as_bytes());
            top += 1;
            out.extend_from_slice(format!("\x1b[{text_rows};1H").as_bytes());
            out.extend_from_slice(&source_line(top + text_rows - 1, cols, &mut rng));
        }
        out.extend_from_slice(
            format!(
                "\x1b[{rows};1H\x1b[7msrc/terminal.rs [+]\x1b[27m{}{},{}{}All\x1b[{};5H\x1b[?25h",
                " ".repeat(cols.saturating_sub(40)),
                top + 10,
                1 + rng.below(80),
                " ".repeat(8),
                1 + rng.below(text_rows as u32)
            )
            .as_bytes(),
        );
        step += 1;
    }
    out.extend_from_slice(b"\x1b[?1049l");
    out
}
