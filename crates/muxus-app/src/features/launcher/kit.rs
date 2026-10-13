//! What the launcher, workspace, history and command button screens share
//! beyond the `muxus-ui` widgets: text in the theme's typography tiers, the
//! toast hook, and the confirm/prompt dialog the screens ask with
//! (`confirmAction` / `promptForText`, built from `muxus-ui`'s `Dialog`).

use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div,
};
use muxus_ui::label::{Label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, TextVariant, Theme, fonts};
use muxus_ui::widgets::typography::{self, VariantExt};
use muxus_ui::widgets::{Button, ButtonColor, Chip, Dialog, DialogWidth, TextField};

pub use muxus_ui::widgets::tooltip::follow_anchor;

/// A line height on the device-pixel grid of a 2× display. GPUI rounds a
/// text line's box up to whole device pixels, so Chrome's 18.59px body2 line
/// would become 19px and every row would grow; the nearest half pixel keeps
/// stacked rows within a fraction of Chrome's positions.
pub fn snap(line_height: f32) -> f32 {
    (line_height * 2.).round() / 2.
}

/// One line of text in a typography tier (`muxus-ui`'s `Typography`, with
/// the line height on the half-pixel grid; ellipsized when it does not fit).
pub fn text(content: impl Into<SharedString>, variant: TextVariant, color: Hsla) -> Label {
    typography::text(
        content,
        variant.line_height(snap(variant.line_height)),
        color,
    )
}

/// Like [`text`], in another weight (`fontWeight: 600` on a body2 line).
pub fn text_weight(
    content: impl Into<SharedString>,
    variant: TextVariant,
    weight: f32,
    color: Hsla,
) -> Label {
    text(content, variant, color).weight(weight)
}

/// `Typography variant="overline"`: uppercase caption type on its tall
/// line (MUI only adds letter spacing for its default font).
pub fn overline(content: &str, theme: &Theme, color: Hsla) -> Label {
    text(content.to_uppercase(), theme.typography.overline, color)
}

/// `<Box component="kbd" sx={chordSx(10)}>`: a bare chord in the mono tier.
pub fn kbd(content: impl Into<SharedString>, theme: &Theme, color: Hsla) -> Label {
    text(content, theme.typography.chord_small, color)
        .family(fonts::MONO)
        .no_truncate()
}

/// `<Chip size="small" variant="outlined" sx={chordSx(10)}>`: a chord in a pill.
pub fn chord_chip(content: impl Into<SharedString>, theme: &Theme) -> Chip {
    Chip::new(content)
        .outlined()
        .mono()
        .font_size(theme.typography.chord_small.size)
}

/// A toast host: shows `message` with `status`.
pub type ToastFn = dyn Fn(Status, SharedString, &mut App);

/// What the screens hand the window's toast host (`showToast`).
#[derive(Clone)]
pub struct ToastHook(pub Rc<ToastFn>);

impl Global for ToastHook {}

/// Register the window's toast host. Without one, toasts go to stderr.
pub fn set_toast_hook(hook: impl Fn(Status, SharedString, &mut App) + 'static, cx: &mut App) {
    cx.set_global(ToastHook(Rc::new(hook)));
}

/// `showToast(severity, message)`.
pub fn show_toast(status: Status, message: impl Into<SharedString>, cx: &mut App) {
    let message = message.into();
    match cx.try_global::<ToastHook>().cloned() {
        Some(hook) => (hook.0)(status, message, cx),
        None => eprintln!("[{status:?}] {message}"),
    }
}

/// `showErrorToast(error)`.
pub fn show_error_toast(message: impl Into<SharedString>, cx: &mut App) {
    show_toast(Status::Error, message, cx);
}

/// A confirmation or a one-line prompt (`confirmAction` / `promptForText`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskRequest {
    pub title: String,
    pub description: Option<String>,
    pub confirm_label: String,
    /// Paints the confirm button as destructive.
    pub destructive: bool,
    /// A text field: its label, start value and placeholder.
    pub prompt: Option<PromptField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptField {
    pub label: String,
    pub initial_value: String,
    pub placeholder: Option<String>,
    /// Lets an empty value be submitted.
    pub allow_empty: bool,
    /// Longest accepted value, in UTF-16 units (`validate` in the callers).
    pub max_len: Option<usize>,
}

