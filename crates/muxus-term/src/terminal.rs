//! The terminal emulator: `par-term-emu-core-rust` behind the Muxus stream filter, with the
//! behaviour Muxus configures xterm.js for — Unicode 11 widths, ED2 scrolling the screen into the
//! scrollback (`scrollOnEraseInDisplay`), OSC 52 writes without reads, shell-integration marks and
//! current-directory tracking, kitty keyboard and graphics, and xterm's window-size reports.
//!
//! The emulator is UI-free: feed it output with [`Emulator::write`], read what it wants to tell the
//! session and the app with [`Emulator::take_events`], and paint it through the [`TextBuffer`] and
//! [`Emulator::styled_line`] views.

use std::collections::HashMap;

use par_term_emu_core_rust::cell::{Cell, UnderlineStyle as CoreUnderline};
use par_term_emu_core_rust::color::{Color, NamedColor};
use par_term_emu_core_rust::cursor::CursorStyle as CoreCursorStyle;
use par_term_emu_core_rust::graphics::TerminalGraphic;
use par_term_emu_core_rust::terminal::Terminal as Core;

use crate::buffer::{CellText, TextBuffer};
use crate::color::{Rgba, parse_css_color};
use crate::filter::{
    ANSI_BLACK_BG_SENTINEL, ANSI_WHITE_FG_SENTINEL, DEFAULT_FG_SENTINEL, Segment, StreamFilter,
};
use crate::mouse::{MouseEncoding, MouseProtocol};
use crate::osc52::{Osc52Action, handle_osc52};
use crate::palette::{ResolvedPalette, TerminalTheme};
use crate::shell_integration::{CommandMarkerHost, CommandTracker, CwdTracker};
use crate::style::{
    CellColor, CellStyle, ColorOverrides, EffectiveColors, StyledCell, UnderlineStyle,
};

/// What the emulator tells its owner after processing output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmulatorEvent {
    /// Bytes the terminal answers the program with (device reports, color and clipboard queries).
    /// They go back to the same transport and are never mirrored to other sessions.
    Reply(Vec<u8>),
    /// The window title changed (OSC 0/2).
    TitleChanged(String),
    Bell,
    /// The shell reported a new working directory (OSC 7, or OSC 133/633 `P;Cwd=`).
    CwdChanged(String),
    /// A program wrote the clipboard through OSC 52 (only when allowed).
    ClipboardWrite(String),
}

/// The cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// The cursor as it should be painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Buffer line (scrollback first).
    pub line: usize,
    pub col: usize,
    pub visible: bool,
    /// The program's choice (DECSCUSR); `None` follows the preference.
    pub shape: Option<CursorShape>,
    /// The program's blink choice; `None` follows the preference.
    pub blink: Option<bool>,
}

/// An image on a buffer line.
#[derive(Debug, Clone)]
pub struct ImagePlacement {
    pub id: u64,
    /// Buffer line of the image's top edge (may be above the buffer when partly trimmed).
    pub line: isize,
    pub col: usize,
    /// RGBA pixels.
    pub pixels: std::sync::Arc<Vec<u8>>,
    pub width: usize,
    pub height: usize,
    /// Cells the image is stretched over when the program sized it in cells.
    pub cells: Option<(usize, usize)>,
    pub z_index: i32,
}

/// Settings a terminal is created with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmulatorConfig {
    pub cols: u16,
    pub rows: u16,
    /// Lines of scrollback.
    pub scrollback: usize,
}

impl Default for EmulatorConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            scrollback: 10_000,
        }
    }
}

const fn rgb(value: (u8, u8, u8)) -> Color {
    Color::Rgb(value.0, value.1, value.2)
}

/// The cell color a stored color stands for.
fn fg_color(color: Color) -> CellColor {
    match color {
        Color::Named(NamedColor::White) => CellColor::Default,
        Color::Rgb(r, g, b) if (r, g, b) == DEFAULT_FG_SENTINEL => CellColor::Default,
        Color::Rgb(r, g, b) if (r, g, b) == ANSI_WHITE_FG_SENTINEL => CellColor::Palette(7),
        other => plain_color(other),
    }
}

fn bg_color(color: Color) -> CellColor {
    match color {
        Color::Named(NamedColor::Black) => CellColor::Default,
        Color::Rgb(r, g, b) if (r, g, b) == ANSI_BLACK_BG_SENTINEL => CellColor::Palette(0),
        other => plain_color(other),
    }
}

fn plain_color(color: Color) -> CellColor {
    match color {
        Color::Named(named) => CellColor::Palette(named as u8),
        Color::Indexed(index) => CellColor::Palette(index),
        Color::Rgb(r, g, b) => CellColor::Rgb(r, g, b),
    }
}

/// Whether a program wrote this cell. Blank and erased cells keep the emulator's built-in
/// default foreground, which the filter's pen never uses.
fn has_content(cell: &Cell) -> bool {
    !(cell.c == ' ' && cell.fg() == Color::Named(NamedColor::White) && cell.combining().is_empty())
        && cell.c != '\0'
}

fn cell_text(cell: &Cell) -> CellText {
    if cell.flags.wide_char_spacer() {
        return CellText::new("", 0);
    }
    let width = if cell.flags.wide_char() { 2 } else { 1 };
    if has_content(cell) {
        CellText::new(cell.get_grapheme(), width)
    } else {
        CellText::new("", width)
    }
}

