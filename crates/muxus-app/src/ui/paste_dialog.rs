//! The multi-line paste preview: `client/src/components/PasteConfirmDialog.tsx`.
//!
//! A paste that spans lines may run several commands at once, so it is shown
//! first in an editor with a line-number gutter, where it can be changed, and
//! the delay after each line can be set for this paste.

use muxus_core::paste_pacing::{PastePacing, paste_pacing_duration_ms, prepare_paste_text};
use muxus_term_view::muxus_term::font_catalog::terminal_font_stack;
use muxus_term_view::muxus_term::paste::{
    PasteDelayKind, format_paste_duration, parse_paste_delay, paste_line_count,
    paste_line_number_window,
};
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState, Textarea, TextareaState};
use muxus_ui::gpui_kit::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, px, relative,
};
use muxus_ui::label::paragraph;
use muxus_ui::theme::{ActiveTheme, fonts};
use muxus_ui::widgets::text_field::style_input;
use muxus_ui::widgets::{Button, ButtonColor, Dialog, DialogWidth, TextField};

/// `EDITOR_PADDING_Y_PX`.
const EDITOR_PADDING_Y: f32 = 8.5;
const MIN_ROWS: usize = 8;
const MAX_ROWS: usize = 18;

/// What the preview asks the window to do.
#[derive(Clone, Debug, PartialEq)]
pub enum PasteDialogEvent {
    Confirm { text: String, line_delay_ms: u32 },
    Cancel,
}

/// What the pace means for this paste, or how to slow it down (`pacingNote`).
pub fn pacing_note(text: &str, pacing: PastePacing, mirrored: usize) -> String {
    let note = if pacing.line_delay_ms == 0 && pacing.char_delay_ms == 0 {
        "Sent all at once. A line delay paces it for consoles that drop input.".to_string()
    } else {
        let duration = paste_pacing_duration_ms(&prepare_paste_text(text, false), pacing);
        let chars = if pacing.char_delay_ms > 0 {
            format!(", with {} ms after each character", pacing.char_delay_ms)
        } else {
            String::new()
        };
        format!("Takes {}{chars}.", format_paste_duration(duration as f64))
    };
    if mirrored > 0 {
        format!("{note} Mirrored terminals paste at their own host's pace.")
    } else {
        note
    }
}

/// The title: how many lines are about to go in.
pub fn paste_title(text: &str) -> String {
    let lines = if text.is_empty() {
        0
    } else {
        paste_line_count(text)
    };
    if lines == 0 {
        "Nothing to paste".into()
    } else {
        format!("Paste {lines} lines into the terminal?")
    }
}

pub struct PasteDialog {
    pub tab_id: String,
    editor: Entity<TextareaState>,
    delay: Entity<InputState>,
    pacing: PastePacing,
    mirrored: usize,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PasteDialogEvent> for PasteDialog {}

impl Focusable for PasteDialog {
    fn focus_handle(&self, _: &muxus_ui::gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PasteDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab_id: String,
        text: String,
        pacing: PastePacing,
        mirrored: usize,
        font_family: &str,
        font_size: f32,
        line_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .auto_grow(MIN_ROWS, MAX_ROWS)
                .soft_wrap(false);
            state.set_value(text, window, cx);
            state
        });
        let delay = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(pacing.line_delay_ms.to_string(), window, cx);
            state
        });
        let subscriptions = vec![
            cx.observe(&editor, |_, _, cx| cx.notify()),
            cx.subscribe(&delay, |this, _, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => cx.notify(),
            }),
        ];
        let focus = cx.focus_handle();
        // The Paste button has the keyboard in the client: Enter pastes at once.
        window.focus(&focus, cx);
        Self {
            tab_id,
            editor,
            delay,
            pacing,
            mirrored,
            font_family: terminal_font_stack(font_family)
                .first()
                .cloned()
                .unwrap_or_else(|| fonts::MONO.to_string())
                .into(),
            font_size,
            line_height,
            focus,
            _subscriptions: subscriptions,
        }
    }

    fn text(&self, cx: &muxus_ui::gpui_kit::App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn line_delay_ms(&self, cx: &muxus_ui::gpui_kit::App) -> u32 {
        parse_paste_delay(&self.delay.read(cx).value(), PasteDelayKind::Line).unwrap_or(0)
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if text.is_empty() {
            return;
        }
        let line_delay_ms = self.line_delay_ms(cx);
        cx.emit(PasteDialogEvent::Confirm {
            text,
            line_delay_ms,
        });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(PasteDialogEvent::Cancel);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" {
            cx.stop_propagation();
            self.cancel(cx);
        } else if keystroke.key == "enter" {
            let editing = self.editor.read(cx).focus_handle(cx).is_focused(window);
            // Enter adds a line while editing; Ctrl+Enter (⌘Enter) pastes.
            if !editing || keystroke.modifiers.control || keystroke.modifiers.platform {
                cx.stop_propagation();
                self.confirm(cx);
            }
        }
    }
}

