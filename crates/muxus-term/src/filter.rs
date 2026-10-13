//! A streaming pre-parser in front of the emulator. It finds the few sequences Muxus handles itself
//! — OSC 7, 10, 11, 12, 52, 133 and 633, the mouse modes, erase-in-display and resets — and
//! rewrites the two SGR colors the emulator cannot tell apart from the default colors (ANSI white
//! foreground and ANSI black background). Everything else passes through unchanged, split at the
//! points where a handled sequence sits so the terminal state is right when it is handled.
//!
//! Sequences may be split across reads; the filter keeps the partial sequence until it completes.

/// Truecolor stand-ins for SGR 37 and SGR 40 (and their 256-color spellings). The emulator stores
/// the default colors as ANSI white and black; these keep explicitly chosen white and black apart
/// so they follow the palette instead of the theme's default colors.
pub const ANSI_WHITE_FG_SENTINEL: (u8, u8, u8) = (1, 2, 7);
pub const ANSI_BLACK_BG_SENTINEL: (u8, u8, u8) = (1, 2, 0);
/// The emulator's default foreground: written cells carry it, blank cells carry ANSI white, which
/// is how blank cells and written spaces are told apart (xterm's "has content").
pub const DEFAULT_FG_SENTINEL: (u8, u8, u8) = (1, 2, 3);

/// One piece of filtered output, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Bytes for the emulator.
    Bytes(Vec<u8>),
    /// An OSC sequence Muxus handles: its number and payload (after `<number>;`).
    Osc { code: u32, data: String },
    /// `CSI 2 J`: clear the screen (scrolled into scrollback by the caller when it can).
    EraseDisplay,
    /// A full reset (`ESC c`) or soft reset (`CSI ! p`) was passed on; reinitialize the pen.
    Reset { hard: bool },
    /// DECSET (`set`) or DECRST of private modes, when one of them is a mode Muxus tracks
    /// ([`TRACKED_PRIVATE_MODES`]); also passed on to the emulator. Others only pass on.
    PrivateModes { modes: Vec<u16>, set: bool },
    /// DECSCUSR (`CSI Ps SP q`): the program picks a cursor shape; also passed on.
    CursorStyle(u16),
    /// XTWINOPS (`CSI Ps ; … t`): answered here, never passed on (window reports are off in
    /// xterm except the size queries).
    WindowOp(Vec<u16>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
    /// Reading the OSC number.
    OscCode,
    /// Buffering the payload of an OSC that is handled here.
    OscPayload,
    /// Passing an OSC this filter does not handle.
    OscPass,
    /// Saw ESC inside an OSC: ST if `\` follows.
    OscEscape,
    /// DCS, APC, PM or SOS strings pass through untouched until ST.
    StringPass,
    StringEscape,
}

const HANDLED_OSC: [u32; 7] = [7, 10, 11, 12, 52, 133, 633];
/// The private modes the emulator follows besides the engine: the mouse protocols and encodings,
/// and the alternate screens (which bring the viewport back to the bottom). Full-screen programs
/// toggle others (the cursor, 25) on every redraw; those stay in the byte stream.
pub const TRACKED_PRIVATE_MODES: [u16; 9] = [9, 1000, 1002, 1003, 1006, 1016, 47, 1047, 1049];
/// A CSI sequence longer than this passes through as is instead of being buffered further.
const MAX_CSI: usize = 4096;
/// OSC payloads longer than this are dropped rather than buffered without bound.
const MAX_OSC_PAYLOAD: usize = 1 << 20;

/// The streaming filter. Feed it reads in order.
#[derive(Debug)]
pub struct StreamFilter {
    state: State,
    /// The sequence being read (CSI bytes, OSC prefix) or the payload being buffered.
    pending: Vec<u8>,
    osc_code: u32,
    out: Vec<Segment>,
    bytes: Vec<u8>,
}

