//! What a host's sessions type for it: prompt waiting for pastes (`PastePromptFields.tsx`),
//! anti-idle (`AntiIdleFields.tsx`), and its keyboard and echo (the input modes of
//! `TerminalAppearanceSection.tsx`). Shared by the host editor and the bulk editor, which pass
//! how to read and write the draft they edit.

use std::rc::Rc;

use muxus_core::anti_idle::{AntiIdleDraft, anti_idle_interval_problem};
use muxus_core::api::{ANTI_IDLE_TEXT_MAX_LENGTH, TerminalBackspaceKey, TerminalEnterKey};
use muxus_core::input_modes::{
    BACKSPACE_KEY_OPTIONS, ENTER_KEY_OPTIONS, TerminalInputModes, custom_terminal_input_modes,
};
use muxus_core::js::utf16_prefix;
use muxus_core::paste_pacing::{
    DEFAULT_PASTE_ERROR_PATTERN, DEFAULT_PASTE_PROMPT_PATTERN, DEFAULT_PASTE_PROMPT_TIMEOUT_MS,
    MAX_PASTE_PATTERN_LENGTH, paste_pattern_problem,
};
use muxus_core::paste_prompt::{PastePromptWaitDraft, paste_prompt_timeout_problem};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement, SharedString, Styled, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::Alert;
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::form_field::{FormField, end_adornment};
use muxus_ui::widgets::typography::{body1, body2, para};

use super::form::{self, Choice, Form};
use super::ssh::{row, stack};
use crate::features::host_editor::widgets::section::intro;

const MONO_FONT: &str = "JetBrains Mono";
const MONO_SIZE: f32 = 13.;

/// How many of a host's typing settings are set, for the Terminal appearance badge: its own
/// paste delays, prompt waiting, anti-idle and keys or echo other than a terminal's.
pub fn typing_settings_count(
    paste_delays: bool,
    prompt_wait: &PastePromptWaitDraft,
    anti_idle: &AntiIdleDraft,
    modes: &TerminalInputModes,
) -> usize {
    [
        paste_delays,
        prompt_wait.enabled,
        anti_idle.enabled,
        custom_terminal_input_modes(modes),
    ]
    .into_iter()
    .filter(|set| *set)
    .count()
}

type Getter<V, T> = Rc<dyn Fn(&V) -> Option<T>>;
type Setter<V, T> = Rc<dyn Fn(&mut V, T, &mut Context<V>)>;
/// Writes a chosen select value into the modes.
type ApplyChoice = Rc<dyn Fn(&mut TerminalInputModes, &str)>;

/// Reads the draft a field edits, and writes a changed one back.
pub struct DraftAccess<V, T> {
    get: Getter<V, T>,
    set: Setter<V, T>,
}

impl<V, T> Clone for DraftAccess<V, T> {
    fn clone(&self) -> Self {
        Self {
            get: self.get.clone(),
            set: self.set.clone(),
        }
    }
}

impl<V: 'static, T: Clone + 'static> DraftAccess<V, T> {
    pub fn new(
        get: impl Fn(&V) -> Option<T> + 'static,
        set: impl Fn(&mut V, T, &mut Context<V>) + 'static,
    ) -> Self {
        Self {
            get: Rc::new(get),
            set: Rc::new(set),
        }
    }

    /// Change the current draft with `change`.
    fn edit(&self, view: &mut V, cx: &mut Context<V>, change: impl FnOnce(&mut T)) {
        if let Some(mut draft) = (self.get)(view) {
            change(&mut draft);
            (self.set)(view, draft, cx);
        }
    }

    /// A text field's change handler for one field of the draft.
    fn text(
        &self,
        field: fn(&mut T) -> &mut String,
        clean: fn(&str) -> String,
    ) -> impl Fn(&mut V, String, &mut Context<V>) -> Option<String> + 'static {
        let access = self.clone();
        move |view, text, cx| {
            let cleaned = clean(&text);
            let value = cleaned.clone();
            access.edit(view, cx, |draft| *field(draft) = value);
            (cleaned != text).then_some(cleaned)
        }
    }
}

