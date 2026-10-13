//! The terminal view entity.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement, Styled, Task, UTF16Selection, Window, canvas, div,
    point,
};
use muxus_core::stream::{TerminalSize, TransportChannels, TransportEvent, TransportInput};
use muxus_term::buffer::TextBuffer;
use muxus_term::color::{Rgba, parse_css_color};
use muxus_term::fit::{
    FontMetrics, cell_metrics, chromium_font_height, fit_grid, should_fit_terminal,
};
use muxus_term::font_catalog::terminal_font_stack;
use muxus_term::keyboard::{KeyAction, KeyEncodeContext, key_down};
use muxus_term::keywords::{KeywordDecoration, keyword_decorations};
use muxus_term::links::{
    BufferLink, LinkClick, LinkTarget, link_activation_for_platform, link_activation_matches,
    link_at,
};
use muxus_term::mouse::{
    MouseAction, MouseButton as ReportButton, MouseReport, MouseReporter, protocol_active,
};
use muxus_term::paste::{requires_paste_confirmation, terminal_paste_data};
use muxus_term::right_click::{RightClickIntent, terminal_right_click_intent};
use muxus_term::search::{
    ACTIVE_MATCH_BACKGROUND, ACTIVE_MATCH_OVERVIEW_RULER, MATCH_BACKGROUND, MATCH_OVERVIEW_RULER,
    SearchOptions, SearchResult, TerminalSearch,
};
use muxus_term::selection::{BufferPoint, Selection, SelectionMode};
use muxus_term::shell_integration::ERROR_MARK_COLOR;
use muxus_term::style::{CellContext, EffectiveColors, resolve_cell};
use muxus_term::terminal::{Emulator, EmulatorConfig, EmulatorEvent};
use muxus_ui::zoom::{css, layout_px, px};

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder;

use crate::keys::keyboard_event;
use crate::paint::{
    CellSize, FontSet, Frame, FrameCursor, FrameImage, PaintCell, RulerLane, RulerMark, Scrollbar,
    hsla, inner_padding, paint_frame, scrollbar_width,
};
use crate::{
    ClearScrollback, CloseFind, Copy, Find, FindNext, FindPrevious, Paste, ScrollPageDown,
    ScrollPageUp, ScrollToBottom, ScrollToTop, SelectAll, TerminalSettings, TerminalViewEvent,
    ZoomIn, ZoomOut, ZoomReset,
};

const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 40.0;
/// The terminal container's border width (`border: 1` on the client's Box).
const CONTAINER_BORDER: f32 = 1.0;
/// Quiet period a size has to hold before the terminal refits.
const RESIZE_SETTLE: Duration = Duration::from_millis(90);
/// xterm's cursor blink: half a second on, half off.
const BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// The scrollbar hides this long after the pointer leaves or scrolling stops, then fades.
const SCROLLBAR_HIDE_AFTER: Duration = Duration::from_millis(500);
const SCROLLBAR_FADE: Duration = Duration::from_millis(800);
/// Type-ahead held while the transport is busy.
const OUTPUT_BATCH: usize = 64;

/// The inputs the painted rows were built from (see `TerminalView::rows_key`).
#[derive(PartialEq)]
struct RowsKey {
    content: u64,
    top: usize,
    rows: usize,
    cols: usize,
    alt: bool,
    colors: EffectiveColors,
    selection: Option<Selection>,
    search: Option<(String, SearchOptions, isize, usize)>,
    hovered: Option<BufferLink>,
    focused: bool,
    /// The blink phase, only when a cell blinks.
    blink: Option<bool>,
    settings: u64,
}

struct RowsCache {
    key: RowsKey,
    rows: Rc<Vec<Vec<PaintCell>>>,
    has_blink: bool,
}

/// Where the terminal was laid out last, for mapping the pointer to cells.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    /// The `.xterm` padding box.
    bounds: Option<Bounds<Pixels>>,
}