impl Default for StreamFilter {
    fn default() -> Self {
        Self {
            state: State::Ground,
            pending: Vec::new(),
            osc_code: 0,
            out: Vec::new(),
            bytes: Vec::new(),
        }
    }
}

impl StreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    fn flush_bytes(&mut self) {
        if !self.bytes.is_empty() {
            self.out
                .push(Segment::Bytes(std::mem::take(&mut self.bytes)));
        }
    }

    fn push(&mut self, segment: Segment) {
        self.flush_bytes();
        self.out.push(segment);
    }

    /// Filter one read; returns the segments that are complete.
    pub fn feed(&mut self, input: &[u8]) -> Vec<Segment> {
        let mut index = 0;
        while index < input.len() {
            // Text and the payloads of strings passed through are copied in runs up to the next
            // byte that can change the state; only those bytes go through `step`.
            let rest = &input[index..];
            let run = match self.state {
                State::Ground => Some(memchr::memchr(0x1b, rest).unwrap_or(rest.len())),
                State::Csi => {
                    // Parameter and intermediate bytes only grow the sequence, up to its cap.
                    let room = &rest[..rest.len().min(MAX_CSI.saturating_sub(self.pending.len()))];
                    Some(
                        room.iter()
                            .position(|byte| !(0x20..=0x3f).contains(byte))
                            .unwrap_or(room.len()),
                    )
                }
                State::StringPass => {
                    Some(memchr::memchr3(0x1b, 0x18, 0x1a, rest).unwrap_or(rest.len()))
                }
                State::OscPass | State::OscPayload => Some(
                    rest.iter()
                        .position(|byte| matches!(byte, 0x07 | 0x1b | 0x18 | 0x1a))
                        .unwrap_or(rest.len()),
                ),
                _ => None,
            };
            if let Some(run) = run
                && run > 0
            {
                self.take_run(&rest[..run]);
                index += run;
                continue;
            }
            self.step(input[index]);
            index += 1;
        }
        // Bytes passed through so far go out now; a partial sequence stays pending.
        self.flush_bytes();
        std::mem::take(&mut self.out)
    }

    /// Bytes that cannot change the state (see `feed`), as `step` would take them one by one.
    fn take_run(&mut self, run: &[u8]) {
        match self.state {
            State::Ground | State::StringPass | State::OscPass => self.bytes.extend_from_slice(run),
            State::Csi => self.pending.extend_from_slice(run),
            State::OscPayload => {
                let room = MAX_OSC_PAYLOAD.saturating_sub(self.pending.len());
                self.pending.extend_from_slice(&run[..run.len().min(room)]);
            }
            _ => run.iter().for_each(|&byte| self.step(byte)),
        }
    }

    fn step(&mut self, byte: u8) {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Escape;
                    self.pending.clear();
                    self.pending.push(byte);
                } else {
                    self.bytes.push(byte);
                }
            }
            State::Escape => match byte {
                b'[' => {
                    self.pending.push(byte);
                    self.state = State::Csi;
                }
                b']' => {
                    self.pending.push(byte);
                    self.osc_code = 0;
                    self.state = State::OscCode;
                }
                b'P' | b'_' | b'^' | b'X' => {
                    self.bytes.extend_from_slice(&self.pending);
                    self.bytes.push(byte);
                    self.pending.clear();
                    self.state = State::StringPass;
                }
                b'c' => {
                    self.bytes.extend_from_slice(b"\x1bc");
                    self.pending.clear();
                    self.state = State::Ground;
                    self.push(Segment::Reset { hard: true });
                }
                0x1b => {
                    // ESC ESC: the first one stands alone.
                    self.bytes.push(0x1b);
                }
                _ => {
                    self.bytes.extend_from_slice(&self.pending);
                    self.bytes.push(byte);
                    self.pending.clear();
                    // Intermediates (ESC ( B, ESC # 8, …) take one more byte, which is ordinary.
                    self.state = State::Ground;
                }
            },
            State::Csi => {
                self.pending.push(byte);
                if (0x40..=0x7e).contains(&byte) {
                    // The buffer goes back afterwards, so its capacity is reused.
                    let sequence = std::mem::take(&mut self.pending);
                    self.state = State::Ground;
                    self.finish_csi(&sequence);
                    self.pending = sequence;
                    self.pending.clear();
                } else if byte == 0x18 || byte == 0x1a || self.pending.len() > MAX_CSI {
                    // CAN/SUB abort the sequence; an absurdly long one passes through as is.
                    let sequence = std::mem::take(&mut self.pending);
                    self.bytes.extend_from_slice(&sequence);
                    self.state = State::Ground;
                } else if byte == 0x1b {
                    // A new escape starts; pass the broken sequence on.
                    self.pending.pop();
                    let sequence = std::mem::take(&mut self.pending);
                    self.bytes.extend_from_slice(&sequence);
                    self.pending.push(0x1b);
                    self.state = State::Escape;
                }
            }
            State::OscCode => {
                if byte.is_ascii_digit() && self.pending.len() < 12 {
                    self.pending.push(byte);
                    self.osc_code = self
                        .osc_code
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                } else if byte == b';'
                    && HANDLED_OSC.contains(&self.osc_code)
                    && self.pending.len() > 2
                {
                    self.pending.clear();
                    self.state = State::OscPayload;
                } else {
                    // Not ours: pass what was read and the rest of the sequence.
                    self.bytes.extend_from_slice(&self.pending);
                    self.pending.clear();
                    self.state = State::OscPass;
                    self.step_osc_pass(byte);
                }
            }
            State::OscPayload => match byte {
                0x07 => self.finish_osc(),
                0x1b => self.state = State::OscEscape,
                0x18 | 0x1a => {
                    self.pending.clear();
                    self.state = State::Ground;
                }
                _ => {
                    if self.pending.len() < MAX_OSC_PAYLOAD {
                        self.pending.push(byte);
                    }
                }
            },
            State::OscEscape => {
                if byte == b'\\' {
                    self.finish_osc();
                } else {
                    // ESC ends the string and starts a new sequence.
                    self.finish_osc();
                    self.state = State::Escape;
                    self.pending.clear();
                    self.pending.push(0x1b);
                    self.step(byte);
                }
            }
            State::OscPass => self.step_osc_pass(byte),
            State::StringPass => {
                self.bytes.push(byte);
                match byte {
                    0x1b => self.state = State::StringEscape,
                    0x18 | 0x1a => self.state = State::Ground,
                    _ => {}
                }
            }
            State::StringEscape => {
                self.bytes.push(byte);
                self.state = if byte == b'\\' {
                    State::Ground
                } else {
                    State::StringPass
                };
            }
        }
    }

    /// An OSC passed through ends with BEL or, like a string, with ST.
    fn step_osc_pass(&mut self, byte: u8) {
        self.bytes.push(byte);
        match byte {
            0x07 | 0x18 | 0x1a => self.state = State::Ground,
            0x1b => self.state = State::StringEscape,
            _ => {}
        }
    }

    fn finish_osc(&mut self) {
        let payload = std::mem::take(&mut self.pending);
        self.state = State::Ground;
        let data = String::from_utf8_lossy(&payload).into_owned();
        self.push(Segment::Osc {
            code: self.osc_code,
            data,
        });
    }

    fn finish_csi(&mut self, sequence: &[u8]) {
        // sequence = ESC [ params… final
        let body = &sequence[2..sequence.len() - 1];
        let final_byte = *sequence.last().expect("final byte");
        if body.iter().any(|b| *b < 0x20) {
            // Embedded controls: leave the sequence to the emulator.
            self.bytes.extend_from_slice(sequence);
            return;
        }
        let private = body
            .first()
            .copied()
            .filter(|b| matches!(b, b'?' | b'>' | b'<' | b'='));
        let intermediates = body.iter().any(|b| (0x20..=0x2f).contains(b));
        match (private, intermediates, final_byte) {
            (None, false, b'm') => {
                if sgr_needs_rewrite(body) {
                    let rewritten = rewrite_sgr(body);
                    self.bytes.extend_from_slice(&rewritten);
                } else {
                    self.bytes.extend_from_slice(sequence);
                }
            }
            (None, false, b'J') if parse_first_param(body) == Some(2) => {
                self.push(Segment::EraseDisplay)
            }
            (Some(b'?'), false, b'h' | b'l') => {
                self.bytes.extend_from_slice(sequence);
                let modes: Vec<u16> = body[1..]
                    .split(|b| *b == b';')
                    .filter_map(|p| std::str::from_utf8(p).ok()?.parse::<u16>().ok())
                    .collect();
                if modes
                    .iter()
                    .any(|mode| TRACKED_PRIVATE_MODES.contains(mode))
                {
                    self.push(Segment::PrivateModes {
                        modes,
                        set: final_byte == b'h',
                    });
                }
            }
            (None, true, b'q') if body.last() == Some(&b' ') => {
                self.bytes.extend_from_slice(sequence);
                let style = parse_first_param(&body[..body.len() - 1]).unwrap_or(0);
                self.push(Segment::CursorStyle(style.min(u32::from(u16::MAX)) as u16));
            }
            (None, false, b't') => {
                let params = body
                    .split(|b| *b == b';')
                    .map(|p| {
                        std::str::from_utf8(p)
                            .ok()
                            .and_then(|p| p.parse::<u16>().ok())
                            .unwrap_or(0)
                    })
                    .collect();
                self.push(Segment::WindowOp(params));
            }
            (None, true, b'p') if body == b"!" => {
                self.bytes.extend_from_slice(sequence);
                self.push(Segment::Reset { hard: false });
            }
            _ => self.bytes.extend_from_slice(sequence),
        }
    }
}