impl AskRequest {
    pub fn confirm(
        title: impl Into<String>,
        description: impl Into<String>,
        confirm_label: impl Into<String>,
    ) -> Self {
        let description = description.into();
        Self {
            title: title.into(),
            description: (!description.is_empty()).then_some(description),
            confirm_label: confirm_label.into(),
            destructive: false,
            prompt: None,
        }
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

/// The answer to an [`AskRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskAnswer {
    Confirmed,
    /// The prompt's value (trimmed).
    Value(String),
    Cancelled,
}

/// The dialog that asks (one at a time, over the screen that asked).
pub struct AskDialog {
    pub request: AskRequest,
    input: Option<Entity<InputState>>,
    focus: FocusHandle,
    _subscription: Option<Subscription>,
}

impl EventEmitter<AskAnswer> for AskDialog {}

impl Focusable for AskDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AskDialog {
    pub fn new(request: AskRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let (input, subscription) = match &request.prompt {
            Some(prompt) => {
                let value = prompt.initial_value.clone();
                let input = cx.new(|cx| {
                    let mut state = InputState::new(window, cx);
                    state.set_value(value, window, cx);
                    state
                });
                let subscription = cx.subscribe(
                    &input,
                    |this: &mut Self, _, event: &InputEvent, cx| match event {
                        InputEvent::PressEnter { .. } => this.submit(cx),
                        _ => cx.notify(),
                    },
                );
                input.update(cx, |state, cx| {
                    state.focus(window, cx);
                    state.select_all(window, cx);
                });
                (Some(input), Some(subscription))
            }
            None => {
                window.focus(&focus, cx);
                (None, None)
            }
        };
        Self {
            request,
            input,
            focus,
            _subscription: subscription,
        }
    }

    fn value(&self, cx: &App) -> String {
        self.input
            .as_ref()
            .map(|input| muxus_core::js::js_trim(&input.read(cx).value()).to_owned())
            .unwrap_or_default()
    }

    fn invalid(&self, cx: &App) -> Option<String> {
        let prompt = self.request.prompt.as_ref()?;
        let value = self.value(cx);
        if let Some(max) = prompt.max_len
            && muxus_core::js::utf16_len(&value) > max
        {
            return Some(format!("Use at most {max} characters."));
        }
        None
    }

    fn can_submit(&self, cx: &App) -> bool {
        match &self.request.prompt {
            None => true,
            Some(prompt) => {
                self.invalid(cx).is_none() && (prompt.allow_empty || !self.value(cx).is_empty())
            }
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        if self.request.prompt.is_some() {
            let value = self.value(cx);
            cx.emit(AskAnswer::Value(value));
        } else {
            cx.emit(AskAnswer::Confirmed);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" if self.request.prompt.is_none() => {
                cx.stop_propagation();
                self.submit(cx);
            }
            "escape" => {
                cx.stop_propagation();
                cx.emit(AskAnswer::Cancelled);
            }
            _ => {}
        }
    }
}

impl Render for AskDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let entity = cx.entity().downgrade();
        let (cancel, commit, close) = (entity.clone(), entity.clone(), entity);
        let mut dialog = Dialog::new()
            .title(self.request.title.clone())
            .max_width(DialogWidth::Xs)
            .full_width()
            .on_close(move |_, cx| {
                let _ = close.update(cx, |_, cx| cx.emit(AskAnswer::Cancelled));
            });
        if let Some(description) = &self.request.description {
            let body2 = theme.typography.body2;
            dialog = dialog.content(
                paragraph(description.clone())
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            );
        }
        if let (Some(prompt), Some(input)) = (&self.request.prompt, &self.input) {
            let mut field = TextField::new("ask-prompt-field", input)
                .label(prompt.label.clone())
                .full_width();
            if let Some(placeholder) = &prompt.placeholder {
                field = field.placeholder(placeholder.clone());
            }
            dialog = dialog.content(div().mt(Theme::spacing(2.)).child(field));
            if let Some(error) = self.invalid(cx) {
                dialog = dialog.content(text(error, theme.typography.caption, p.error.main));
            }
        }
        let can_submit = self.can_submit(cx);
        dialog = dialog
            .action(
                Button::new("ask-cancel", "Cancel").on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(AskAnswer::Cancelled));
                }),
            )
            .action(
                Button::new("ask-commit", self.request.confirm_label.clone())
                    .contained()
                    .disabled(!can_submit)
                    .color(if self.request.destructive {
                        ButtonColor::Error
                    } else {
                        ButtonColor::Primary
                    })
                    .on_click(move |_, _, cx| {
                        let _ = commit.update(cx, |this, cx| this.submit(cx));
                    }),
            );
        div()
            .id("ask-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(dialog)
    }
}
