//! Questions the file browser and the editor ask: the `confirmAction` and
//! `promptForText` halves of `client/src/components/DialogHost.tsx`. Every
//! question has the same anatomy: title, plain-language consequence, Cancel
//! on the left of the committing button. Enter commits, Escape keeps things
//! as they are.
//!
//! A view keeps a [`DialogQueue`], asks through it (each question resolves a
//! future) and draws [`DialogQueue::render`] somewhere in its tree. The
//! dialog covers the whole window, like MUI's modal, wherever that is.

use muxus_ui::widgets::form_field::FormField;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Styled, Subscription, Window, div, px,
    relative,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::dialog::dialog_paper;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::{Button, ButtonColor, Checkbox};

/// `ConfirmOptions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmOptions {
    pub title: String,
    pub description: Option<String>,
    pub confirm_label: String,
    pub cancel_label: Option<String>,
    /// Paints the confirm button as destructive and keeps Cancel the safe default.
    pub destructive: bool,
    /// A box to tick (e.g. "don't ask again"); its state comes back with the answer.
    pub checkbox: Option<String>,
}

impl Default for ConfirmOptions {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: None,
            confirm_label: "Confirm".into(),
            cancel_label: None,
            destructive: false,
            checkbox: None,
        }
    }
}

/// How a confirmation ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfirmAnswer {
    pub confirmed: bool,
    /// The checkbox was ticked when the user confirmed.
    pub checked: bool,
}

/// Returns a message that blocks submission, or `None` when the value is usable.
pub type Validator = Rc<dyn Fn(&str) -> Option<String>>;

/// `PromptOptions`.
#[derive(Clone)]
pub struct PromptOptions {
    pub title: String,
    pub description: Option<String>,
    pub label: String,
    pub initial_value: String,
    pub placeholder: Option<String>,
    pub confirm_label: String,
    /// Lets an empty value be submitted, e.g. to clear an optional name.
    pub allow_empty: bool,
    pub validate: Option<Validator>,
    /// A password field: the value is hidden and returned exactly as typed, untrimmed.
    pub masked: bool,
}

impl Default for PromptOptions {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: None,
            label: String::new(),
            initial_value: String::new(),
            placeholder: None,
            confirm_label: "Save".into(),
            allow_empty: false,
            validate: None,
            masked: false,
        }
    }
}

impl PromptOptions {
    /// What would be submitted for `value`, the problem blocking it, and
    /// whether it can be submitted (`PromptBody`'s rules).
    pub fn evaluate(&self, value: &str) -> (String, Option<String>, bool) {
        let entered = if self.masked {
            value.to_string()
        } else {
            value.trim().to_string()
        };
        let problem = if entered.is_empty() {
            None
        } else {
            self.validate
                .as_ref()
                .and_then(|validate| validate(&entered))
        };
        let submittable = (self.allow_empty || !entered.is_empty()) && problem.is_none();
        (entered, problem, submittable)
    }
}

enum Request {
    Confirm {
        options: ConfirmOptions,
        checked: bool,
        reply: async_channel::Sender<ConfirmAnswer>,
    },
    Prompt {
        options: PromptOptions,
        input: Entity<InputState>,
        reply: async_channel::Sender<Option<String>>,
        _events: Subscription,
    },
}

/// A view's queue of questions: FIFO, a question raised while another is
/// open waits its turn.
pub struct DialogQueue {
    queue: VecDeque<Request>,
    focus: Option<FocusHandle>,
    /// Focus to give back once the last question is answered.
    restore: Option<FocusHandle>,
}

impl Default for DialogQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Picks the queue out of its view, for listeners.
pub type QueueOf<V> = fn(&mut V) -> &mut DialogQueue;