/// Digits only, at most `max` of them (`replace(/[^\d]/g, '').slice(0, max)`).
fn digits(text: &str, max: usize) -> String {
    text.chars()
        .filter(char::is_ascii_digit)
        .take(max)
        .collect()
}

fn pattern(text: &str) -> String {
    utf16_prefix(text, MAX_PASTE_PATTERN_LENGTH).to_string()
}

/// A switch with its label, flush with the text under it.
fn switch_row<V: Form>(
    id: impl Into<SharedString>,
    checked: bool,
    label: &str,
    theme: &Theme,
    cx: &mut Context<V>,
    on_toggle: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> AnyElement {
    let entity = cx.entity().downgrade();
    FormControlLabel::new(id.into(), FormControl::Switch(checked), "")
        .label_element(para(label.to_owned(), body1(), theme.palette.text.primary))
        .flush()
        .on_click(move |_, _, cx| {
            let _ = entity.update(cx, |this, cx| on_toggle(this, cx));
        })
        .into_any_element()
}

/// A host's prompt waiting for pastes: the switch, both patterns and the timeout.
pub fn paste_prompt_fields<V: Form>(
    view: &mut V,
    key: &str,
    value: &PastePromptWaitDraft,
    access: DraftAccess<V, PastePromptWaitDraft>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let toggle = access.clone();
    let mut col = stack(1.5).child(
        div()
            .child(switch_row(
                format!("{key}-prompt-wait"),
                value.enabled,
                "Wait for the prompt after each line",
                &theme,
                cx,
                move |this, cx| toggle.edit(this, cx, |draft| draft.enabled = !draft.enabled),
            ))
            .child(para(
                "Sends the next line once the device shows its prompt again, instead of after the line delay, and stops the paste at the first line the device rejects. A shell in bracketed paste mode, such as bash, gets the delays. Empty fields use the defaults shown.",
                body2(),
                p.text.secondary,
            )),
    );
    if !value.enabled {
        return col.into_any_element();
    }
    let prompt_problem = (!value.prompt_pattern.is_empty())
        .then(|| paste_pattern_problem(&value.prompt_pattern))
        .flatten();
    let error_problem = (!value.error_pattern.is_empty())
        .then(|| paste_pattern_problem(&value.error_pattern))
        .flatten();
    let timeout_problem = paste_prompt_timeout_problem(&value.timeout_ms);

    let prompt_input = form::text_input(
        view,
        format!("{key}-prompt-pattern"),
        window,
        cx,
        value.prompt_pattern.clone(),
        access.text(|draft| &mut draft.prompt_pattern, pattern),
    );
    let timeout_input = form::text_input(
        view,
        format!("{key}-prompt-timeout"),
        window,
        cx,
        value.timeout_ms.clone(),
        access.text(|draft| &mut draft.timeout_ms, |text| digits(text, 5)),
    );
    let error_input = form::text_input(
        view,
        format!("{key}-error-pattern"),
        window,
        cx,
        value.error_pattern.clone(),
        access.text(|draft| &mut draft.error_pattern, pattern),
    );
    let prompt_field = FormField::input(
        SharedString::from(format!("{key}-prompt-pattern-field")),
        &prompt_input,
    )
    .label("Prompt pattern")
    .shrink(true)
    .placeholder(DEFAULT_PASTE_PROMPT_PATTERN)
    .font(MONO_FONT, MONO_SIZE)
    .error(prompt_problem.is_some())
    .helper(
        prompt_problem
            .unwrap_or_else(|| "Regular expression for the line the device waits on.".into()),
    );
    let timeout_field = FormField::input(
        SharedString::from(format!("{key}-prompt-timeout-field")),
        &timeout_input,
    )
    .label("Timeout")
    .shrink(true)
    .placeholder(DEFAULT_PASTE_PROMPT_TIMEOUT_MS.to_string())
    .end(end_adornment("ms", &theme))
    .error(timeout_problem.is_some())
    .helper(timeout_problem.unwrap_or_else(|| "Then the line delay applies.".into()))
    .width(170.);
    let error_field = FormField::input(
        SharedString::from(format!("{key}-error-pattern-field")),
        &error_input,
    )
    .label("Error pattern")
    .shrink(true)
    .placeholder(DEFAULT_PASTE_ERROR_PATTERN)
    .font(MONO_FONT, MONO_SIZE)
    .error(error_problem.is_some())
    .helper(error_problem.unwrap_or_else(|| {
        "Regular expression for an output line that rejects the line sent.".into()
    }));
    col = col
        .child(
            row(1.5)
                .items_start()
                .child(div().flex_1().min_w_0().child(prompt_field))
                .child(div().flex_none().child(timeout_field)),
        )
        .child(error_field);
    col.into_any_element()
}

/// The anti-idle heading, shared by the host editors and the bulk editor.
pub fn anti_idle_intro(theme: &Theme) -> Div {
    intro(
        theme,
        "Anti-idle",
        "Type something after a stretch without input, for devices that close idle sessions. Applies to sessions opened after saving.",
    )
}

/// A host's anti-idle switch, interval, text and Enter, as one draft.
pub fn anti_idle_fields<V: Form>(
    view: &mut V,
    key: &str,
    value: &AntiIdleDraft,
    access: DraftAccess<V, AntiIdleDraft>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let toggle = access.clone();
    let mut col = stack(1.5).child(switch_row(
        format!("{key}-anti-idle"),
        value.enabled,
        "Type into idle sessions",
        &theme,
        cx,
        move |this, cx| toggle.edit(this, cx, |draft| draft.enabled = !draft.enabled),
    ));
    if !value.enabled {
        return col.into_any_element();
    }
    let interval_problem = anti_idle_interval_problem(value);
    let interval_input = form::text_input(
        view,
        format!("{key}-anti-idle-interval"),
        window,
        cx,
        value.interval_seconds.clone(),
        access.text(|draft| &mut draft.interval_seconds, |text| digits(text, 4)),
    );
    let text_input = form::text_input(
        view,
        format!("{key}-anti-idle-text"),
        window,
        cx,
        value.text.clone(),
        access.text(
            |draft| &mut draft.text,
            |text| utf16_prefix(text, ANTI_IDLE_TEXT_MAX_LENGTH).to_string(),
        ),
    );
    let interval_field = FormField::input(
        SharedString::from(format!("{key}-anti-idle-interval-field")),
        &interval_input,
    )
    .label("After")
    .end(end_adornment("s", &theme))
    .error(interval_problem.is_some())
    .helper(interval_problem.unwrap_or_else(|| "Without input".into()))
    .width(150.);
    let mut text_field = FormField::input(
        SharedString::from(format!("{key}-anti-idle-text-field")),
        &text_input,
    )
    .label("Text")
    .shrink(true)
    .font(MONO_FONT, MONO_SIZE)
    .helper("Typed exactly as entered, spaces included.");
    if value.enter {
        text_field = text_field.placeholder("Nothing \u{2014} only press Enter");
    }
    let enter_toggle = access.clone();
    let entity = cx.entity().downgrade();
    col = col
        .child(
            row(1.5)
                .items_start()
                .child(div().flex_none().child(interval_field))
                .child(div().flex_1().min_w_0().child(text_field)),
        )
        .child(
            FormControlLabel::new(
                SharedString::from(format!("{key}-anti-idle-enter")),
                FormControl::Checkbox(value.enter),
                "",
            )
            .label_element(para("Press Enter afterwards", body1(), p.text.primary))
            .flush()
            .on_click(move |_, _, cx| {
                let toggle = enter_toggle.clone();
                let _ = entity.update(cx, |this, cx| {
                    toggle.edit(this, cx, |draft| draft.enter = !draft.enter)
                });
            }),
        );
    if value.enter {
        col = col.child(Alert::new(
            Status::Warning,
            "Enter also answers whatever the device is asking at that moment, such as a confirmation before a reload, and runs a command left half-typed for longer than the interval. In a full-screen editor or pager, the text and Enter land there.",
        ).outlined());
    }
    col.into_any_element()
}

/// The keyboard and echo heading of a host's terminal settings.
pub fn input_modes_intro(theme: &Theme) -> Div {
    intro(
        theme,
        "Keyboard and echo",
        "For consoles that expect other keys than a terminal sends, or do not echo input. Open sessions follow a change at once; an SSH session takes a new Backspace when it opens.",
    )
}

/// The choices of the Backspace select.
pub fn backspace_choices() -> Vec<(TerminalBackspaceKey, String)> {
    BACKSPACE_KEY_OPTIONS
        .iter()
        .map(|(key, label)| (*key, (*label).to_owned()))
        .collect()
}

/// The choices of the Enter select.
pub fn enter_choices() -> Vec<(TerminalEnterKey, String)> {
    ENTER_KEY_OPTIONS
        .iter()
        .map(|(key, label)| (*key, (*label).to_owned()))
        .collect()
}

/// What Backspace and Enter send, implicit CR and local echo.
pub fn input_modes_fields<V: Form>(
    view: &mut V,
    key: &str,
    value: TerminalInputModes,
    access: DraftAccess<V, TerminalInputModes>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let theme = *cx.theme();
    let p = theme.palette;
    let select = |view: &mut V,
                  window: &mut Window,
                  cx: &mut Context<V>,
                  id: &str,
                  label: &'static str,
                  current: String,
                  choices: Vec<(String, String)>,
                  apply: ApplyChoice| {
        let access = access.clone();
        form::select(
            view,
            id,
            move |body| {
                FormField::new(SharedString::from(format!("{label}-field")), body).label(label)
            },
            &current,
            choices
                .into_iter()
                .map(|(value, label)| Choice::new(value, label))
                .collect(),
            None,
            window,
            cx,
            move |this, chosen: String, cx| {
                let apply = apply.clone();
                access.edit(this, cx, |modes| apply(modes, &chosen));
            },
        )
    };
    let backspace = select(
        view,
        window,
        cx,
        &format!("{key}-backspace-key"),
        "Backspace sends",
        value.backspace_key.as_str().to_owned(),
        backspace_choices()
            .into_iter()
            .map(|(key, label)| (key.as_str().to_owned(), label))
            .collect(),
        Rc::new(|modes, chosen| {
            if let Some(key) = TerminalBackspaceKey::parse(chosen) {
                modes.backspace_key = key;
            }
        }),
    );
    let enter = select(
        view,
        window,
        cx,
        &format!("{key}-enter-key"),
        "Enter sends",
        value.enter_key.as_str().to_owned(),
        enter_choices()
            .into_iter()
            .map(|(key, label)| (key.as_str().to_owned(), label))
            .collect(),
        Rc::new(|modes, chosen| {
            if let Some(key) = TerminalEnterKey::parse(chosen) {
                modes.enter_key = key;
            }
        }),
    );
    let (cr, echo) = (access.clone(), access);
    stack(1.)
        .child(
            row(1.5)
                .child(div().flex_1().min_w_0().child(backspace))
                .child(div().flex_1().min_w_0().child(enter)),
        )
        .child(
            div()
                .child(switch_row(
                    format!("{key}-implicit-cr"),
                    value.implicit_cr,
                    "Implicit CR in every received LF",
                    &theme,
                    cx,
                    move |this, cx| cr.edit(this, cx, |modes| modes.implicit_cr = !modes.implicit_cr),
                ))
                .child(para(
                    "For devices that end output lines with LF alone, which otherwise run down the screen like stairs.",
                    body2(),
                    p.text.secondary,
                )),
        )
        .child(
            div()
                .child(switch_row(
                    format!("{key}-local-echo"),
                    value.local_echo,
                    "Local echo",
                    &theme,
                    cx,
                    move |this, cx| echo.edit(this, cx, |modes| modes.local_echo = !modes.local_echo),
                ))
                .child(para(
                    "Shows what you type, paste or send with a button, for devices that do not echo it. Control keys, arrows and function keys are sent without being shown.",
                    body2(),
                    p.text.secondary,
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_digits_a_number_field_takes() {
        assert_eq!(digits("5 000ms", 5), "5000");
        assert_eq!(digits("1234567", 5), "12345");
        assert_eq!(digits("12a34", 4), "1234");
    }

    #[test]
    fn offers_the_keys_the_typescript_editor_offers() {
        assert_eq!(
            backspace_choices()
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            ["DEL, 0x7F (default)", "^H, 0x08"]
        );
        assert_eq!(
            enter_choices()
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            ["CR (default)", "CR LF", "LF"]
        );
    }
}