/// A terminal: an emulator, a transport and a GPUI view that paints and drives it.
pub struct TerminalView {
    emulator: Emulator,
    settings: TerminalSettings,
    focus: FocusHandle,
    input: async_channel::Sender<TransportInput>,
    closed: bool,
    zoom: f32,
    layout: Rc<Cell<Layout>>,
    cell: CellSize,
    fonts: FontSet,
    metrics_key: Option<(SharedString, u32, u32, u32)>,
    fitted: bool,
    size: TerminalSize,
    resize_task: Option<Task<()>>,
    selection: Option<Selection>,
    selecting: bool,
    lines_trimmed: u64,
    search: TerminalSearch,
    search_term: String,
    search_options: SearchOptions,
    search_open: bool,
    search_input: Option<Entity<InputState>>,
    _search_subscription: Option<gpui_kit::Subscription>,
    hovered_link: Option<BufferLink>,
    mouse: MouseReporter,
    reporting_button: Option<ReportButton>,
    blink_started: Instant,
    blink_visible: bool,
    focused: bool,
    hovered: bool,
    scrolled_at: Option<Instant>,
    left_at: Option<Instant>,
    scroll_remainder: f32,
    images: HashMap<u64, ((u32, u32), Arc<RenderImage>)>,
    scale_factor: f32,
    preedit: Option<String>,
    /// Bumped with every settings change, for the rows cache.
    settings_version: u64,
    rows_cache: Option<RowsCache>,
    /// Runs while the cursor blinks or the scrollbar fades, and only then: an
    /// idle, unfocused terminal wakes nothing.
    tick_task: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn parse_color(value: &str) -> Rgba {
    parse_css_color(value).unwrap_or_default()
}

impl TerminalView {
    /// A terminal on `channels`. Output arriving on the channels is shown; keystrokes, pastes and
    /// resizes go back on them.
    pub fn new(
        channels: TransportChannels,
        settings: TerminalSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let emulator = {
            let mut emulator = Emulator::new(
                EmulatorConfig {
                    cols: 80,
                    rows: 24,
                    scrollback: settings.scrollback,
                },
                &settings.theme,
            );
            emulator.set_allow_osc52_write(settings.allow_osc52_write);
            emulator
        };
        let focus = cx.focus_handle();
        let (input_tx, input_rx) = async_channel::unbounded::<TransportInput>();
        let transport_input = channels.input.clone();
        let mut tasks = Vec::new();
        // Keystrokes and resizes go out in order through one pump.
        tasks.push(cx.background_spawn(async move {
            while let Ok(message) = input_rx.recv().await {
                if transport_input.send(message).await.is_err() {
                    break;
                }
            }
        }));
        let events = channels.events.clone();
        tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < OUTPUT_BATCH {
                    match events.try_recv() {
                        Ok(more) => batch.push(more),
                        Err(_) => break,
                    }
                }
                if this
                    .update(cx, |view, cx| view.handle_transport(batch, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        let subscriptions = vec![
            cx.on_focus(&focus, window, |view, _, cx| view.set_focused(true, cx)),
            cx.on_blur(&focus, window, |view, _, cx| view.set_focused(false, cx)),
        ];
        let fonts = FontSet {
            family: "JetBrains Mono".into(),
            fallbacks: Vec::new(),
            size: px(settings.font_size),
        };
        Self {
            emulator,
            settings,
            focus,
            input: input_tx,
            closed: false,
            zoom: 0.0,
            layout: Rc::default(),
            cell: CellSize {
                width: 8.4,
                height: 18.5,
                baseline_shift: 0.0,
            },
            fonts,
            metrics_key: None,
            fitted: false,
            size: TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            resize_task: None,
            selection: None,
            selecting: false,
            lines_trimmed: 0,
            search: TerminalSearch::new(),
            search_term: String::new(),
            search_options: SearchOptions::default(),
            search_open: false,
            search_input: None,
            _search_subscription: None,
            hovered_link: None,
            mouse: MouseReporter::default(),
            reporting_button: None,
            blink_started: Instant::now(),
            blink_visible: true,
            focused: false,
            hovered: false,
            scrolled_at: None,
            left_at: None,
            scroll_remainder: 0.0,
            images: HashMap::new(),
            scale_factor: 1.0,
            preedit: None,
            settings_version: 0,
            rows_cache: None,
            tick_task: None,
            _tasks: tasks,
            _subscriptions: subscriptions,
        }
    }

    // ---- public API ----------------------------------------------------------------------

    /// The emulator, for reading the buffer (text export, snapshots, recent lines).
    pub fn emulator(&self) -> &Emulator {
        &self.emulator
    }

    /// The grid size last sent to the transport.
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// Apply new settings; they take effect on the running terminal.
    pub fn set_settings(&mut self, settings: TerminalSettings, cx: &mut Context<Self>) {
        self.emulator.set_theme(&settings.theme);
        self.emulator
            .set_allow_osc52_write(settings.allow_osc52_write);
        self.settings = settings;
        self.settings_version += 1;
        self.metrics_key = None;
        cx.notify();
    }

    pub fn settings(&self) -> &TerminalSettings {
        &self.settings
    }

    /// Show output that did not come from the transport (status notices, a restored snapshot).
    pub fn write_output(&mut self, data: &[u8], cx: &mut Context<Self>) {
        self.feed(data, cx);
        cx.notify();
    }

    /// Send raw input to the session, as if typed. Returns `false` when the session has ended.
    pub fn send_input(&mut self, data: impl Into<Vec<u8>>) -> bool {
        if self.closed {
            return false;
        }
        self.input
            .try_send(TransportInput::Data(data.into()))
            .is_ok()
    }

    /// Paste text the way xterm does: line breaks become Enter, wrapped in bracketed-paste
    /// markers when the program asked for them. A multi-line paste asks the app to confirm first
    /// ([`TerminalViewEvent::ConfirmPaste`]) when the setting says so.
    pub fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if self.settings.paste_warn_multiline && requires_paste_confirmation(text) {
            cx.emit(TerminalViewEvent::ConfirmPaste(text.to_string()));
            return;
        }
        self.paste_confirmed(text, cx);
    }

    /// Paste without asking (after the app's confirmation dialog).
    pub fn paste_confirmed(&mut self, text: &str, cx: &mut Context<Self>) {
        let data = terminal_paste_data(text, self.emulator.bracketed_paste());
        self.user_input(data.into_bytes(), cx);
    }

    /// Send BREAK on the session's line (serial, telnet, SSH).
    pub fn send_break(&mut self, duration_ms: u32) -> bool {
        !self.closed
            && self
                .input
                .try_send(TransportInput::Break { duration_ms })
                .is_ok()
    }

    /// Close the session from the user's side.
    pub fn close(&mut self) {
        let _ = self.input.try_send(TransportInput::Close);
        self.closed = true;
    }

    /// The selected text, as copying it would put it on the clipboard.
    pub fn selection_text(&self) -> String {
        self.selection
            .as_ref()
            .map(|s| s.text(&self.emulator, line_ending()))
            .unwrap_or_default()
    }

    pub fn has_selection(&self) -> bool {
        self.selection
            .as_ref()
            .is_some_and(|s| !s.is_empty(&self.emulator))
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection = Some(Selection::all());
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selection = None;
        cx.notify();
    }

    /// Clear the screen and scrollback, keeping the prompt line.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.emulator.clear();
        self.selection = None;
        self.lines_trimmed = self.emulator.lines_trimmed();
        cx.notify();
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.apply_zoom(1.0, cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.apply_zoom(-1.0, cx);
    }

    pub fn zoom_reset(&mut self, cx: &mut Context<Self>) {
        self.zoom = 0.0;
        self.metrics_key = None;
        cx.notify();
    }

    /// Zoom as a percentage of the preference font size.
    pub fn zoom_percent(&self) -> u32 {
        ((self.settings.font_size + self.zoom) / self.settings.font_size * 100.0).round() as u32
    }

    /// Persist-ready scrollback snapshot (see `muxus_term::serialize::snapshot`).
    pub fn snapshot(&self, max_body_bytes: Option<usize>) -> Option<String> {
        muxus_term::serialize::snapshot(&self.emulator, max_body_bytes)
    }

    /// Open the search box ("Find in terminal") and focus it.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        let input = match &self.search_input {
            Some(input) => input.clone(),
            None => {
                let input =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Find in terminal"));
                self._search_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |view, input, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => {
                            let term = input.read(cx).value().to_string();
                            let options = view.search_options;
                            view.set_search(&term, options, cx);
                        }
                        InputEvent::PressEnter { shift, .. } => {
                            if *shift {
                                view.find_previous(cx);
                            } else {
                                view.find_next(cx);
                            }
                        }
                        _ => {}
                    },
                ));
                self.search_input = Some(input.clone());
                input
            }
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.emit(TerminalViewEvent::SearchOpened);
        cx.notify();
    }

    /// Open the search box with a query already typed (the app's "find selection" and tests).
    pub fn set_search_query(
        &mut self,
        query: &str,
        options: SearchOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_search(window, cx);
        if let Some(input) = &self.search_input {
            let query = query.to_string();
            input.update(cx, |input, cx| input.set_value(query, window, cx));
        }
        self.set_search(query, options, cx);
    }

    /// Close the search box; the highlights go and the terminal takes the focus back.
    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn toggle_search_option(
        &mut self,
        toggle: impl FnOnce(&mut SearchOptions),
        cx: &mut Context<Self>,
    ) {
        let mut options = self.search_options;
        toggle(&mut options);
        let term = self.search_term.clone();
        self.set_search(&term, options, cx);
    }

    pub fn is_search_open(&self) -> bool {
        self.search_open
    }

    /// Update the query as the user types: the match grows in place (xterm's incremental find).
    pub fn set_search(&mut self, term: &str, options: SearchOptions, cx: &mut Context<Self>) {
        self.search_term = term.to_string();
        self.search_options = options;
        if term.is_empty() {
            self.search.clear();
            cx.notify();
            return;
        }
        let selection = self.selection_range();
        let result = self
            .search
            .find_next(&self.emulator, term, &options, selection);
        self.select_result(result, cx);
    }

    /// Next match (Enter in the search box).
    pub fn find_next(&mut self, cx: &mut Context<Self>) {
        let (term, options) = (self.search_term.clone(), self.search_options);
        if term.is_empty() {
            return;
        }
        let selection = self.selection_range();
        let result = self
            .search
            .find_next(&self.emulator, &term, &options, selection);
        self.select_result(result, cx);
    }

    /// Previous match (Shift+Enter).
    pub fn find_previous(&mut self, cx: &mut Context<Self>) {
        let (term, options) = (self.search_term.clone(), self.search_options);
        if term.is_empty() {
            return;
        }
        let selection = self.selection_range();
        let result = self
            .search
            .find_previous(&self.emulator, &term, &options, selection);
        self.select_result(result, cx);
    }

    /// `(active index, match count)` for the "3/12" label; index -1 when nothing is selected.
    pub fn search_result_index(&self) -> (isize, usize) {
        self.search.result_index()
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    // ---- transport ---------------------------------------------------------------------

    fn handle_transport(&mut self, batch: Vec<TransportEvent>, cx: &mut Context<Self>) {
        for event in batch {
            match event {
                TransportEvent::Data(data) => self.feed(&data, cx),
                TransportEvent::Exit { code, signal } => {
                    self.closed = true;
                    cx.emit(TerminalViewEvent::Exited { code, signal });
                }
                TransportEvent::Error(message) => {
                    self.closed = true;
                    cx.emit(TerminalViewEvent::Failed(message));
                }
            }
        }
        cx.notify();
    }

    fn feed(&mut self, data: &[u8], cx: &mut Context<Self>) {
        self.emulator.write(data);
        for event in self.emulator.take_events() {
            match event {
                EmulatorEvent::Reply(reply) => {
                    if !self.closed {
                        let _ = self.input.try_send(TransportInput::Data(reply));
                    }
                }
                EmulatorEvent::TitleChanged(title) => {
                    cx.emit(TerminalViewEvent::TitleChanged(title))
                }
                EmulatorEvent::Bell => cx.emit(TerminalViewEvent::Bell),
                EmulatorEvent::CwdChanged(cwd) => cx.emit(TerminalViewEvent::CwdChanged(cwd)),
                EmulatorEvent::ClipboardWrite(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text))
                }
            }
        }
        // Keep the selection on its lines while the scrollback trims.
        let trimmed = self.emulator.lines_trimmed();
        if trimmed > self.lines_trimmed {
            let delta = (trimmed - self.lines_trimmed) as usize;
            if let Some(selection) = &mut self.selection
                && !selection.shift_up(delta)
            {
                self.selection = None;
            }
        }
        self.lines_trimmed = trimmed;
        if self.search_open && !self.search_term.is_empty() {
            self.search.refresh(&self.emulator);
        }
        self.restart_blink();
    }

    /// Input the user made (keys, pastes): it scrolls to the bottom, clears the selection and is
    /// mirrored by the app when multi-execution is on.
    fn user_input(&mut self, data: Vec<u8>, cx: &mut Context<Self>) {
        if data.is_empty() {
            return;
        }
        self.emulator.scroll_to_bottom();
        self.selection = None;
        self.restart_blink();
        if self.closed {
            cx.emit(TerminalViewEvent::InputAfterExit(data));
        } else {
            let _ = self.input.try_send(TransportInput::Data(data.clone()));
            cx.emit(TerminalViewEvent::Input(data));
        }
        cx.notify();
    }

    // ---- sizing --------------------------------------------------------------------------

    fn apply_zoom(&mut self, step: f32, cx: &mut Context<Self>) {
        let base = self.settings.font_size;
        let next = (base + self.zoom + step).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.zoom = next - base;
        self.metrics_key = None;
        cx.notify();
    }

    fn font_size(&self) -> f32 {
        (self.settings.font_size + self.zoom).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    }

    /// Measure the font the way xterm's DOM renderer does and derive the cell size.
    fn update_metrics(&mut self, window: &mut Window) {
        let size = self.font_size();
        let key = (
            SharedString::from(self.settings.font_family.clone()),
            size.to_bits(),
            self.settings.line_height.to_bits(),
            window.scale_factor().to_bits(),
        );
        if self.metrics_key.as_ref() == Some(&key) {
            return;
        }
        let stack = terminal_font_stack(&self.settings.font_family);
        let fonts = FontSet {
            family: stack[0].clone().into(),
            fallbacks: stack[1..].to_vec(),
            size: px(size),
        };
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&fonts.font(false, false));
        let advance = text_system
            .advance(font_id, px(size), 'W')
            .map(|s| f32::from(s.width))
            .unwrap_or(size * 0.6);
        let ascent = f32::from(text_system.ascent(font_id, px(size)));
        let descent = f32::from(text_system.descent(font_id, px(size))).abs();
        let metrics = cell_metrics(
            FontMetrics {
                char_width: advance,
                char_height: chromium_font_height(ascent, descent),
            },
            self.settings.line_height,
            0.0,
            window.scale_factor(),
        );
        let chromium_ascent = ascent.round();
        let chromium_height = chromium_font_height(ascent, descent);
        let baseline_shift = (ascent + descent - chromium_height) / 2.0 + chromium_ascent - ascent;
        self.cell = CellSize {
            width: metrics.width,
            height: metrics.height,
            baseline_shift,
        };
        self.emulator.set_cell_size(metrics.width, metrics.height);
        self.fonts = fonts;
        self.metrics_key = Some(key);
        self.scale_factor = window.scale_factor();
    }

    fn request_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        if !self.fitted {
            // The first measurable size is not a drag: apply it at once so the session starts at
            // the right width.
            self.fitted = true;
            self.apply_resize(cols, rows, cx);
            return;
        }
        // Dragging a side panel changes the width every frame; refit once it holds still so the
        // remote shell is not SIGWINCHed dozens of times.
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _ = this.update(cx, |view, cx| view.apply_resize(cols, rows, cx));
        }));
    }

    fn apply_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        self.emulator.resize(cols, rows);
        let size = TerminalSize {
            cols,
            rows,
            pixel_width: (self.cell.width * f32::from(cols)).round() as u16,
            pixel_height: (self.cell.height * f32::from(rows)).round() as u16,
        };
        if size != self.size {
            self.size = size;
            if !self.closed {
                let _ = self.input.try_send(TransportInput::Resize(size));
            }
            cx.emit(TerminalViewEvent::Resized(size));
        }
        cx.notify();
    }

    // ---- timing --------------------------------------------------------------------------

    fn restart_blink(&mut self) {
        self.blink_started = Instant::now();
        self.blink_visible = true;
    }

    /// How long until the next blink phase or fade step, or `None` when
    /// nothing animates.
    fn next_tick_delay(&self) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        if self.focused && self.cursor_blink() {
            let elapsed = self.blink_started.elapsed().as_millis();
            let period = BLINK_INTERVAL.as_millis();
            let left = period - elapsed % period;
            next = Some(Duration::from_millis(left as u64 + 1));
        }
        if self.scrollbar_alpha_changes() {
            let fade = Duration::from_millis(100);
            next = Some(next.map_or(fade, |n| n.min(fade)));
        }
        next
    }

    /// Start the animation timer when something begins to animate.
    fn schedule_tick(&mut self, cx: &mut Context<Self>) {
        if self.tick_task.is_some() || self.next_tick_delay().is_none() {
            return;
        }
        self.tick_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(delay)) = this.update(cx, |view, _| view.next_tick_delay()) else {
                    break;
                };
                cx.background_executor().timer(delay).await;
                if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |view, cx| {
                view.tick_task = None;
                // Something may have started animating while the task wound down.
                view.schedule_tick(cx);
            });
        }));
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let blinking = self.focused && self.cursor_blink();
        let visible = !blinking
            || (self.blink_started.elapsed().as_millis() / BLINK_INTERVAL.as_millis())
                .is_multiple_of(2);
        if visible != self.blink_visible {
            self.blink_visible = visible;
            changed = true;
        }
        if self.scrollbar_alpha_changes() {
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn cursor_blink(&self) -> bool {
        self.emulator
            .cursor()
            .blink
            .unwrap_or(self.settings.cursor_blink)
    }

    fn set_focused(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        self.restart_blink();
        self.schedule_tick(cx);
        if self.emulator.focus_reporting() && !self.closed {
            let report = if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            };
            let _ = self.input.try_send(TransportInput::Data(report));
        }
        cx.notify();
    }

    /// Opacity of the scrollbar slider: shown while hovered or scrolling, hidden 500 ms later and
    /// faded over 800 ms.
    fn scrollbar_opacity(&self) -> f32 {
        if self.hovered {
            return 1.0;
        }
        let last = match (self.scrolled_at, self.left_at) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return 0.0,
        };
        let elapsed = last.elapsed();
        if elapsed < SCROLLBAR_HIDE_AFTER {
            return 1.0;
        }
        let fade = (elapsed - SCROLLBAR_HIDE_AFTER).as_secs_f32() / SCROLLBAR_FADE.as_secs_f32();
        (1.0 - fade).max(0.0)
    }

    fn scrollbar_alpha_changes(&self) -> bool {
        let opacity = self.scrollbar_opacity();
        opacity > 0.0 && opacity < 1.0 || {
            let last = self.scrolled_at.into_iter().chain(self.left_at).max();
            last.is_some_and(|l| {
                l.elapsed() < SCROLLBAR_HIDE_AFTER + SCROLLBAR_FADE + Duration::from_millis(150)
            })
        }
    }

    // ---- selection and search ----------------------------------------------------------

    fn selection_range(&self) -> Option<(BufferPoint, BufferPoint)> {
        self.selection
            .as_ref()
            .and_then(|s| s.range(&self.emulator))
    }

    fn select_result(&mut self, result: Option<SearchResult>, cx: &mut Context<Self>) {
        if let Some(result) = result {
            let cols = self.emulator.cols();
            let (start, end) = result.range(cols);
            let mut selection = Selection::begin(&self.emulator, SelectionMode::Normal, start)
                .expect("normal selection");
            selection.extend(&self.emulator, end);
            self.selection = Some(selection);
            // Bring the match into view, centred, when it is outside the viewport.
            let top = self.emulator.viewport_top();
            let rows = self.emulator.rows();
            if result.row < top || result.row >= top + rows {
                self.emulator
                    .scroll_to_line(result.row.saturating_sub(rows / 2));
            }
        } else {
            self.selection = None;
        }
        cx.notify();
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.selection_text();
        if text.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        match cx.read_from_clipboard() {
            Some(item) => match item.text() {
                Some(text) => self.paste(&text, cx),
                None => cx.emit(TerminalViewEvent::ClipboardWithoutText),
            },
            None => cx.emit(TerminalViewEvent::ClipboardWithoutText),
        }
    }

    // ---- pointer -------------------------------------------------------------------------

    /// Where menus opened "at the caret" go (`cursorAnchorPosition`): the middle of the
    /// cursor cell's bottom edge, in window coordinates. `None` before the first layout.
    pub fn cursor_anchor(&self) -> Option<Point<Pixels>> {
        let origin = self.grid_origin()?;
        if self.size.cols == 0 || self.size.rows == 0 {
            return None;
        }
        let cursor = self.emulator.cursor();
        let row = cursor
            .line
            .saturating_sub(self.emulator.viewport_top())
            .min(usize::from(self.size.rows) - 1);
        let column = cursor.col.min(usize::from(self.size.cols) - 1);
        Some(point(
            origin.x + layout_px(((column as f32 + 0.5) * self.cell.width).round()),
            origin.y + layout_px(((row + 1) as f32 * self.cell.height).round()),
        ))
    }

    fn grid_origin(&self) -> Option<Point<Pixels>> {
        let bounds = self.layout.get().bounds?;
        Some(point(
            bounds.origin.x + layout_px(inner_padding()),
            bounds.origin.y + layout_px(inner_padding()),
        ))
    }

    /// The cell under a window position: `(viewport row, column, x, y)` with pixels relative to the
    /// grid. Selection rounds to the nearer cell edge, as xterm does.
    fn cell_at(
        &self,
        position: Point<Pixels>,
        for_selection: bool,
    ) -> Option<(usize, usize, f32, f32)> {
        let origin = self.grid_origin()?;
        let x = f32::from(position.x - origin.x);
        let y = f32::from(position.y - origin.y);
        let cols = self.emulator.cols();
        let rows = self.emulator.rows();
        let adjusted = if for_selection {
            x + self.cell.width / 2.0
        } else {
            x
        };
        let col = (adjusted / self.cell.width).floor().clamp(
            0.0,
            if for_selection {
                cols as f32
            } else {
                cols as f32 - 1.0
            },
        ) as usize;
        let row = (y / self.cell.height).floor().clamp(0.0, rows as f32 - 1.0) as usize;
        Some((row, col, x.max(0.0), y.max(0.0)))
    }

    fn buffer_point(&self, position: Point<Pixels>, for_selection: bool) -> Option<BufferPoint> {
        let (row, col, _, y) = self.cell_at(position, for_selection)?;
        let mut line = self.emulator.viewport_top() + row;
        // Dragging above or below the grid selects into the scrollback.
        if y <= 0.0 && for_selection {
            line = self.emulator.viewport_top();
        }
        Some(BufferPoint::new(
            line.min(self.emulator.length().saturating_sub(1)),
            col,
        ))
    }

    fn mouse_reporting(&self, shift: bool) -> bool {
        // Shift forces a selection while a program reads the mouse.
        protocol_active(self.emulator.mouse_protocol()) && !shift
    }

    fn report_mouse(
        &mut self,
        position: Point<Pixels>,
        button: ReportButton,
        action: MouseAction,
        modifiers: &gpui_kit::Modifiers,
    ) -> bool {
        let Some((row, col, x, y)) = self.cell_at(position, false) else {
            return false;
        };
        let report = MouseReport {
            col,
            row,
            x: x.round() as usize,
            y: y.round() as usize,
            button,
            action,
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
        };
        let protocol = self.emulator.mouse_protocol();
        let encoding = self.emulator.mouse_encoding();
        if let Some(bytes) = self.mouse.report(
            protocol,
            encoding,
            report,
            self.emulator.cols(),
            self.emulator.rows(),
        ) && !self.closed
        {
            let _ = self.input.try_send(TransportInput::Data(bytes));
        }
        true
    }

    fn link_click(event_button: MouseButton, modifiers: &gpui_kit::Modifiers) -> LinkClick {
        LinkClick {
            button: match event_button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                _ => 2,
            },
            alt: modifiers.alt,
            ctrl: modifiers.control,
            meta: modifiers.platform,
            shift: modifiers.shift,
        }
    }

    /// The link under a cell: web and file links, then OSC 8 hyperlinks.
    fn link_under(&self, point: BufferPoint) -> Option<BufferLink> {
        if let Some(link) = link_at(&self.emulator, point, self.settings.file_links) {
            return Some(link);
        }
        let cells = self.emulator.styled_line(point.line)?;
        let id = cells.get(point.col)?.hyperlink?;
        let url = self.emulator.hyperlink_url(id)?;
        let start = (0..=point.col)
            .rev()
            .take_while(|c| cells[*c].hyperlink == Some(id))
            .last()?;
        let end = (point.col..cells.len())
            .take_while(|c| cells[*c].hyperlink == Some(id))
            .last()?;
        Some(BufferLink {
            target: LinkTarget::Web(url.clone()),
            start: BufferPoint::new(point.line, start),
            end: BufferPoint::new(point.line, end),
            text: url,
        })
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if event.button == MouseButton::Right {
            self.on_right_click(event, cx);
            if self.mouse_reporting(event.modifiers.shift) {
                self.report_mouse(
                    event.position,
                    ReportButton::Right,
                    MouseAction::Down,
                    &event.modifiers,
                );
            }
            return;
        }
        let button = match event.button {
            MouseButton::Left => ReportButton::Left,
            MouseButton::Middle => ReportButton::Middle,
            _ => return,
        };
        if self.mouse_reporting(event.modifiers.shift) {
            self.reporting_button = Some(button);
            self.report_mouse(event.position, button, MouseAction::Down, &event.modifiers);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let Some(point) = self.buffer_point(event.position, true) else {
            return;
        };
        if event.modifiers.shift
            && let Some(selection) = &mut self.selection
        {
            // Shift+click extends the selection.
            selection.extend(&self.emulator, point);
            self.selecting = true;
            cx.notify();
            return;
        }
        let mode = match event.click_count {
            2 => SelectionMode::Word,
            n if n >= 3 => SelectionMode::Line,
            _ if event.modifiers.alt && !self.settings.is_mac => SelectionMode::Column,
            _ => SelectionMode::Normal,
        };
        let anchor = if mode == SelectionMode::Normal || mode == SelectionMode::Column {
            point
        } else {
            self.buffer_point(event.position, false).unwrap_or(point)
        };
        self.selection = Selection::begin(&self.emulator, mode, anchor);
        self.selecting = true;
        cx.notify();
    }

    fn on_right_click(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let selection = self.selection_text();
        match terminal_right_click_intent(self.settings.right_click, &selection) {
            RightClickIntent::Copy { selection } => {
                cx.write_to_clipboard(ClipboardItem::new_string(selection));
                self.selection = None;
                cx.notify();
            }
            RightClickIntent::Paste => self.paste_clipboard(cx),
            RightClickIntent::Menu { selection } => {
                cx.emit(TerminalViewEvent::ContextMenu {
                    position: event.position,
                    selection,
                });
            }
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(button) = self.reporting_button {
            self.report_mouse(event.position, button, MouseAction::Move, &event.modifiers);
            return;
        }
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            if let Some(origin) = self.grid_origin() {
                // Dragging past the top or bottom scrolls.
                let y = f32::from(event.position.y - origin.y);
                let grid_height = self.emulator.rows() as f32 * self.cell.height;
                if y < 0.0 {
                    self.emulator.scroll_lines(-1);
                } else if y > grid_height {
                    self.emulator.scroll_lines(1);
                }
            }
            if let Some(point) = self.buffer_point(event.position, true) {
                let point = match self.selection.as_ref().map(|s| s.mode) {
                    Some(SelectionMode::Word | SelectionMode::Line) => {
                        self.buffer_point(event.position, false).unwrap_or(point)
                    }
                    _ => point,
                };
                if let Some(selection) = &mut self.selection {
                    selection.extend(&self.emulator, point);
                }
            }
            cx.notify();
            return;
        }
        if event.pressed_button.is_none() && self.mouse_reporting(event.modifiers.shift) {
            self.report_mouse(
                event.position,
                ReportButton::None,
                MouseAction::Move,
                &event.modifiers,
            );
        }
        let inside = self
            .layout
            .get()
            .bounds
            .is_some_and(|b| b.contains(&event.position));
        let link = if inside {
            self.buffer_point(event.position, false)
                .and_then(|p| self.link_under(p))
        } else {
            None
        };
        if link != self.hovered_link {
            self.hovered_link = link;
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if let Some(button) = self.reporting_button.take() {
            self.report_mouse(event.position, button, MouseAction::Up, &event.modifiers);
            return;
        }
        if event.button != MouseButton::Left || !self.selecting {
            return;
        }
        self.selecting = false;
        if !self.has_selection() {
            self.selection = None;
            // A click without a drag: open a link under the configured gesture.
            if let Some(point) = self.buffer_point(event.position, false)
                && let Some(link) = self.link_under(point)
            {
                let activation = link_activation_for_platform(
                    self.settings.link_activation,
                    self.settings.is_mac,
                );
                if link_activation_matches(
                    Self::link_click(event.button, &event.modifiers),
                    activation,
                ) {
                    match link.target {
                        LinkTarget::Web(url) => {
                            if let Some(url) = muxus_term::links::web_link_target(&url) {
                                cx.emit(TerminalViewEvent::OpenUrl(url));
                            }
                        }
                        LinkTarget::File(path) => cx.emit(TerminalViewEvent::OpenFile(path)),
                    }
                }
            }
        } else if self.settings.copy_on_select {
            self.copy_selection(cx);
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / self.cell.height,
        };
        if event.modifiers.control {
            // Ctrl+wheel zoom, the convention every terminal user tries first.
            if lines > 0.0 {
                self.zoom_in(cx);
            } else if lines < 0.0 {
                self.zoom_out(cx);
            }
            return;
        }
        self.scroll_remainder += lines;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        let amount = whole as isize;
        if amount == 0 {
            return;
        }
        let up = amount > 0;
        if self.mouse_reporting(event.modifiers.shift)
            && self.emulator.mouse_protocol() != muxus_term::mouse::MouseProtocol::X10
        {
            for _ in 0..amount.unsigned_abs() {
                let action = if up {
                    MouseAction::Down
                } else {
                    MouseAction::Up
                };
                self.report_mouse(
                    event.position,
                    ReportButton::Wheel,
                    action,
                    &event.modifiers,
                );
            }
            return;
        }
        if self.emulator.is_alt_screen() {
            // No scrollback in full-screen programs: the wheel becomes arrow keys.
            let prefix = if self.emulator.application_cursor() {
                "\x1bO"
            } else {
                "\x1b["
            };
            let key = format!("{prefix}{}", if up { 'A' } else { 'B' });
            if !self.closed {
                let _ = self.input.try_send(TransportInput::Data(
                    key.repeat(amount.unsigned_abs()).into_bytes(),
                ));
            }
            return;
        }
        self.emulator.scroll_lines(-amount);
        self.scrolled_at = Some(Instant::now());
        self.schedule_tick(cx);
        cx.notify();
    }

    // ---- keyboard ------------------------------------------------------------------------

    fn key_context(&self) -> KeyEncodeContext {
        KeyEncodeContext {
            application_cursor: self.emulator.application_cursor(),
            kitty_flags: self.emulator.kitty_keyboard_flags(),
            is_mac: self.settings.is_mac,
            mac_option_is_meta: false,
            is_windows: cfg!(windows),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let ev = keyboard_event(keystroke, event.is_held);
        match key_down(
            &ev,
            keystroke.key_char.as_deref(),
            &self.key_context(),
            true,
        ) {
            KeyAction::Send(data) => {
                cx.stop_propagation();
                self.user_input(data.into_bytes(), cx);
            }
            KeyAction::ScrollPage { up } => {
                cx.stop_propagation();
                let rows = self.emulator.rows() as isize - 1;
                self.emulator.scroll_lines(if up { -rows } else { rows });
                self.scrolled_at = Some(Instant::now());
                self.schedule_tick(cx);
                cx.notify();
            }
            KeyAction::SelectAll => {
                cx.stop_propagation();
                self.select_all(cx);
            }
            // Text arrives through the input handler (IME, dead keys, plain typing).
            KeyAction::Text | KeyAction::Ignored => {}
        }
    }

    // ---- actions -------------------------------------------------------------------------

    fn action_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selection(cx) {
            self.selection = None;
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn action_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(cx);
    }

    fn action_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all(cx);
    }

    fn action_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
    }

    fn action_find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.find_next(cx);
    }

    fn action_find_previous(&mut self, _: &FindPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.find_previous(cx);
    }

    fn action_close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        self.close_search(window, cx);
    }

    fn action_clear(&mut self, _: &ClearScrollback, _: &mut Window, cx: &mut Context<Self>) {
        self.clear(cx);
    }

    fn action_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_in(cx);
    }

    fn action_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_out(cx);
    }

    fn action_zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_reset(cx);
    }

    fn action_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.emulator
            .scroll_lines(-(self.emulator.rows() as isize - 1));
        cx.notify();
    }

    fn action_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.emulator
            .scroll_lines(self.emulator.rows() as isize - 1);
        cx.notify();
    }

    fn action_top(&mut self, _: &ScrollToTop, _: &mut Window, cx: &mut Context<Self>) {
        self.emulator.scroll_to_top();
        cx.notify();
    }

    fn action_bottom(&mut self, _: &ScrollToBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.emulator.scroll_to_bottom();
        cx.notify();
    }

    // ---- frame ---------------------------------------------------------------------------

    /// The image as a GPUI sprite at its painted size in device pixels: scaled with a bilinear
    /// filter like the canvas the image addon draws on (GPUI samples sprites without filtering).
    fn render_image(
        &mut self,
        id: u64,
        pixels: &[u8],
        width: usize,
        height: usize,
        target: (u32, u32),
    ) -> Option<Arc<RenderImage>> {
        if let Some((size, image)) = self.images.get(&id)
            && *size == target
        {
            return Some(image.clone());
        }
        if width == 0
            || height == 0
            || target.0 == 0
            || target.1 == 0
            || pixels.len() < width * height * 4
        {
            return None;
        }
        let source = image::RgbaImage::from_raw(
            width as u32,
            height as u32,
            pixels[..width * height * 4].to_vec(),
        )?;
        let mut scaled = if (width as u32, height as u32) == target {
            source
        } else {
            image::imageops::resize(
                &source,
                target.0,
                target.1,
                image::imageops::FilterType::Triangle,
            )
        };
        // GPUI's sprites are BGRA.
        for pixel in scaled.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            scaled
        )]));
        self.images.insert(id, (target, image.clone()));
        Some(image)
    }

    /// The "Find in terminal" box: a Paper at the top right with the query field, the match count,
    /// the case/word/regex toggles and previous/next/close buttons.
    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let dark = self.settings.app_dark;
        let chrome = SearchChrome::new(dark, cx.try_global::<muxus_ui::theme::Theme>());
        let (index, count) = self.search.result_index();
        let label = if self.search_term.is_empty() {
            None
        } else if count > 0 {
            Some(format!("{}/{}", index + 1, count))
        } else {
            Some("No matches".to_string())
        };
        let input = self.search_input.clone();
        let options = self.search_options;
        let toggle = |id: &'static str,
                      text: &'static str,
                      selected: bool,
                      cx: &mut Context<Self>,
                      apply: fn(&mut SearchOptions)| {
            div()
                .id(id)
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(8.0))
                .text_size(px(12.0))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(if selected {
                    chrome.text
                } else {
                    chrome.text_secondary
                })
                .when(selected, |el| el.bg(chrome.selected))
                .cursor(CursorStyle::PointingHand)
                .child(text)
                .on_click(cx.listener(move |view, _: &gpui_kit::ClickEvent, _, cx| {
                    view.toggle_search_option(apply, cx)
                }))
        };
        let icon_button =
            |id: &'static str,
             icon: IconName,
             enabled: bool,
             cx: &mut Context<Self>,
             action: fn(&mut Self, &mut Window, &mut Context<Self>)| {
                div()
                    .id(id)
                    .size(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .when(enabled, |el| {
                        el.cursor(CursorStyle::PointingHand)
                            .hover(|el| el.bg(chrome.hover))
                    })
                    .child(Icon::new(icon).size(px(20.0)).text_color(if enabled {
                        chrome.icon
                    } else {
                        chrome.disabled
                    }))
                    .on_click(
                        cx.listener(move |view, _: &gpui_kit::ClickEvent, window, cx| {
                            if enabled {
                                action(view, window, cx)
                            }
                        }),
                    )
            };
        let has_query = !self.search_term.is_empty();
        div()
            .absolute()
            .top(px(11.0))
            .right(px(13.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(4.0))
            .bg(chrome.paper)
            .border_1()
            .border_color(chrome.divider)
            .rounded(px(10.0))
            .shadow_lg()
            .font_family("Inter")
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    view.close_search(window, cx);
                }
            }))
            .child(
                div()
                    .w(px(240.0))
                    .h(px(37.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(12.0))
                    .pr(px(12.0))
                    .rounded(px(8.0))
                    .border_2()
                    .border_color(chrome.primary)
                    .text_size(px(14.0))
                    .text_color(chrome.text)
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(17.0))
                            .text_color(chrome.icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .children(input.map(|input| Input::new(&input).appearance(false))),
                    )
                    .children(label.map(|label| {
                        div()
                            .flex_none()
                            .text_size(px(12.0))
                            .text_color(chrome.text_secondary)
                            .child(label)
                    })),
            )
            .child(toggle(
                "search-case",
                "Aa",
                options.case_sensitive,
                cx,
                |o| o.case_sensitive = !o.case_sensitive,
            ))
            .child(toggle("search-word", "W", options.whole_word, cx, |o| {
                o.whole_word = !o.whole_word
            }))
            .child(toggle("search-regex", ".*", options.regex, cx, |o| {
                o.regex = !o.regex
            }))
            .child(icon_button(
                "search-previous",
                IconName::ChevronUp,
                has_query,
                cx,
                |view, _, cx| view.find_previous(cx),
            ))
            .child(icon_button(
                "search-next",
                IconName::ChevronDown,
                has_query,
                cx,
                |view, _, cx| view.find_next(cx),
            ))
            .child(icon_button(
                "search-close",
                IconName::Close,
                true,
                cx,
                |view, window, cx| view.close_search(window, cx),
            ))
    }

    /// What the painted rows are built from. The cursor is drawn on its own,
    /// so a blink of the cursor (the only change while a terminal idles) reuses
    /// the rows instead of resolving every cell again.
    fn rows_key(&self) -> RowsKey {
        let search = self.search_open.then(|| {
            let (index, count) = self.search.result_index();
            (self.search_term.clone(), self.search_options, index, count)
        });
        RowsKey {
            content: self.emulator.content_generation(),
            top: self.emulator.viewport_top(),
            rows: self.emulator.rows(),
            cols: self.emulator.cols(),
            alt: self.emulator.is_alt_screen(),
            colors: self.emulator.colors(),
            selection: self.selection.clone(),
            search,
            hovered: self.hovered_link.clone(),
            focused: self.focused,
            blink: None,
            settings: self.settings_version,
        }
    }

    /// The visible rows, resolved for painting; from the cache when nothing
    /// they depend on changed.
    fn frame_rows(&mut self) -> Rc<Vec<Vec<PaintCell>>> {
        let mut key = self.rows_key();
        if let Some(cached) = &self.rows_cache {
            if cached.has_blink {
                key.blink = Some(self.blink_visible);
            }
            if cached.key == key {
                return cached.rows.clone();
            }
            key.blink = None;
        }
        let (rows, has_blink) = self.build_rows();
        if has_blink {
            key.blink = Some(self.blink_visible);
        }
        let rows = Rc::new(rows);
        self.rows_cache = Some(RowsCache {
            key,
            rows: rows.clone(),
            has_blink,
        });
        rows
    }

    /// Resolve every visible cell: colors, selection, search matches, keyword
    /// highlights, hovered link. Also says whether any cell blinks.
    fn build_rows(&self) -> (Vec<Vec<PaintCell>>, bool) {
        let colors = self.emulator.colors();
        let rows = self.emulator.rows();
        let cols = self.emulator.cols();
        let top = self.emulator.viewport_top();
        let focused = self.focused;
        let decorations: Vec<KeywordDecoration> =
            if self.emulator.is_alt_screen() || self.settings.keyword_rules.is_empty() {
                Vec::new()
            } else {
                keyword_decorations(&self.emulator, top, rows, &self.settings.keyword_rules)
            };
        // Only the matches that reach the viewport are looked at per cell.
        let matches: Vec<&SearchResult> = if self.search_open {
            let cols = self.emulator.cols().max(1);
            self.search
                .results()
                .iter()
                .filter(|m| m.row + (m.col + m.size) / cols >= top && m.row < top + rows)
                .collect()
        } else {
            Vec::new()
        };
        let active_match = if self.search_open {
            self.search.active().cloned()
        } else {
            None
        };
        let match_bg = parse_color(MATCH_BACKGROUND);
        let active_bg = parse_color(ACTIVE_MATCH_BACKGROUND);
        let mut frame_rows = Vec::with_capacity(rows);
        let mut has_blink = false;
        for row in 0..rows {
            let line = top + row;
            let cells = self.emulator.styled_line(line).unwrap_or_default();
            let mut painted = Vec::with_capacity(cols);
            for (col, cell) in cells.iter().enumerate().take(cols) {
                let point = BufferPoint::new(line, col);
                let selected = self
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.contains(&self.emulator, point));
                let keyword = decorations
                    .iter()
                    .rev()
                    .find(|d| d.line == line && col >= d.col && col < d.col + d.width);
                let mut context = CellContext {
                    selected,
                    focused,
                    decoration_fg: keyword.map(|d| parse_color(&d.foreground)),
                    decoration_bg: keyword.and_then(|d| d.background.as_deref().map(parse_color)),
                    decoration_top: false,
                    minimum_contrast_ratio: self.settings.minimum_contrast_ratio,
                };
                if active_match
                    .as_ref()
                    .is_some_and(|m| m.contains(point, cols))
                {
                    context.decoration_bg = Some(active_bg);
                    context.decoration_top = true;
                } else if matches.iter().any(|m| m.contains(point, cols)) {
                    context.decoration_bg = Some(match_bg);
                }
                let resolved = resolve_cell(cell, &colors, &context);
                let link = self
                    .hovered_link
                    .as_ref()
                    .is_some_and(|l| l.contains(point));
                has_blink |= cell.style.blink;
                let blink_hidden = cell.style.blink && !self.blink_visible;
                let text = if cell.text.is_empty() || cell.style.invisible || blink_hidden {
                    " ".to_string()
                } else {
                    cell.text.clone()
                };
                painted.push(PaintCell {
                    text,
                    width: cell.width,
                    fg: hsla(resolved.fg),
                    bg: resolved.bg.map(hsla),
                    underline: cell.style.underline,
                    underline_color: hsla(resolved.underline),
                    bold: cell.style.bold,
                    italic: cell.style.italic,
                    strikethrough: cell.style.strikethrough,
                    overline: cell.style.overline,
                    link,
                });
            }
            while painted.len() < cols {
                painted.push(PaintCell {
                    text: " ".into(),
                    width: 1,
                    fg: hsla(colors.foreground),
                    bg: None,
                    underline: muxus_term::style::UnderlineStyle::None,
                    underline_color: hsla(colors.foreground),
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    overline: false,
                    link: false,
                });
            }
            frame_rows.push(painted);
        }
        (frame_rows, has_blink)
    }

    fn build_frame(&mut self) -> Frame {
        let colors = self.emulator.colors();
        let rows = self.emulator.rows();
        let top = self.emulator.viewport_top();
        let focused = self.focused;
        let active_match = if self.search_open {
            self.search.active().cloned()
        } else {
            None
        };
        let frame_rows = self.frame_rows();
        let state = self.emulator.cursor();
        let cursor =
            (state.visible && !self.closed && state.line >= top && state.line < top + rows).then(
                || {
                    let under = self
                        .emulator
                        .styled_line(state.line)
                        .and_then(|cells| cells.get(state.col).cloned())
                        .unwrap_or_default();
                    FrameCursor {
                        row: state.line - top,
                        col: state.col,
                        width: under.width.max(1),
                        shape: state.shape.unwrap_or(self.settings.cursor_style),
                        focused,
                        hidden: !self.blink_visible,
                        color: hsla(colors.cursor),
                        accent: hsla(colors.cursor_accent),
                        text: if under.text.is_empty() {
                            " ".into()
                        } else {
                            under.text.clone()
                        },
                        bold: under.style.bold,
                        italic: under.style.italic,
                    }
                },
            );

        let mut images = Vec::new();
        let mut live = std::collections::HashSet::new();
        for placement in self.emulator.images() {
            live.insert(placement.id);
            let row = placement.line - top as isize;
            let (width, height) = match placement.cells {
                Some((c, r)) => (c as f32 * self.cell.width, r as f32 * self.cell.height),
                None => (placement.width as f32, placement.height as f32),
            };
            if row + ((height / self.cell.height).ceil() as isize) < 0 || row > rows as isize {
                continue;
            }
            let target = (
                (width * self.scale_factor).round() as u32,
                (height * self.scale_factor).round() as u32,
            );
            if let Some(image) = self.render_image(
                placement.id,
                &placement.pixels,
                placement.width,
                placement.height,
                target,
            ) {
                images.push(FrameImage {
                    row,
                    col: placement.col,
                    image,
                    width,
                    height,
                    below_text: placement.z_index < 0,
                });
            }
        }
        self.images.retain(|id, _| live.contains(id));

        let length = self.emulator.length();
        let scrollback = self.emulator.scrollback_len();
        let opacity = self.scrollbar_opacity();
        let scrollbar =
            (scrollback > 0 && opacity > 0.0 && self.settings.scrollback > 0).then(|| {
                let slider = colors
                    .foreground
                    .with_alpha((0.2 * 255.0 * opacity).round() as u8);
                Scrollbar {
                    top: top as f32 / length as f32,
                    height: rows as f32 / length as f32,
                    color: hsla(slider),
                }
            });

        let mut ruler = Vec::new();
        if !self.emulator.is_alt_screen() && length > 0 {
            let fraction = |line: usize| line as f32 / length as f32;
            for line in self.emulator.failed_command_lines() {
                ruler.push(RulerMark {
                    start: fraction(line),
                    end: fraction(line),
                    lane: RulerLane::Left,
                    color: hsla(parse_color(ERROR_MARK_COLOR)),
                });
            }
            for line in self
                .search
                .highlighted_lines()
                .into_iter()
                .filter(|_| self.search_open)
            {
                ruler.push(RulerMark {
                    start: fraction(line),
                    end: fraction(line),
                    lane: RulerLane::Center,
                    color: hsla(parse_color(MATCH_OVERVIEW_RULER)),
                });
            }
            if let Some(active) = &active_match {
                ruler.push(RulerMark {
                    start: fraction(active.row),
                    end: fraction(active.row),
                    lane: RulerLane::Center,
                    color: hsla(parse_color(ACTIVE_MATCH_OVERVIEW_RULER)),
                });
            }
        }

        Frame {
            rows: frame_rows,
            cell: self.cell,
            fonts: self.fonts.clone(),
            background: hsla(colors.background),
            cursor,
            images,
            scrollbar,
            ruler,
            buffer_lines: length,
            preedit: self.preedit.clone(),
        }
    }
}