impl Render for PasteDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        style_input(&self.delay, &theme, cx);
        let text = self.text(cx);
        let line_count = if text.is_empty() {
            0
        } else {
            paste_line_count(&text)
        };
        let visible_lines = line_count.max(1);
        let gutter_width =
            (30. + visible_lines.to_string().len() as f32 * self.font_size * 0.65).ceil();
        let line_px = self.font_size * self.line_height;
        let editor_state = self.editor.read(cx);
        let scroll_top = -muxus_ui::zoom::unzoom(editor_state.scroll_offset().y);
        let line_px = editor_state.line_height().map(f32::from).unwrap_or(line_px);
        let numbers = paste_line_number_window(
            visible_lines as f64,
            f64::from(scroll_top),
            Some(f64::from(line_px)),
        );
        let delay_ms = self.line_delay_ms(cx);
        let note = pacing_note(
            &text,
            PastePacing {
                line_delay_ms: delay_ms,
                ..self.pacing
            },
            self.mirrored,
        );
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let editor_focused = self
            .editor
            .read(cx)
            .focus_handle(cx)
            .contains_focused(_window, cx);

        let gutter = div()
            .absolute()
            .top(px(1.))
            .bottom(px(1.))
            .left(px(1.))
            .w(px(gutter_width))
            .overflow_hidden()
            .bg(p.action.hover)
            .border_r_1()
            .border_color(p.divider)
            .rounded_l(px(3.))
            .child(
                div()
                    .absolute()
                    .top(px(EDITOR_PADDING_Y - 1. - numbers.offset_px as f32))
                    .right(px(10.))
                    .flex()
                    .flex_col()
                    .items_end()
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .line_height(px(line_px))
                    .text_color(p.text.disabled)
                    .children(
                        numbers
                            .labels
                            .lines()
                            .map(|label| div().child(label.to_string())),
                    ),
            );
        let editor = div()
            .relative()
            .w(relative(1.))
            .rounded(px(8.))
            .border_1()
            .border_color(if editor_focused {
                p.primary.main
            } else {
                p.divider
            })
            .when_focused_border(editor_focused, p.primary.main)
            .child(gutter)
            .child(
                div()
                    .pl(px(gutter_width + 12.))
                    .pr(px(14.))
                    .py(px(EDITOR_PADDING_Y))
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .line_height(px(line_px))
                    .text_color(p.text.primary)
                    .child(Textarea::new(&self.editor)),
            );

        let delay_field = TextField::new("paste-line-delay", &self.delay)
            .label("Delay after each line")
            .end_text("ms")
            .width(200.);

        let entity = cx.entity().downgrade();
        let confirm_entity = entity.clone();
        let cancel_entity = entity.clone();
        let close_entity = entity;
        div()
            .id("paste-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(
                Dialog::new()
                    .title(paste_title(&text))
                    .max_width(DialogWidth::Md)
                    .full_width()
                    .on_close(move |_, cx| {
                        let _ = close_entity.update(cx, |this, cx| this.cancel(cx));
                    })
                    .content(
                        div().mb(px(12.)).child(
                            paragraph(
                                "Multiple lines may run several commands immediately. Review or edit the content before sending it.",
                            )
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.secondary),
                        ),
                    )
                    .content(editor)
                    .content(
                        div().mt(px(8.)).child(
                            paragraph(
                                "Enter adds a new line while editing. Press Ctrl+Enter or ⌘Enter to paste.",
                            )
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                        ),
                    )
                    .content(
                        div()
                            .mt(px(16.))
                            .flex()
                            .items_center()
                            .gap(px(16.))
                            .child(delay_field)
                            .child(
                                div().flex_1().min_w_0().child(
                                    paragraph(note)
                                        .size(caption.size)
                                        .line_height(caption.line_height)
                                        .color(p.text.secondary),
                                ),
                            ),
                    )
                    .action(Button::new("paste-cancel", "Cancel").on_click(move |_, _, cx| {
                        let _ = cancel_entity.update(cx, |this, cx| this.cancel(cx));
                    }))
                    .action(
                        Button::new("paste-confirm", "Paste")
                            .contained()
                            .color(ButtonColor::Warning)
                            .disabled(text.is_empty())
                            .on_click(move |_, _, cx| {
                                let _ = confirm_entity.update(cx, |this, cx| this.confirm(cx));
                            }),
                    ),
            )
    }
}

trait FocusBorder: Styled + Sized {
    /// The 2px focus ring of an outlined field.
    fn when_focused_border(self, focused: bool, color: muxus_ui::gpui_kit::Hsla) -> Self {
        if focused {
            self.border_2().border_color(color)
        } else {
            self
        }
    }
}

impl FocusBorder for muxus_ui::gpui_kit::Div {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_the_preview_by_line_count() {
        assert_eq!(paste_title(""), "Nothing to paste");
        assert_eq!(paste_title("a\nb"), "Paste 2 lines into the terminal?");
    }

    #[test]
    fn explains_the_pace() {
        assert_eq!(
            pacing_note("a\nb", PastePacing::default(), 0),
            "Sent all at once. A line delay paces it for consoles that drop input."
        );
        assert_eq!(
            pacing_note(
                "a\nb",
                PastePacing {
                    line_delay_ms: 0,
                    char_delay_ms: 0
                },
                2
            ),
            "Sent all at once. A line delay paces it for consoles that drop input. Mirrored terminals paste at their own host's pace."
        );
        assert!(
            pacing_note(
                "a\nb\nc",
                PastePacing {
                    line_delay_ms: 500,
                    char_delay_ms: 0
                },
                0
            )
            .starts_with("Takes ")
        );
    }
}