fn parse_first_param(body: &[u8]) -> Option<u32> {
    let first = body.split(|b| *b == b';').next()?;
    if first.is_empty() {
        return Some(0);
    }
    std::str::from_utf8(first).ok()?.parse().ok()
}

/// Whether `rewrite_sgr` would change an SGR body: the same walk over its parameters, without
/// allocating, since nearly every SGR passes through unchanged.
fn sgr_needs_rewrite(body: &[u8]) -> bool {
    let mut groups = body.split(|b| *b == b';');
    while let Some(group) = groups.next() {
        if group.contains(&b':') {
            let mut parts = group.split(|b| *b == b':');
            let (first, mode, value, extra) =
                (parts.next(), parts.next(), parts.next(), parts.next());
            if extra.is_none()
                && mode == Some(b"5")
                && matches!(
                    (first, value),
                    (Some(b"38"), Some(b"7")) | (Some(b"48"), Some(b"0"))
                )
            {
                return true;
            }
            continue;
        }
        match group {
            b"37" | b"40" => return true,
            b"38" | b"48" | b"58" => {
                let mut ahead = groups.clone();
                let (mode, value) = (ahead.next(), ahead.next());
                // `5;n` and `2;r;g;b` are arguments, not codes; anything else is read as a code.
                let arguments = match mode {
                    Some(b"5") => 2,
                    Some(b"2") => 4,
                    _ => 0,
                };
                if arguments == 2
                    && ((group == b"38" && value == Some(b"7"))
                        || (group == b"48" && value == Some(b"0")))
                {
                    return true;
                }
                for _ in 0..arguments {
                    groups.next();
                }
            }
            _ => {}
        }
    }
    false
}