fn styled_cell(cell: &Cell) -> StyledCell {
    let text = cell_text(cell);
    let flags = &cell.flags;
    let underline = if flags.underline() {
        match flags.underline_style {
            CoreUnderline::None | CoreUnderline::Straight => UnderlineStyle::Single,
            CoreUnderline::Double => UnderlineStyle::Double,
            CoreUnderline::Curly => UnderlineStyle::Curly,
            CoreUnderline::Dotted => UnderlineStyle::Dotted,
            CoreUnderline::Dashed => UnderlineStyle::Dashed,
        }
    } else {
        UnderlineStyle::None
    };
    StyledCell {
        text: text.text,
        width: text.width,
        style: CellStyle {
            fg: fg_color(cell.fg()),
            bg: bg_color(cell.bg()),
            underline_color: cell.underline_color().map(fg_color),
            underline,
            bold: flags.bold(),
            dim: flags.dim(),
            italic: flags.italic(),
            blink: flags.blink(),
            inverse: flags.reverse(),
            invisible: flags.hidden(),
            strikethrough: flags.strikethrough(),
            overline: flags.overline(),
        },
        hyperlink: flags.hyperlink_id.map(|id| id.get()),
    }
}

/// Marks for shell-integration commands, by absolute line.
struct Marks<'a> {
    absolute_cursor_line: u64,
    first_absolute_line: u64,
    failed: &'a mut Vec<u64>,
}

impl CommandMarkerHost for Marks<'_> {
    type Marker = u64;

    fn register_marker(&mut self) -> Option<u64> {
        Some(self.absolute_cursor_line)
    }

    fn is_marker_disposed(&self, marker: &u64) -> bool {
        *marker < self.first_absolute_line
    }

    fn dispose_marker(&mut self, _marker: u64) {}

    fn mark_failed_command(&mut self, marker: u64) {
        if !self.failed.contains(&marker) {
            self.failed.push(marker);
        }
    }
}

/// One terminal.
pub struct Emulator {
    core: Core,
    filter: StreamFilter,
    events: Vec<EmulatorEvent>,
    cwd: CwdTracker,
    commands: CommandTracker<u64>,
    failed_commands: Vec<u64>,
    mouse_protocol: MouseProtocol,
    mouse_encoding: MouseEncoding,
    cursor_shape: Option<CursorShape>,
    cursor_blink: Option<bool>,
    overrides: ColorOverrides,
    palette: ResolvedPalette,
    theme: TerminalTheme,
    allow_osc52_write: bool,
    /// Lines the viewport is scrolled up from the bottom.
    display_offset: usize,
    /// Cell size in logical pixels, for the pixel-size reports.
    cell_size: (f32, f32),
    title: String,
    bell_count: u64,
    /// Absolute top line of every image seen on screen, so it stays put in the scrollback.
    image_tops: HashMap<u64, (u64, usize)>,
    scrollback_limit: usize,
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("cols", &self.cols())
            .field("rows", &self.rows())
            .finish_non_exhaustive()
    }
}

impl Emulator {
    pub fn new(config: EmulatorConfig, theme: &TerminalTheme) -> Self {
        let mut core = Core::with_scrollback(
            usize::from(config.cols.max(2)),
            usize::from(config.rows.max(1)),
            config.scrollback,
        );
        core.set_unicode_version(par_term_emu_core_rust::UnicodeVersion::Unicode11);
        core.set_allow_clipboard_read(false);
        // Scrollback lines without their trailing blank cells: a scrolled
        // line then costs in proportion to its text, not the screen width.
        // Readers already pad short lines (scrollback lines keep the width
        // they scrolled off at, so they are short after the screen widens).
        core.set_compact_scrollback(true);
        let mut emulator = Self {
            core,
            filter: StreamFilter::new(),
            events: Vec::new(),
            cwd: CwdTracker::new(),
            commands: CommandTracker::new(),
            failed_commands: Vec::new(),
            mouse_protocol: MouseProtocol::None,
            mouse_encoding: MouseEncoding::Default,
            cursor_shape: None,
            cursor_blink: None,
            overrides: ColorOverrides::default(),
            palette: ResolvedPalette::new(theme),
            theme: theme.clone(),
            allow_osc52_write: true,
            display_offset: 0,
            cell_size: (8.0, 17.0),
            title: String::new(),
            bell_count: 0,
            image_tops: HashMap::new(),
            scrollback_limit: config.scrollback,
        };
        emulator.init_pen();
        emulator.apply_palette();
        emulator
    }

    /// Point the emulator's pen at the default-foreground stand-in, so every written cell is told
    /// apart from a blank one.
    fn init_pen(&mut self) {
        self.core.set_default_fg(rgb(DEFAULT_FG_SENTINEL));
        self.core.process(b"\x1b[0m");
    }

    /// The palette OSC 4 queries report.
    fn apply_palette(&mut self) {
        for index in 0..16 {
            let c = self.palette.ansi[index];
            let _ = self
                .core
                .set_ansi_palette_color(index, Color::Rgb(c.r, c.g, c.b));
        }
    }

    // ---- configuration -------------------------------------------------------------------