/// Colors of the search box, from the Muxus palette (`client/src/theme.ts`).
#[derive(Clone, Copy)]
struct SearchChrome {
    paper: gpui_kit::Hsla,
    divider: gpui_kit::Hsla,
    primary: gpui_kit::Hsla,
    text: gpui_kit::Hsla,
    text_secondary: gpui_kit::Hsla,
    icon: gpui_kit::Hsla,
    disabled: gpui_kit::Hsla,
    selected: gpui_kit::Hsla,
    hover: gpui_kit::Hsla,
}

impl SearchChrome {
    /// The app theme's paper, ink and overlays (MUI's `Paper` and icon buttons).
    fn new(dark: bool, theme: Option<&muxus_ui::theme::Theme>) -> Self {
        let fallback;
        let theme = match theme.filter(|theme| theme.mode.is_dark() == dark) {
            Some(theme) => theme,
            None => {
                fallback = muxus_ui::theme::Theme::new(if dark {
                    muxus_ui::theme::Mode::Dark
                } else {
                    muxus_ui::theme::Mode::Light
                });
                &fallback
            }
        };
        let p = &theme.palette;
        let ink = if dark { p.common_white } else { p.common_black };
        let fade = |amount: f32| gpui_kit::Hsla { a: amount, ..ink };
        Self {
            paper: p.background_paper,
            divider: p.divider,
            primary: p.primary.main,
            text: p.text.primary,
            text_secondary: p.text.secondary,
            icon: if dark { fade(0.56) } else { p.action.active },
            disabled: fade(0.26),
            selected: p.action.selected,
            hover: p.action.hover,
        }
    }
}