impl DialogQueue {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            focus: None,
            restore: None,
        }
    }

    pub fn is_open(&self) -> bool {
        !self.queue.is_empty()
    }

    fn opened<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        if self.queue.len() == 1 {
            self.restore = window.focused(cx);
            self.focus_head(window, cx);
        }
        cx.notify();
    }

    fn focus_head<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        match self.queue.front() {
            Some(Request::Prompt { input, .. }) => {
                let handle = input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                input.update(cx, |state, cx| state.select_all(window, cx));
            }
            Some(Request::Confirm { .. }) => {
                let handle = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
                window.focus(&handle, cx);
            }
            None => {
                if let Some(previous) = self.restore.take() {
                    window.focus(&previous, cx);
                }
            }
        }
    }

    /// Ask for confirmation. Resolves unconfirmed on Cancel, Escape or a backdrop click.
    pub fn confirm<V: 'static>(
        &mut self,
        options: ConfirmOptions,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> impl Future<Output = ConfirmAnswer> + 'static + use<V> {
        let (reply, answer) = async_channel::bounded(1);
        self.queue.push_back(Request::Confirm {
            options,
            checked: false,
            reply,
        });
        self.opened(window, cx);
        async move { answer.recv().await.unwrap_or_default() }
    }

    /// Ask for a line of text. Resolves `None` when dismissed.
    pub fn prompt<V: 'static>(
        &mut self,
        options: PromptOptions,
        queue_of: QueueOf<V>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> impl Future<Output = Option<String>> + 'static + use<V> {
        let (reply, answer) = async_channel::bounded(1);
        let initial = options.initial_value.clone();
        let placeholder = options.placeholder.clone().unwrap_or_default();
        let masked = options.masked;
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .placeholder(placeholder)
                .masked(masked)
        });
        let events = cx.subscribe_in(
            &input,
            window,
            move |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => queue_of(this).submit_prompt(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        self.queue.push_back(Request::Prompt {
            options,
            input,
            reply,
            _events: events,
        });
        self.opened(window, cx);
        async move { answer.recv().await.unwrap_or(None) }
    }

    fn finish<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        self.queue.pop_front();
        self.focus_head(window, cx);
        cx.notify();
    }

    fn answer_confirm<V: 'static>(
        &mut self,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        if let Some(Request::Confirm { checked, reply, .. }) = self.queue.front() {
            let _ = reply.try_send(ConfirmAnswer {
                confirmed,
                checked: confirmed && *checked,
            });
            self.finish(window, cx);
        }
    }

    fn submit_prompt<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        if let Some(Request::Prompt {
            options,
            input,
            reply,
            ..
        }) = self.queue.front()
        {
            let value = input.read(cx).value().to_string();
            let (entered, _, submittable) = options.evaluate(&value);
            if submittable {
                let _ = reply.try_send(Some(entered));
                self.finish(window, cx);
            }
        }
    }

    fn dismiss<V: 'static>(&mut self, window: &mut Window, cx: &mut Context<V>) {
        match self.queue.front() {
            Some(Request::Confirm { .. }) => self.answer_confirm(false, window, cx),
            Some(Request::Prompt { reply, .. }) => {
                let _ = reply.try_send(None);
                self.finish(window, cx);
            }
            None => {}
        }
    }

    /// The question in front, drawn over the whole window; `None` when nothing is asked.
    pub fn render<V: 'static>(
        &mut self,
        queue_of: QueueOf<V>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Option<AnyElement> {
        let theme = *cx.theme();
        let body = match self.queue.front()? {
            Request::Confirm {
                options, checked, ..
            } => confirm_body(options, *checked, &theme, queue_of, cx),
            Request::Prompt { options, input, .. } => {
                prompt_body(options, input, &theme, queue_of, window, cx)
            }
        };
        let viewport = window.viewport_size();
        let focus = self.focus.clone();
        let width = (viewport.width - px(64.)).min(px(444.));
        let mut paper = div()
            .id("sftp-dialog")
            .key_context("SftpDialog")
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let queue = queue_of(this);
                match event.keystroke.key.as_str() {
                    "escape" => {
                        cx.stop_propagation();
                        queue.dismiss(window, cx);
                    }
                    "enter" if matches!(queue.queue.front(), Some(Request::Confirm { .. })) => {
                        cx.stop_propagation();
                        queue.answer_confirm(true, window, cx);
                    }
                    _ => {}
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .m(px(32.))
            .w(width)
            .child(body);
        if let Some(focus) = &focus
            && matches!(self.queue.front(), Some(Request::Confirm { .. }))
        {
            paper = paper.track_focus(focus);
        }
        let entity = cx.entity().downgrade();
        let dismiss: muxus_ui::widgets::dialog::CloseHandler = Rc::new(move |window, cx| {
            let _ = entity.update(cx, |this, cx| queue_of(this).dismiss(window, cx));
        });
        Some(modal_layer(
            window,
            "sftp-dialog-backdrop",
            paper,
            Some(dismiss),
        ))
    }
}

fn dialog_title(title: &str, theme: &Theme) -> impl IntoElement {
    let h6 = theme.typography.h6;
    div().px(px(24.)).py(px(16.)).child(
        label(title.to_string())
            .size(h6.size)
            .weight(h6.weight)
            .line_height(h6.line_height)
            .tracking(-0.2)
            .color(theme.palette.text.primary),
    )
}

fn dialog_actions(children: Vec<AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(8.))
        .p(px(8.))
        .children(children)
}

fn confirm_body<V: 'static>(
    options: &ConfirmOptions,
    checked: bool,
    theme: &Theme,
    queue_of: QueueOf<V>,
    cx: &mut Context<V>,
) -> AnyElement {
    let p = theme.palette;
    let body2 = theme.typography.body2;
    let mut paper = dialog_paper(theme)
        .w(relative(1.))
        .child(dialog_title(&options.title, theme));
    if options.description.is_some() || options.checkbox.is_some() {
        let mut content = div().flex().flex_col().px(px(24.)).pb(px(20.));
        if let Some(description) = &options.description {
            content = content.child(
                paragraph(description.clone())
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            );
        }
        if let Some(text) = &options.checkbox {
            content = content.child(
                div()
                    .mt(px(8.))
                    .ml(px(-11.))
                    .flex()
                    .items_center()
                    .child(
                        Checkbox::new("sftp-dialog-check", checked)
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Request::Confirm { checked, .. }) =
                                    queue_of(this).queue.front_mut()
                                {
                                    *checked = !*checked;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        label(text.clone())
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.primary),
                    ),
            );
        }
        paper = paper.child(content);
    }
    paper
        .child(dialog_actions(vec![
            Button::new(
                "sftp-dialog-cancel",
                options
                    .cancel_label
                    .clone()
                    .unwrap_or_else(|| "Cancel".into()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                queue_of(this).answer_confirm(false, window, cx)
            }))
            .into_any_element(),
            Button::new("sftp-dialog-confirm", options.confirm_label.clone())
                .contained()
                .color(if options.destructive {
                    ButtonColor::Error
                } else {
                    ButtonColor::Primary
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    queue_of(this).answer_confirm(true, window, cx)
                }))
                .into_any_element(),
        ]))
        .into_any_element()
}