    pub fn set_theme(&mut self, theme: &TerminalTheme) {
        if &self.theme == theme {
            return;
        }
        self.theme = theme.clone();
        self.palette = ResolvedPalette::new(theme);
        self.apply_palette();
    }

    pub fn theme(&self) -> &TerminalTheme {
        &self.theme
    }

    /// The theme colors in effect, with what programs set through OSC 10/11/12.
    pub fn colors(&self) -> EffectiveColors {
        EffectiveColors::new(&self.palette, &self.overrides)
    }

    /// Whether programs may write the clipboard through OSC 52 (reads are never allowed).
    pub fn set_allow_osc52_write(&mut self, allow: bool) {
        self.allow_osc52_write = allow;
    }

    /// The cell size in logical pixels, for CSI 14/16 t reports and images.
    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        self.cell_size = (width, height);
        self.core.set_cell_dimensions(
            width.round().max(1.0) as u32,
            height.round().max(1.0) as u32,
        );
        let (cols, rows) = self.core.size();
        self.core.set_pixel_size(
            (width * cols as f32).round() as usize,
            (height * rows as f32).round() as usize,
        );
    }

    pub fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    // ---- geometry ------------------------------------------------------------------------

    pub fn cols(&self) -> usize {
        self.core.size().0
    }

    pub fn rows(&self) -> usize {
        self.core.size().1
    }

    /// Resize the grid (the screen reflows; the scrollback keeps its line widths).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols.max(2)), usize::from(rows.max(1)));
        if (cols, rows) == self.core.size() {
            return;
        }
        let before = self.lines_trimmed();
        self.keep_cursor_on_shorter_screen(rows);
        self.core.resize(cols, rows);
        self.set_cell_size(self.cell_size.0, self.cell_size.1);
        self.after_change(before);
    }

    /// xterm's `Buffer.resize` when the screen gets shorter: the rows below
    /// the cursor go first, then the top rows scroll into the scrollback so
    /// the cursor's line stays on screen. The engine instead cuts the bottom
    /// rows off and clamps the cursor, which drops the output between the
    /// new bottom and the cursor. Scrolling the screen up beforehand (the
    /// whole screen, so the lines reach the scrollback) gives xterm's result;
    /// the engine resets the scroll region on resize anyway. The alternate
    /// screen has no scrollback and its program redraws on SIGWINCH.
    fn keep_cursor_on_shorter_screen(&mut self, rows: usize) {
        if self.core.is_alt_screen_active() {
            return;
        }
        let cursor = *self.core.cursor();
        if cursor.row < rows {
            return;
        }
        let shift = cursor.row + 1 - rows;
        // DECSTBM homes the cursor, so it is placed back explicitly.
        let sequence = format!(
            "\x1b[r\x1b[{shift}S\x1b[{};{}H",
            cursor.row - shift + 1,
            cursor.col + 1
        );
        self.core.process(sequence.as_bytes());
    }

    // ---- input ---------------------------------------------------------------------------

    /// Process program output.
    pub fn write(&mut self, data: &[u8]) {
        let before = self.lines_trimmed() + self.core.active_grid().scrollback_len() as u64;
        let segments = self.filter.feed(data);
        for segment in segments {
            match segment {
                Segment::Bytes(bytes) => self.core.process(&bytes),
                Segment::Osc { code, data } => self.handle_osc(code, &data),
                Segment::EraseDisplay => self.erase_display(),
                Segment::Reset { hard } => {
                    if hard {
                        self.mouse_protocol = MouseProtocol::None;
                        self.mouse_encoding = MouseEncoding::Default;
                        self.cursor_shape = None;
                        self.cursor_blink = None;
                        self.overrides = ColorOverrides::default();
                    }
                    self.init_pen();
                }
                Segment::PrivateModes { modes, set } => self.private_modes(&modes, set),
                Segment::CursorStyle(style) => self.cursor_style(style),
                Segment::WindowOp(params) => self.window_op(&params),
            }
        }
        self.collect_core_events();
        // A viewport scrolled back stays on its lines while output arrives (xterm's
        // isUserScrolling); at the bottom it follows the output.
        let after = self.lines_trimmed() + self.core.active_grid().scrollback_len() as u64;
        if self.display_offset > 0 && after > before {
            self.display_offset += (after - before) as usize;
        }
        self.display_offset = self.display_offset.min(self.scrollback_len());
        self.track_images();
    }

    fn after_change(&mut self, _trimmed_before: u64) {
        self.display_offset = self.display_offset.min(self.scrollback_len());
        self.collect_core_events();
        self.track_images();
    }

    fn collect_core_events(&mut self) {
        let responses = self.core.drain_responses();
        if !responses.is_empty() {
            self.events.push(EmulatorEvent::Reply(responses));
        }
        if self.core.title() != self.title {
            let title = self.core.title().to_string();
            self.title = title.clone();
            self.events.push(EmulatorEvent::TitleChanged(title));
        }
        let bells = self.core.bell_count();
        if bells != self.bell_count {
            self.bell_count = bells;
            self.events.push(EmulatorEvent::Bell);
        }
        // The emulator's own event queues are not used; keep them from growing.
        let _ = self.core.poll_events();
        let _ = self.core.drain_bell_events();
    }

    /// Events produced since the last call, in order.
    pub fn take_events(&mut self) -> Vec<EmulatorEvent> {
        std::mem::take(&mut self.events)
    }

    fn reply(&mut self, data: impl Into<Vec<u8>>) {
        self.events.push(EmulatorEvent::Reply(data.into()));
    }

    fn handle_osc(&mut self, code: u32, data: &str) {
        match code {
            7 => {
                let mut changed = None;
                self.cwd
                    .handle_file_uri(data, |cwd| changed = Some(cwd.to_string()));
                if let Some(cwd) = changed {
                    self.events.push(EmulatorEvent::CwdChanged(cwd));
                }
            }
            52 => match handle_osc52(data, self.allow_osc52_write) {
                Osc52Action::Write(text) => self.events.push(EmulatorEvent::ClipboardWrite(text)),
                Osc52Action::Reply(reply) => self.reply(reply),
                Osc52Action::Ignore => {}
            },
            133 | 633 => {
                let line = self.absolute_cursor_line();
                let first = self.lines_trimmed();
                let mut marks = Marks {
                    absolute_cursor_line: line,
                    first_absolute_line: first,
                    failed: &mut self.failed_commands,
                };
                if !self.commands.handle(&mut marks, data) {
                    let mut changed = None;
                    self.cwd
                        .handle_property(data, |cwd| changed = Some(cwd.to_string()));
                    if let Some(cwd) = changed {
                        self.events.push(EmulatorEvent::CwdChanged(cwd));
                    }
                }
            }
            10..=12 => self.dynamic_colors(code, data),
            _ => {}
        }
    }

    /// OSC 10/11/12: set or query the default foreground, background and cursor colors. Several
    /// `;`-separated values address the following colors in turn, as in xterm.
    fn dynamic_colors(&mut self, code: u32, data: &str) {
        for (offset, value) in data.split(';').enumerate() {
            let slot = code + offset as u32;
            if slot > 12 {
                break;
            }
            if value == "?" {
                let colors = self.colors();
                let color = match slot {
                    10 => colors.foreground,
                    11 => colors.background,
                    _ => colors.cursor,
                };
                let channel = |c: u8| format!("{c:02x}{c:02x}");
                self.reply(format!(
                    "\x1b]{slot};rgb:{}/{}/{}\x1b\\",
                    channel(color.r),
                    channel(color.g),
                    channel(color.b)
                ));
            } else if let Some(color) = parse_x_color(value) {
                match slot {
                    10 => self.overrides.foreground = Some(color),
                    11 => self.overrides.background = Some(color),
                    _ => self.overrides.cursor = Some(color),
                }
            }
        }
    }

    fn private_modes(&mut self, modes: &[u16], set: bool) {
        for mode in modes {
            match (mode, set) {
                (9, true) => self.mouse_protocol = MouseProtocol::X10,
                (1000, true) => self.mouse_protocol = MouseProtocol::Vt200,
                (1002, true) => self.mouse_protocol = MouseProtocol::Drag,
                (1003, true) => self.mouse_protocol = MouseProtocol::Any,
                (9 | 1000 | 1002 | 1003, false) => self.mouse_protocol = MouseProtocol::None,
                (1006, true) => self.mouse_encoding = MouseEncoding::Sgr,
                (1016, true) => self.mouse_encoding = MouseEncoding::SgrPixels,
                (1006 | 1016, false) => self.mouse_encoding = MouseEncoding::Default,
                (47 | 1047 | 1049, _) => self.display_offset = 0,
                _ => {}
            }
        }
    }

    fn cursor_style(&mut self, style: u16) {
        let style = if style == 0 { 1 } else { style };
        self.cursor_shape = Some(match style {
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => CursorShape::Block,
        });
        self.cursor_blink = Some(style % 2 == 1);
    }

    /// XTWINOPS: xterm (as Muxus configures it) answers the size reports only.
    fn window_op(&mut self, params: &[u16]) {
        let (cols, rows) = self.core.size();
        match params.first().copied().unwrap_or(0) {
            14 => {
                let width = (self.cell_size.0 * cols as f32).round();
                let height = (self.cell_size.1 * rows as f32).round();
                self.reply(format!("\x1b[4;{height};{width}t"));
            }
            16 => {
                let (width, height) = (self.cell_size.0.round(), self.cell_size.1.round());
                self.reply(format!("\x1b[6;{height};{width}t"));
            }
            18 => self.reply(format!("\x1b[8;{rows};{cols}t")),
            _ => {}
        }
    }

    /// ED 2. On the normal screen the screen's lines scroll into the scrollback instead of being
    /// blanked, so a shell that clears on login cannot destroy restored history.
    fn erase_display(&mut self) {
        let (cols, rows) = self.core.size();
        let full_region = self.core.scroll_region() == (0, rows.saturating_sub(1));
        if self.core.is_alt_screen_active() || self.core.origin_mode() || !full_region {
            self.core.process(b"\x1b[2J");
            return;
        }
        let grid = self.core.active_grid();
        let last = (0..rows).rev().find(|row| {
            grid.row(*row)
                .is_some_and(|cells| cells.iter().any(has_content))
        });
        let Some(last) = last else { return };
        let cursor = *self.core.cursor();
        let mut sequence = format!("\x1b[{rows};1H").into_bytes();
        sequence.extend(std::iter::repeat_n(b'\n', last + 1));
        sequence.extend(
            format!("\x1b[{};{}H", cursor.row + 1, cursor.col.min(cols - 1) + 1).into_bytes(),
        );
        self.core.process(&sequence);
    }

    // ---- state ---------------------------------------------------------------------------

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.current()
    }

    pub fn is_alt_screen(&self) -> bool {
        self.core.is_alt_screen_active()
    }

    pub fn application_cursor(&self) -> bool {
        self.core.application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.core.bracketed_paste()
    }

    /// Focus in/out reports (DECSET 1004).
    pub fn focus_reporting(&self) -> bool {
        self.core.focus_tracking()
    }

    /// The kitty keyboard flags the program negotiated.
    pub fn kitty_keyboard_flags(&self) -> u16 {
        self.core.keyboard_flags()
    }

    pub fn mouse_protocol(&self) -> MouseProtocol {
        self.mouse_protocol
    }

    pub fn mouse_encoding(&self) -> MouseEncoding {
        self.mouse_encoding
    }

    /// DECSCNM reverse video.
    pub fn reverse_video(&self) -> bool {
        self.core.reverse_video()
    }

    pub fn cursor(&self) -> CursorState {
        let cursor = self.core.cursor();
        CursorState {
            line: self.scrollback_len() + cursor.row,
            col: cursor.col.min(self.cols().saturating_sub(1)),
            visible: cursor.visible,
            shape: self.cursor_shape.or(match cursor.style {
                CoreCursorStyle::BlinkingBlock => None,
                CoreCursorStyle::SteadyBlock => Some(CursorShape::Block),
                CoreCursorStyle::BlinkingUnderline | CoreCursorStyle::SteadyUnderline => {
                    Some(CursorShape::Underline)
                }
                CoreCursorStyle::BlinkingBar | CoreCursorStyle::SteadyBar => Some(CursorShape::Bar),
            }),
            blink: self.cursor_blink,
        }
    }

    // ---- lines and scrolling -------------------------------------------------------------

    pub fn scrollback_len(&self) -> usize {
        self.core.active_grid().scrollback_len()
    }

    /// Lines dropped from the top of the scrollback since the terminal started.
    /// A number that grows whenever the screen's or the scrollback's cells
    /// change (the engine's damage generation): a renderer that remembers it
    /// can tell that nothing it drew from the buffer changed.
    pub fn content_generation(&self) -> u64 {
        self.core.damage_generation()
    }

    pub fn lines_trimmed(&self) -> u64 {
        let grid = self.core.active_grid();
        (grid.total_lines_scrolled() - grid.scrollback_len()) as u64
    }

    /// The absolute number of a buffer line: stable while lines scroll and are trimmed.
    pub fn absolute_line(&self, line: usize) -> u64 {
        self.lines_trimmed() + line as u64
    }

    /// The buffer line of an absolute line, if it is still in the buffer.
    pub fn buffer_line(&self, absolute: u64) -> Option<usize> {
        let first = self.lines_trimmed();
        let line = absolute.checked_sub(first)? as usize;
        (line < self.length()).then_some(line)
    }

    fn absolute_cursor_line(&self) -> u64 {
        self.absolute_line(self.scrollback_len() + self.core.cursor().row)
    }

    /// Lines the viewport is scrolled up from the bottom.
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// The buffer line shown in the top row.
    pub fn viewport_top(&self) -> usize {
        self.scrollback_len() - self.display_offset
    }

    /// Scroll the viewport by `delta` lines (negative is up, into the scrollback).
    pub fn scroll_lines(&mut self, delta: isize) {
        let offset = self.display_offset as isize - delta;
        self.display_offset = offset.clamp(0, self.scrollback_len() as isize) as usize;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.display_offset = 0;
    }

    pub fn scroll_to_top(&mut self) {
        self.display_offset = self.scrollback_len();
    }

    /// Bring a buffer line into the top row (clamped).
    pub fn scroll_to_line(&mut self, line: usize) {
        let top = line.min(self.scrollback_len());
        self.display_offset = self.scrollback_len() - top;
    }

    fn core_line(&self, line: usize) -> Option<&[Cell]> {
        let grid = self.core.active_grid();
        let scrollback = grid.scrollback_len();
        if line < scrollback {
            grid.scrollback_line(line)
        } else {
            grid.row(line - scrollback)
        }
    }

    /// The cells of a buffer line with their styles, for painting.
    pub fn styled_line(&self, line: usize) -> Option<Vec<StyledCell>> {
        let cells = self.core_line(line)?;
        Some(cells.iter().map(styled_cell).collect())
    }

    /// The URL of an OSC 8 hyperlink.
    pub fn hyperlink_url(&self, id: u32) -> Option<String> {
        self.core.get_hyperlink_url(id)
    }

    /// Lines of commands that failed, as buffer lines (scrollbar marks).
    pub fn failed_command_lines(&self) -> Vec<usize> {
        self.failed_commands
            .iter()
            .filter_map(|line| self.buffer_line(*line))
            .collect()
    }

    // ---- images --------------------------------------------------------------------------

    fn graphic_cells(&self, graphic: &TerminalGraphic) -> Option<(usize, usize)> {
        let placement = &graphic.placement;
        match (placement.columns, placement.rows) {
            (Some(cols), Some(rows)) => Some((cols as usize, rows as usize)),
            _ => None,
        }
    }

    fn track_images(&mut self) {
        let trimmed = self.lines_trimmed();
        let scrollback = self.scrollback_len() as u64;
        let mut seen = std::collections::HashSet::new();
        for graphic in self.core.all_graphics() {
            seen.insert(graphic.id);
            let top = (trimmed + scrollback + graphic.position.1 as u64)
                .saturating_sub(graphic.scroll_offset_rows as u64);
            self.image_tops
                .insert(graphic.id, (top, graphic.position.0));
        }
        for graphic in self.core.all_scrollback_graphics() {
            seen.insert(graphic.id);
        }
        self.image_tops.retain(|id, _| seen.contains(id));
    }

    /// Images to paint, with the buffer line of their top edge.
    pub fn images(&self) -> Vec<ImagePlacement> {
        let trimmed = self.lines_trimmed() as i64;
        let graphics = self
            .core
            .all_graphics()
            .iter()
            .chain(self.core.all_scrollback_graphics());
        graphics
            .filter(|graphic| !graphic.is_virtual)
            .filter_map(|graphic| {
                let (top, col) = *self.image_tops.get(&graphic.id)?;
                Some(ImagePlacement {
                    id: graphic.id,
                    line: (top as i64 - trimmed) as isize,
                    col,
                    pixels: graphic.pixels.clone(),
                    width: graphic.width,
                    height: graphic.height,
                    cells: self.graphic_cells(graphic),
                    z_index: graphic.placement.z_index,
                })
            })
            .collect()
    }

    // ---- whole-buffer operations ---------------------------------------------------------

    /// xterm's `clear()`: the cursor's line becomes the first line and everything else, the
    /// scrollback included, is cleared.
    pub fn clear(&mut self) {
        if self.core.is_alt_screen_active() {
            return;
        }
        let cursor = *self.core.cursor();
        let line = self.scrollback_len() + cursor.row;
        let row =
            crate::serialize::serialize_line_cells(&self.styled_line(line).unwrap_or_default());
        let mut sequence = b"\x1b[3J\x1b[H".to_vec();
        sequence.extend(row.into_bytes());
        sequence.extend(format!("\x1b[0m\x1b[1;{}H", cursor.col + 1).into_bytes());
        self.core.process(&sequence);
        self.failed_commands.clear();
        self.display_offset = 0;
        self.collect_core_events();
    }

    /// Plain text of the scrollback and screen, trailing blank lines dropped, each line ending in
    /// a newline ("Export as text").
    pub fn buffer_text(&self) -> String {
        let mut lines: Vec<String> = (0..self.length())
            .map(|line| crate::buffer::line_text(self, line).unwrap_or_default())
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        }
    }

    /// The last non-blank lines at or above the cursor — usually the prompt and the output just
    /// before it. The scan stops one screen up, so a freshly cleared terminal reads as empty.
    pub fn recent_lines(&self, count: usize) -> Vec<String> {
        let cursor_line = self.cursor().line as isize;
        let rows = self.rows() as isize;
        let mut lines = Vec::new();
        let mut y = cursor_line;
        while y >= 0 && y > cursor_line - rows && lines.len() < count {
            let text = crate::buffer::line_text(self, y as usize).unwrap_or_default();
            if !text.trim().is_empty() {
                lines.insert(0, text);
            }
            y -= 1;
        }
        lines
    }
}