/// Rewrite SGR 37, 40, 38;5;7 and 48;5;0 (in either the `;` or `:` form) to their truecolor
/// stand-ins; everything else is kept byte for byte.
fn rewrite_sgr(body: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(body);
    let groups: Vec<&str> = text.split(';').collect();
    let mut out: Vec<String> = Vec::with_capacity(groups.len());
    let white = format!(
        "38;2;{};{};{}",
        ANSI_WHITE_FG_SENTINEL.0, ANSI_WHITE_FG_SENTINEL.1, ANSI_WHITE_FG_SENTINEL.2
    );
    let black = format!(
        "48;2;{};{};{}",
        ANSI_BLACK_BG_SENTINEL.0, ANSI_BLACK_BG_SENTINEL.1, ANSI_BLACK_BG_SENTINEL.2
    );
    let mut index = 0;
    let mut changed = false;
    while index < groups.len() {
        let group = groups[index];
        if group.contains(':') {
            // Colon sub-parameters carry their own arguments.
            let parts: Vec<&str> = group.split(':').collect();
            match (parts[0], parts.get(1).copied(), parts.get(2).copied()) {
                ("38", Some("5"), Some("7")) if parts.len() == 3 => {
                    out.push(white.clone());
                    changed = true;
                }
                ("48", Some("5"), Some("0")) if parts.len() == 3 => {
                    out.push(black.clone());
                    changed = true;
                }
                _ => out.push(group.to_string()),
            }
            index += 1;
            continue;
        }
        match group {
            "37" => {
                out.push(white.clone());
                changed = true;
                index += 1;
            }
            "40" => {
                out.push(black.clone());
                changed = true;
                index += 1;
            }
            "38" | "48" | "58" => {
                let mode = groups.get(index + 1).copied();
                let take = match mode {
                    Some("5") => 3,
                    Some("2") => 5,
                    _ => 1,
                };
                if take == 3 && groups.get(index + 2).is_some() {
                    let value = groups[index + 2];
                    if group == "38" && value == "7" {
                        out.push(white.clone());
                        changed = true;
                        index += 3;
                        continue;
                    }
                    if group == "48" && value == "0" {
                        out.push(black.clone());
                        changed = true;
                        index += 3;
                        continue;
                    }
                }
                let end = (index + take).min(groups.len());
                out.extend(groups[index..end].iter().map(|g| g.to_string()));
                index = end;
            }
            _ => {
                out.push(group.to_string());
                index += 1;
            }
        }
    }
    let mut sequence = b"\x1b[".to_vec();
    if changed {
        sequence.extend_from_slice(out.join(";").as_bytes());
    } else {
        sequence.extend_from_slice(body);
    }
    sequence.push(b'm');
    sequence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> Vec<Segment> {
        let mut filter = StreamFilter::new();
        let mut segments = Vec::new();
        for chunk in chunks {
            segments.extend(filter.feed(chunk));
        }
        // Merge adjacent byte segments for easier assertions.
        let mut merged: Vec<Segment> = Vec::new();
        for segment in segments {
            match (merged.last_mut(), segment) {
                (Some(Segment::Bytes(previous)), Segment::Bytes(next)) => previous.extend(next),
                (_, segment) => merged.push(segment),
            }
        }
        merged
    }

    fn bytes(text: &str) -> Segment {
        Segment::Bytes(text.as_bytes().to_vec())
    }

    #[test]
    fn passes_ordinary_output_through() {
        assert_eq!(
            run(&[b"hello \x1b[1;32mworld\x1b[0m\r\n"]),
            [bytes("hello \x1b[1;32mworld\x1b[0m\r\n")]
        );
    }

    #[test]
    fn extracts_handled_osc_sequences_across_reads() {
        assert_eq!(
            run(&[b"a\x1b]133;D", b";1\x07b\x1b]7;file:///tmp\x1b\\c"]),
            [
                bytes("a"),
                Segment::Osc {
                    code: 133,
                    data: "D;1".into()
                },
                bytes("b"),
                Segment::Osc {
                    code: 7,
                    data: "file:///tmp".into()
                },
                bytes("c"),
            ]
        );
    }

    #[test]
    fn passes_other_osc_and_string_sequences_untouched() {
        let title = "\x1b]0;my title\x07x";
        assert_eq!(run(&[title.as_bytes()]), [bytes(title)]);
        let kitty = "\x1b_Ga=T,f=100;iVBORw0KGgo=\x1b\\y";
        assert_eq!(
            run(&[&kitty.as_bytes()[..10], &kitty.as_bytes()[10..]]),
            [bytes(kitty)]
        );
        let hyperlink = "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(run(&[hyperlink.as_bytes()]), [bytes(hyperlink)]);
    }

    #[test]
    fn rewrites_explicit_white_and_black() {
        assert_eq!(
            run(&[b"\x1b[1;37;40m"]),
            [bytes("\x1b[1;38;2;1;2;7;48;2;1;2;0m")]
        );
        assert_eq!(
            run(&[b"\x1b[38;5;7m\x1b[48:5:0m"]),
            [bytes("\x1b[38;2;1;2;7m\x1b[48;2;1;2;0m")]
        );
        // Values inside extended colors are not SGR codes.
        assert_eq!(
            run(&[b"\x1b[38;2;37;40;37m\x1b[38;5;40m"]),
            [bytes("\x1b[38;2;37;40;37m\x1b[38;5;40m")]
        );
        assert_eq!(run(&[b"\x1b[>4;1m"]), [bytes("\x1b[>4;1m")]);
    }

    #[test]
    fn reports_erase_display_resets_and_private_modes() {
        assert_eq!(
            run(&[b"x\x1b[2Jy"]),
            [bytes("x"), Segment::EraseDisplay, bytes("y")]
        );
        assert_eq!(run(&[b"\x1b[J\x1b[3J"]), [bytes("\x1b[J\x1b[3J")]);
        assert_eq!(
            run(&[b"\x1bc"]),
            [bytes("\x1bc"), Segment::Reset { hard: true }]
        );
        assert_eq!(
            run(&[b"\x1b[!p"]),
            [bytes("\x1b[!p"), Segment::Reset { hard: false }]
        );
        assert_eq!(
            run(&[b"\x1b[5 q"]),
            [bytes("\x1b[5 q"), Segment::CursorStyle(5)]
        );
        assert_eq!(
            run(&[b"\x1b[14t\x1b[21t"]),
            [Segment::WindowOp(vec![14]), Segment::WindowOp(vec![21])]
        );
        assert_eq!(
            run(&[b"\x1b[?1000;1006h"]),
            [
                bytes("\x1b[?1000;1006h"),
                Segment::PrivateModes {
                    modes: vec![1000, 1006],
                    set: true
                }
            ]
        );
    }

    #[test]
    fn the_rewrite_check_agrees_with_the_rewrite() {
        for body in [
            "",
            "0",
            "1;37",
            "40",
            "37;40",
            "1;32",
            "38;2;37;40;37",
            "38;5;40",
            "38;5;7",
            "48;5;0",
            "48;5;7",
            "58;5;7",
            "38;5",
            "38",
            "38:5:7",
            "48:5:0",
            "48:5:0:1",
            "38:2:1:2:3",
            "4:3",
            "37:1",
            "2;38;2;255;95;95;4;24",
            "0;38;5;244;48;5;0",
            "x;37",
        ] {
            let unchanged = [b"\x1b[", body.as_bytes(), b"m"].concat();
            assert_eq!(
                sgr_needs_rewrite(body.as_bytes()),
                rewrite_sgr(body.as_bytes()) != unchanged,
                "`{body}`"
            );
        }
        // And on random parameter lists made of the codes that matter.
        const TOKENS: [&str; 12] = [
            "0", "1", "2", "5", "7", "37", "38", "40", "48", "58", "255", "",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for _ in 0..5000 {
            let count = 1 + next(8);
            let mut body = String::new();
            for index in 0..count {
                if index > 0 {
                    body.push(if next(6) == 0 { ':' } else { ';' });
                }
                body.push_str(TOKENS[next(TOKENS.len())]);
            }
            let unchanged = [b"\x1b[", body.as_bytes(), b"m"].concat();
            assert_eq!(
                sgr_needs_rewrite(body.as_bytes()),
                rewrite_sgr(body.as_bytes()) != unchanged,
                "`{body}`"
            );
        }
    }

    #[test]
    fn passes_long_strings_through_across_reads() {
        let payload = "A".repeat(10_000);
        let sixel = format!("\x1bPq#0;2;0;0;0{payload}\x1b\\z");
        let (head, tail) = sixel.as_bytes().split_at(4_000);
        assert_eq!(run(&[head, tail]), [bytes(&sixel)]);
        let osc = format!("\x1b]7;file://host/{payload}\x07z");
        let (head, tail) = osc.as_bytes().split_at(4_000);
        assert_eq!(
            run(&[head, tail]),
            [
                Segment::Osc {
                    code: 7,
                    data: format!("file://host/{payload}")
                },
                bytes("z")
            ]
        );
    }

    /// The filter as it was before runs were copied in bulk: one `step` per byte.
    fn run_bytewise(chunks: &[&[u8]]) -> Vec<Segment> {
        let mut filter = StreamFilter::new();
        let mut segments = Vec::new();
        for chunk in chunks {
            for &byte in *chunk {
                filter.step(byte);
            }
            filter.flush_bytes();
            segments.extend(std::mem::take(&mut filter.out));
        }
        let mut merged: Vec<Segment> = Vec::new();
        for segment in segments {
            match (merged.last_mut(), segment) {
                (Some(Segment::Bytes(previous)), Segment::Bytes(next)) => previous.extend(next),
                (_, segment) => merged.push(segment),
            }
        }
        merged
    }

    #[test]
    fn bulk_runs_match_stepping_byte_by_byte() {
        const ALPHABET: &[u8] =
            b"\x1b\x1b\x1b[[]]??;;:0123457mhlJqtp!c P_X^\\\x07\x18\x1a\r\nab\xe3\x81\x82";
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for _ in 0..3000 {
            let len = next(200);
            let input: Vec<u8> = (0..len).map(|_| ALPHABET[next(ALPHABET.len())]).collect();
            let cut = next(len + 1);
            let (head, tail) = input.split_at(cut);
            assert_eq!(
                run(&[head, tail]),
                run_bytewise(&[head, tail]),
                "{:?}",
                String::from_utf8_lossy(&input)
            );
        }
    }

    #[test]
    fn caps_csi_sequences_split_anywhere() {
        let long = format!("\x1b[{}mx", "1;".repeat(3000));
        for cut in [1, 2, 3, 100, 4095, 4096, 4097, 5000] {
            let (head, tail) = long.as_bytes().split_at(cut);
            assert_eq!(run(&[head, tail]), [bytes(&long)], "cut at {cut}");
        }
        let sgr = "\x1b[1;37;4mx";
        for cut in 1..sgr.len() {
            let (head, tail) = sgr.as_bytes().split_at(cut);
            assert_eq!(
                run(&[head, tail]),
                [bytes("\x1b[1;38;2;1;2;7;4mx")],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn only_tracked_private_modes_split_the_output() {
        assert_eq!(
            run(&[b"a\x1b[?25lb\x1b[?2004hc\x1b[?1049;25hd"]),
            [
                bytes("a\x1b[?25lb\x1b[?2004hc\x1b[?1049;25h"),
                Segment::PrivateModes {
                    modes: vec![1049, 25],
                    set: true
                },
                bytes("d"),
            ]
        );
    }

    #[test]
    fn keeps_a_split_csi_until_it_completes() {
        assert_eq!(run(&[b"\x1b[3", b"7mA"]), [bytes("\x1b[38;2;1;2;7mA")]);
    }
}