fn line_ending() -> &'static str {
    if cfg!(windows) { "\r\n" } else { "\n" }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_metrics(window);
        // Focus is read at paint time: focus events that fire before the view is first drawn
        // would otherwise be missed. An inactive window shows the inactive cursor, as a blurred
        // xterm does.
        let focused = self.focus.is_focused(window) && window.is_window_active();
        if focused != self.focused {
            self.focused = focused;
            self.restart_blink();
            self.schedule_tick(cx);
        }
        let frame = self.build_frame();
        let colors = self.emulator.colors();
        let background = hsla(colors.background);
        let scheme_light = self.settings.theme_is_light;
        let border = if self.settings.app_dark && !scheme_light {
            gpui_kit::transparent_black()
        } else if self.settings.app_dark {
            hsla(Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 20,
            })
        } else {
            hsla(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 20,
            })
        };
        let layout = self.layout.clone();
        let entity = cx.entity();
        let weak = entity.downgrade();
        let focus = self.focus.clone();
        let current = (self.emulator.cols() as u16, self.emulator.rows() as u16);
        let cell = self.cell;
        let scrollback = self.settings.scrollback;
        let mouse_cursor = if self.hovered_link.is_some() {
            CursorStyle::PointingHand
        } else if protocol_active(self.emulator.mouse_protocol()) {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };

        let grid = canvas(
            move |bounds, window, cx| {
                layout.set(Layout {
                    bounds: Some(bounds),
                });
                // FitAddon measures the container's computed size, which under
                // MUI's `box-sizing: border-box` includes its 1px border on each
                // side; the grid is fitted to that, as in the client.
                let width = f32::from(bounds.size.width) + 2.0 * css(CONTAINER_BORDER)
                    - 2.0 * inner_padding();
                let height = f32::from(bounds.size.height) + 2.0 * css(CONTAINER_BORDER)
                    - 2.0 * inner_padding();
                if should_fit_terminal(Some((width, height))) {
                    let scrollbar = if scrollback == 0 {
                        0.0
                    } else {
                        scrollbar_width()
                    };
                    let fitted = fit_grid(
                        width - scrollbar,
                        height,
                        muxus_term::fit::CellMetrics {
                            width: cell.width,
                            height: cell.height,
                        },
                    );
                    if let Some(size) = fitted
                        && size != current
                    {
                        let weak = weak.clone();
                        window.defer(cx, move |_, cx| {
                            let _ =
                                weak.update(cx, |view, cx| view.request_resize(size.0, size.1, cx));
                        });
                    }
                }
            },
            move |bounds, _, window, cx| {
                paint_frame(&frame, bounds, window, cx);
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                let view = entity.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |view, cx| view.on_mouse_move(event, cx));
                    }
                });
                let view = entity.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |view, cx| view.on_mouse_up(event, cx));
                    }
                });
            },
        )
        .size_full();

        let search_bar = self.search_open.then(|| self.render_search_bar(cx));
        div()
            .relative()
            .size_full()
            .pt(px(6.0))
            .px(px(8.0))
            .pb(px(8.0))
            .child(
                div()
                    .id("terminal")
                    .key_context("Terminal")
                    .track_focus(&self.focus)
                    .size_full()
                    .bg(background)
                    .border_1()
                    .border_color(border)
                    .rounded(px(8.0))
                    .overflow_hidden()
                    .cursor(mouse_cursor)
                    .on_action(cx.listener(Self::action_copy))
                    .on_action(cx.listener(Self::action_paste))
                    .on_action(cx.listener(Self::action_select_all))
                    .on_action(cx.listener(Self::action_find))
                    .on_action(cx.listener(Self::action_find_next))
                    .on_action(cx.listener(Self::action_find_previous))
                    .on_action(cx.listener(Self::action_close_find))
                    .on_action(cx.listener(Self::action_clear))
                    .on_action(cx.listener(Self::action_zoom_in))
                    .on_action(cx.listener(Self::action_zoom_out))
                    .on_action(cx.listener(Self::action_zoom_reset))
                    .on_action(cx.listener(Self::action_page_up))
                    .on_action(cx.listener(Self::action_page_down))
                    .on_action(cx.listener(Self::action_top))
                    .on_action(cx.listener(Self::action_bottom))
                    .on_key_down(cx.listener(Self::on_key_down))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
                    .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| {
                        view.on_scroll(event, cx)
                    }))
                    .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                        view.hovered = *hovered;
                        if !*hovered {
                            view.left_at = Some(Instant::now());
                            view.schedule_tick(cx);
                        }
                        cx.notify();
                    }))
                    .child(grid),
            )
            .children(search_bar)
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.preedit
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = None;
        self.user_input(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        new_text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The IME window opens at the cursor.
        let cursor = self.emulator.cursor();
        let row = cursor.line.checked_sub(self.emulator.viewport_top())?;
        let origin = element_bounds.origin
            + point(
                layout_px(inner_padding() + cursor.col as f32 * self.cell.width),
                layout_px(inner_padding() + row as f32 * self.cell.height),
            );
        Some(Bounds::new(
            origin,
            gpui_kit::size(layout_px(self.cell.width), layout_px(self.cell.height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// Open a terminal view in an entity.
pub fn new_terminal_view(
    channels: TransportChannels,
    settings: TerminalSettings,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalView> {
    cx.new(|cx| TerminalView::new(channels, settings, window, cx))
}