/// Parse an X11 color spec as OSC 10/11/12 take it: `rgb:r/g/b` (1–4 hex digits per channel),
/// `#rgb`-style hex, or a CSS color.
fn parse_x_color(value: &str) -> Option<Rgba> {
    if let Some(spec) = value.strip_prefix("rgb:") {
        let parts: Vec<&str> = spec.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let channel = |part: &str| -> Option<u8> {
            if part.is_empty() || part.len() > 4 {
                return None;
            }
            let value = u32::from_str_radix(part, 16).ok()?;
            let max = (1u32 << (4 * part.len())) - 1;
            Some(((value * 255 + max / 2) / max) as u8)
        };
        return Some(Rgba::rgb(
            channel(parts[0])?,
            channel(parts[1])?,
            channel(parts[2])?,
        ));
    }
    parse_css_color(value).map(|c| c.with_alpha(255))
}

impl TextBuffer for Emulator {
    fn length(&self) -> usize {
        self.scrollback_len() + self.rows()
    }

    fn cols(&self) -> usize {
        Emulator::cols(self)
    }

    fn is_wrapped(&self, line: usize) -> bool {
        if line == 0 {
            return false;
        }
        let grid = self.core.active_grid();
        let previous = line - 1;
        let scrollback = grid.scrollback_len();
        if previous < scrollback {
            grid.is_scrollback_wrapped(previous)
        } else {
            grid.is_line_wrapped(previous - scrollback)
        }
    }