/// MUI's small outlined `TextField` with a floating label and helper text.
fn prompt_field(
    options: &PromptOptions,
    input: &Entity<InputState>,
    problem: Option<&str>,
) -> impl IntoElement {
    let mut field = FormField::input("dialog-prompt-field", input)
        .label(options.label.clone())
        .shrink(options.placeholder.is_some())
        .error(problem.is_some())
        // A blank helper keeps the dialog's height while the value is fine.
        .helper(problem.unwrap_or(" ").to_string());
    if let Some(placeholder) = &options.placeholder {
        field = field.placeholder(placeholder.clone());
    }
    div().mt(px(4.)).child(field)
}

fn prompt_body<V: 'static>(
    options: &PromptOptions,
    input: &Entity<InputState>,
    theme: &Theme,
    queue_of: QueueOf<V>,
    _window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let p = theme.palette;
    let body2 = theme.typography.body2;
    let value = input.read(cx).value().to_string();
    let (_, problem, submittable) = options.evaluate(&value);
    let mut content = div().flex().flex_col().px(px(24.)).pb(px(20.));
    if let Some(description) = &options.description {
        content = content.child(
            div().mb(px(12.)).child(
                paragraph(description.clone())
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            ),
        );
    }
    content = content.child(prompt_field(options, input, problem.as_deref()));
    dialog_paper(theme)
        .w(relative(1.))
        .child(dialog_title(&options.title, theme))
        .child(content)
        .child(dialog_actions(vec![
            Button::new("sftp-dialog-cancel", "Cancel")
                .on_click(
                    cx.listener(move |this, _, window, cx| queue_of(this).dismiss(window, cx)),
                )
                .into_any_element(),
            Button::new("sftp-dialog-submit", options.confirm_label.clone())
                .contained()
                .disabled(!submittable)
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        queue_of(this).submit_prompt(window, cx)
                    }),
                )
                .into_any_element(),
        ]))
        .into_any_element()
}

/// A window's place for questions that belong to no particular view (a save
/// of a file opened with a local program, asked long after its browser
/// closed): the app renders the host somewhere in each window it may ask in.
pub struct DialogHost {
    queue: DialogQueue,
    window: Option<muxus_ui::gpui_kit::AnyWindowHandle>,
}

impl DialogHost {
    pub fn new() -> Self {
        Self {
            queue: DialogQueue::new(),
            window: None,
        }
    }

    /// Ask in the window the host was last drawn in (else the active one).
    pub fn ask(
        host: &Entity<Self>,
        options: ConfirmOptions,
        cx: &mut App,
    ) -> std::pin::Pin<Box<dyn Future<Output = ConfirmAnswer>>> {
        let window = host.read(cx).window.or_else(|| cx.active_window());
        let host = host.clone();
        let asked = window.and_then(|window| {
            window
                .update(cx, |_, window, cx| {
                    host.update(cx, |host, cx| host.queue.confirm(options, window, cx))
                })
                .ok()
        });
        match asked {
            Some(answer) => Box::pin(answer),
            None => Box::pin(async { ConfirmAnswer::default() }),
        }
    }
}

impl Default for DialogHost {
    fn default() -> Self {
        Self::new()
    }
}

impl muxus_ui::gpui_kit::Render for DialogHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window = Some(window.window_handle());
        let overlay = self
            .queue
            .render(|host: &mut Self| &mut host.queue, window, cx);
        div().size_0().children(overlay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_trim_and_validate_what_was_typed() {
        let options = PromptOptions {
            validate: Some(Rc::new(|value: &str| {
                value.contains('/').then(|| "no slashes".to_string())
            })),
            ..PromptOptions::default()
        };
        assert_eq!(options.evaluate("  logs "), ("logs".into(), None, true));
        assert_eq!(options.evaluate("   "), (String::new(), None, false));
        assert_eq!(
            options.evaluate("a/b"),
            ("a/b".into(), Some("no slashes".into()), false)
        );
        let masked = PromptOptions {
            masked: true,
            ..PromptOptions::default()
        };
        assert_eq!(masked.evaluate(" pw "), (" pw ".into(), None, true));
        let optional = PromptOptions {
            allow_empty: true,
            ..PromptOptions::default()
        };
        assert!(optional.evaluate("").2);
    }
}