    fn line_cells(&self, line: usize) -> Option<Vec<CellText>> {
        self.core_line(line)
            .map(|cells| cells.iter().map(cell_text).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::line_text;
    use crate::palette::terminal_scheme;

    fn emulator(cols: u16, rows: u16) -> Emulator {
        Emulator::new(
            EmulatorConfig {
                cols,
                rows,
                scrollback: 100,
            },
            &terminal_scheme(None, &[]).theme,
        )
    }

    fn screen(term: &Emulator) -> Vec<String> {
        (term.scrollback_len()..term.length())
            .map(|line| line_text(term, line).unwrap())
            .collect()
    }

    #[test]
    fn the_content_generation_moves_with_every_change_to_the_cells() {
        let mut term = emulator(10, 3);
        let mut last = term.content_generation();
        let mut changed = |term: &Emulator, what: &str| {
            let now = term.content_generation();
            assert!(now > last, "{what} kept the generation at {now}");
            last = now;
        };
        term.write(b"a");
        changed(&term, "a write");
        term.write(b"\r\n\r\n\r\nscrolled");
        changed(&term, "a scroll");
        term.write(b"\x1b[2J");
        changed(&term, "ED 2");
        term.write(b"\x1b[?1049h");
        changed(&term, "the alternate screen");
        term.write(b"\x1b[?1049l");
        changed(&term, "leaving it");
        term.resize(12, 4);
        changed(&term, "a resize");
        term.clear();
        changed(&term, "clear");
        term.write(b"\x1b[31m");
        assert_eq!(term.content_generation(), last, "a pen change alone");
    }

    #[test]
    fn a_shorter_screen_keeps_the_cursor_line_and_scrolls_the_top_away() {
        let mut term = emulator(20, 10);
        for n in 0..8 {
            term.write(format!("line {n}\r\n").as_bytes());
        }
        term.write(b"$ ");
        term.resize(20, 4);
        assert_eq!(screen(&term), ["line 5", "line 6", "line 7", "$ "]);
        assert_eq!(term.cursor().col, 2);
        assert_eq!(
            line_text(&term, term.scrollback_len() - 1).unwrap(),
            "line 4"
        );
        // Typing goes on where the prompt is.
        term.write(b"ls");
        assert_eq!(screen(&term)[3], "$ ls");

        // Blank rows below the cursor simply go.
        let mut short = emulator(20, 10);
        short.write(b"one\r\ntwo");
        short.resize(20, 4);
        assert_eq!(screen(&short), ["one", "two", "", ""]);
        assert_eq!(short.scrollback_len(), 0);

        // A narrower and shorter screen keeps the cursor line as well.
        let mut both = emulator(20, 10);
        for n in 0..8 {
            both.write(format!("row {n}\r\n").as_bytes());
        }
        both.write(b"$ ");
        both.resize(15, 3);
        let rows = screen(&both);
        assert_eq!(rows[..2], ["row 6", "row 7"]);
        assert_eq!(rows[2].trim_end(), "$");
    }

    #[test]
    fn writes_text_and_tells_written_spaces_from_blank_cells() {
        let mut term = emulator(10, 3);
        term.write(b"abc \r\nx");
        assert_eq!(screen(&term), ["abc ", "x", ""]);
        term.write(b"\x1b[2K");
        assert_eq!(screen(&term)[1], "");
    }

    #[test]
    fn keeps_explicit_white_and_black_apart_from_the_defaults() {
        let mut term = emulator(10, 2);
        term.write(b"\x1b[37;40mA\x1b[0mB\x1b[31;47mC");
        let cells = term.styled_line(term.scrollback_len()).unwrap();
        assert_eq!(
            (cells[0].style.fg, cells[0].style.bg),
            (CellColor::Palette(7), CellColor::Palette(0))
        );
        assert_eq!(
            (cells[1].style.fg, cells[1].style.bg),
            (CellColor::Default, CellColor::Default)
        );
        assert_eq!(
            (cells[2].style.fg, cells[2].style.bg),
            (CellColor::Palette(1), CellColor::Palette(7))
        );
        assert_eq!(cells[3].style.fg, CellColor::Default);
        assert!(cells[3].text.is_empty());
    }

    #[test]
    fn soft_wraps_long_lines() {
        let mut term = emulator(4, 3);
        term.write(b"abcdef");
        let first = term.scrollback_len();
        assert!(!term.is_wrapped(first));
        assert!(term.is_wrapped(first + 1));
        assert_eq!(screen(&term)[..2], ["abcd", "ef"]);
    }

    #[test]
    fn scrolls_the_screen_into_the_scrollback_on_erase_display() {
        let mut term = emulator(10, 3);
        term.write(b"one\r\ntwo\r\n\x1b[2J\x1b[Hthree");
        assert_eq!(term.scrollback_len(), 2);
        assert_eq!(line_text(&term, 0).as_deref(), Some("one"));
        assert_eq!(line_text(&term, 1).as_deref(), Some("two"));
        assert_eq!(screen(&term)[0], "three");
    }

    #[test]
    fn answers_osc52_queries_with_an_empty_clipboard_and_writes_when_allowed() {
        let mut term = emulator(10, 2);
        term.write(b"\x1b]52;c;?\x07\x1b]52;c;aGk=\x07");
        assert_eq!(
            term.take_events(),
            [
                EmulatorEvent::Reply(b"\x1b]52;c;\x07".to_vec()),
                EmulatorEvent::ClipboardWrite("hi".into())
            ]
        );
        term.set_allow_osc52_write(false);
        term.write(b"\x1b]52;c;aGk=\x07");
        assert!(term.take_events().is_empty());
    }

    #[test]
    fn tracks_the_working_directory_and_failed_commands() {
        let mut term = emulator(20, 4);
        term.write(b"\x1b]7;file://host/srv/app\x07");
        assert_eq!(
            term.take_events(),
            [EmulatorEvent::CwdChanged("/srv/app".into())]
        );
        term.write(b"\x1b]133;C\x07false\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ ");
        assert_eq!(term.failed_command_lines(), [term.scrollback_len()]);
    }

    #[test]
    fn reports_titles_bells_and_sizes() {
        let mut term = emulator(80, 24);
        term.set_cell_size(8.4, 18.5);
        term.write(b"\x1b]0;hello\x07\x07\x1b[14t\x1b[16t\x1b[18t\x1b[21t");
        let events = term.take_events();
        assert!(events.contains(&EmulatorEvent::TitleChanged("hello".into())));
        assert!(events.contains(&EmulatorEvent::Bell));
        let replies: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                EmulatorEvent::Reply(r) => Some(r.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            String::from_utf8(replies).unwrap(),
            "\x1b[4;444;672t\x1b[6;19;8t\x1b[8;24;80t"
        );
    }

    #[test]
    fn answers_and_applies_dynamic_color_requests() {
        let mut term = emulator(10, 2);
        term.write(b"\x1b]11;?\x07");
        assert_eq!(
            term.take_events(),
            [EmulatorEvent::Reply(
                b"\x1b]11;rgb:1818/1818/1818\x1b\\".to_vec()
            )]
        );
        term.write(b"\x1b]10;rgb:ff/00/00\x07");
        assert_eq!(term.colors().foreground, Rgba::rgb(255, 0, 0));
        term.write(b"\x1bc");
        assert_eq!(term.colors().foreground, Rgba::rgb(0xcc, 0xcc, 0xcc));
    }

    #[test]
    fn tracks_mouse_modes_and_cursor_styles() {
        let mut term = emulator(10, 2);
        term.write(b"\x1b[?1002h\x1b[?1006h\x1b[6 q");
        assert_eq!(
            (term.mouse_protocol(), term.mouse_encoding()),
            (MouseProtocol::Drag, MouseEncoding::Sgr)
        );
        assert_eq!(
            (term.cursor().shape, term.cursor().blink),
            (Some(CursorShape::Bar), Some(false))
        );
        term.write(b"\x1b[?1002l");
        assert_eq!(term.mouse_protocol(), MouseProtocol::None);
    }

    #[test]
    fn keeps_a_scrolled_back_viewport_on_its_lines() {
        let mut term = emulator(10, 2);
        for index in 0..10 {
            term.write(format!("line {index}\r\n").as_bytes());
        }
        term.scroll_lines(-3);
        let top = term.viewport_top();
        let text = line_text(&term, top);
        term.write(b"more\r\nmore\r\n");
        assert_eq!(line_text(&term, term.viewport_top()), text);
        term.scroll_to_bottom();
        assert_eq!(term.display_offset(), 0);
    }

    #[test]
    fn clears_everything_but_the_prompt_line() {
        let mut term = emulator(10, 3);
        term.write(b"a\r\nb\r\nc\r\nd\r\n$ ls");
        term.clear();
        assert_eq!(term.scrollback_len(), 0);
        assert_eq!(screen(&term), ["$ ls", "", ""]);
        assert_eq!((term.cursor().line, term.cursor().col), (0, 4));
    }

    #[test]
    fn places_kitty_images_and_keeps_them_with_their_lines() {
        use base64::Engine;
        let mut term = emulator(20, 4);
        term.set_cell_size(8.0, 16.0);
        let pixels = base64::engine::general_purpose::STANDARD.encode([255u8; 2 * 2 * 4]);
        term.write(format!("x\r\n\x1b_Ga=T,f=32,s=2,v=2;{pixels}\x1b\\").as_bytes());
        let images = term.images();
        assert_eq!(images.len(), 1);
        assert_eq!(
            (
                images[0].line,
                images[0].col,
                images[0].width,
                images[0].height
            ),
            (1, 0, 2, 2)
        );
        term.write(b"\r\n\r\n\r\n\r\n\r\n");
        let images = term.images();
        assert_eq!(
            images.len(),
            1,
            "the image scrolls into the scrollback with its line"
        );
        assert_eq!(images[0].line, 1);
    }

    #[test]
    fn places_sixel_and_iterm2_images() {
        use base64::Engine;
        let mut term = emulator(20, 6);
        term.set_cell_size(8.0, 16.0);
        // A 1×6 red sixel column.
        term.write(b"\x1bPq#0;2;100;0;0#0~\x1b\\");
        assert_eq!(term.images().len(), 1, "sixel");
        // A 1×1 PNG through iTerm2's OSC 1337.
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        let size = base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap()
            .len();
        term.write(format!("\r\n\x1b]1337;File=inline=1;size={size}:{PNG}\x07").as_bytes());
        assert_eq!(term.images().len(), 2, "iTerm2");
    }

    #[test]
    fn exports_text_and_recent_lines() {
        let mut term = emulator(10, 3);
        term.write(b"one\r\n\r\ntwo\r\n$ ");
        assert_eq!(term.buffer_text(), "one\n\ntwo\n$ \n");
        assert_eq!(term.recent_lines(2), ["two", "$ "]);
    }
}
