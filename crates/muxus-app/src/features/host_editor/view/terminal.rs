//! The sections every terminal host has, SSH, Telnet and serial alike
//! (`TerminalAppearanceSection.tsx`, `LoginSequenceSection.tsx`,
//! `LoggingSection.tsx`, `HighlightingSection.tsx`).

use muxus_core::api::{HostKeywordHighlightConfig, SavedHostKind};
use muxus_session::login_sequence::editor::{LoginSequenceDraft, inherited_login_sequence};
use muxus_store::prefs::state::PrefsState;
use muxus_term::palette::{
    TerminalScheme, is_terminal_scheme_id, terminal_color_for_host, terminal_scheme,
    terminal_scheme_id_for_host, terminal_schemes,
};
use muxus_term::paste::PasteDelayKind;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};

use super::HostEditor;
use super::form::{self, Choice};
use super::shared::{
    color_override, keyword_rules_editor, logging_section, login_sequence_editor, paste_delay_field,
};
use super::ssh::{row, stack};
use crate::features::host_editor::widgets::section::intro;
use crate::hosts::tree::normalize_group_path;
use muxus_ui::widgets::Alert;
use muxus_ui::widgets::display::AlertVariant;
use muxus_ui::widgets::display::alert_ink;
use muxus_ui::widgets::form_control::FormControl;
use muxus_ui::widgets::form_control::FormControlLabel;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{body2, caption, inline_caption, para, text};

macro_rules! terminal_field {
    ($get:ident, $set:ident, $field:ident: $ty:ty) => {
        pub(super) fn $get(&self) -> $ty {
            if self.kind == SavedHostKind::Ssh {
                self.ssh.$field.clone()
            } else {
                self.native.$field.clone()
            }
        }

        pub(super) fn $set(&mut self, value: $ty) {
            if self.kind == SavedHostKind::Ssh {
                self.ssh.$field = value;
            } else {
                self.native.$field = value;
            }
        }
    };
}

impl HostEditor {
    terminal_field!(term_scheme, set_term_scheme, terminal_scheme: Option<String>);
    terminal_field!(term_font, set_term_font, terminal_font_color: Option<String>);
    terminal_field!(term_background, set_term_background, terminal_background_color: Option<String>);
    terminal_field!(term_command_group, set_term_command_group, command_button_group: Option<String>);
    terminal_field!(term_line_delay, set_term_line_delay, paste_line_delay_ms: Option<u32>);
    terminal_field!(term_char_delay, set_term_char_delay, paste_char_delay_ms: Option<u32>);
    terminal_field!(term_highlights, set_term_highlights, keyword_highlights: HostKeywordHighlightConfig);
    terminal_field!(term_login, set_term_login, login_sequence: LoginSequenceDraft);

    pub(super) fn term_group(&self) -> String {
        if self.kind == SavedHostKind::Ssh {
            self.ssh.group.clone()
        } else {
            self.native.group.clone()
        }
    }

    pub(super) fn appearance_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.data.prefs.clone();
        let custom = custom_schemes(&prefs);
        let scheme = self.term_scheme();
        let valid = scheme
            .as_deref()
            .is_some_and(|id| is_terminal_scheme_id(id, &custom));
        let defaults =
            terminal_color_defaults(&prefs, &custom, scheme.as_deref(), theme.mode.is_dark());
        let mut col = stack(2.5).child(intro(
            &theme,
            "Terminal appearance",
            "Distinguish this host in focus mode. Unset values follow the application settings.",
        ));
        col = col.child(scheme_select(
            self,
            "host-terminal-scheme",
            if valid {
                scheme.clone().unwrap_or_default()
            } else {
                String::new()
            },
            false,
            &custom,
            window,
            cx,
            |this, id, cx| {
                this.set_term_scheme((!id.is_empty()).then_some(id));
                this.changed(cx);
            },
        ));
        let font = self.term_font();
        col = col.child(color_override(
            self,
            "host-font-color",
            "Text color",
            font.as_deref(),
            &defaults.0,
            false,
            cx,
            |this, color, cx| {
                this.set_term_font(color);
                this.changed(cx);
            },
        ));
        let background = self.term_background();
        col = col.child(color_override(
            self,
            "host-background-color",
            "Background color",
            background.as_deref(),
            &defaults.1,
            false,
            cx,
            |this, color, cx| {
                this.set_term_background(color);
                this.changed(cx);
            },
        ));
        col = col.child(div().pt(Theme::spacing(1.)).child(intro(
            &theme,
            "Command buttons",
            "The command bar switches to this group while a session to this host is active.",
        )));
        let group = self.term_command_group();
        let groups = prefs.command_button_groups.clone();
        let mut choices = vec![Choice::new(String::new(), "Group chosen in the bar")];
        if let Some(value) = &group
            && !groups.iter().any(|g| &g.id == value)
        {
            choices.push(Choice::new(value.clone(), "Deleted group"));
        }
        for g in &groups {
            let name = muxus_core::js::js_trim(&g.name);
            choices.push(Choice::new(
                g.id.clone(),
                if name.is_empty() {
                    "Untitled group".to_owned()
                } else {
                    name.to_owned()
                },
            ));
        }
        let helper = if groups.len() > 1 {
            "Another group can still be picked in the bar during the session."
        } else {
            "Create groups in the command button manager, for example one per vendor."
        };
        col = col.child(form::select(
            self,
            "command-group",
            |body| {
                FormField::new("command-group-field", body)
                    .label("Command button group")
                    .helper(helper)
            },
            &group.clone().unwrap_or_default(),
            choices,
            // An empty value is MUI's "nothing selected": the label sits in the field.
            group
                .is_none()
                .then(|| FieldBody::Value(SharedString::default())),
            window,
            cx,
            |this, id, cx| {
                this.set_term_command_group((!id.is_empty()).then_some(id));
                this.changed(cx);
            },
        ));
        col = col.child(
            div().pt(Theme::spacing(1.)).child(intro(
                &theme,
                "Paste pacing",
                "Type pastes in slowly for consoles that drop input sent all at once. Empty follows the Terminal settings; 0 sends pastes at once.",
            )),
        );
        let (line, ch) = (self.term_line_delay(), self.term_char_delay());
        let line_field = paste_delay_field(
            self,
            "paste-line",
            PasteDelayKind::Line,
            line,
            prefs.paste_line_delay_ms,
            false,
            window,
            cx,
            |this, v, cx| {
                this.set_term_line_delay(v);
                this.changed(cx);
            },
        );
        let char_field = paste_delay_field(
            self,
            "paste-char",
            PasteDelayKind::Char,
            ch,
            prefs.paste_char_delay_ms,
            false,
            window,
            cx,
            |this, v, cx| {
                this.set_term_char_delay(v);
                this.changed(cx);
            },
        );
        col = col.child(row(1.5).child(line_field).child(char_field));
        col.into_any_element()
    }

    pub(super) fn login_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let group = self.term_group();
        let inherited = inherited_login_sequence(Some(&self.data.folders), Some(&group));
        let value = self.term_login();
        let vault = self.data.vault.clone();
        let empty = if normalize_group_path(Some(&group)).is_empty() {
            "This host is not in a folder, so no login sequence runs."
        } else {
            "No folder this host is in sets a login sequence, so none runs."
        };
        stack(2.)
            .child(intro(
                &theme,
                "Login sequence",
                "Steps that run once the session connects, and again after every reconnect: wait for a prompt, then answer it. Secrets are typed from the password vault and never written to session history or log files.",
            ))
            .child(login_sequence_editor(
                self,
                "host-login",
                &value,
                inherited,
                "Use the folder's login sequence",
                empty,
                vault.as_ref(),
                window,
                cx,
                |this, draft, cx| {
                    this.set_term_login(draft);
                    this.changed(cx);
                },
            ))
            .into_any_element()
    }

    pub(super) fn logging_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = *super::logging_draft(self);
        logging_section(
            self,
            "host-logging",
            &value,
            "Choose whether new sessions for this host inherit the application default or use a dedicated retention and input-privacy policy.",
            window,
            cx,
            |this, draft, cx| {
                *super::logging_draft(this) = draft;
                this.changed(cx);
            },
        )
    }

    pub(super) fn highlighting_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let config = self.term_highlights();
        let profiles = self.data.prefs.keyword_highlight_profiles.clone();
        let selected = config
            .profile_id
            .as_ref()
            .and_then(|id| profiles.iter().find(|profile| &profile.id == id));
        let description = if self.kind == SavedHostKind::Ssh {
            "Host rules apply whenever a terminal connects through this OpenSSH alias."
        } else {
            "Host rules apply whenever a session connects to this host."
        };
        let mut choices = vec![Choice::new(String::new(), "No reusable profile")];
        if let Some(id) = &config.profile_id
            && selected.is_none()
        {
            choices.push(Choice::new(id.clone(), format!("Missing profile ({id})")));
        }
        for profile in &profiles {
            choices.push(Choice::new(profile.id.clone(), profile.name.clone()));
        }
        let helper = match selected {
            Some(profile) => format!(
                "{} shared rule{}; edit it in Settings → Highlighting.",
                profile.rules.len(),
                if profile.rules.len() == 1 { "" } else { "s" }
            ),
            None => "Optional named rule set shared by several hosts.".into(),
        };
        let mut col = stack(2.).child(intro(&theme, "Keyword highlighting", description));
        col = col.child(form::select(
            self,
            "highlight-profile",
            |body| {
                FormField::new("highlight-profile-field", body)
                    .label("Highlighting profile")
                    .helper(helper)
            },
            &config.profile_id.clone().unwrap_or_default(),
            choices,
            config
                .profile_id
                .is_none()
                .then(|| FieldBody::Value(SharedString::default())),
            window,
            cx,
            |this, id, cx| {
                let mut config = this.term_highlights();
                config.profile_id = (!id.is_empty()).then_some(id);
                this.set_term_highlights(config);
                this.changed(cx);
            },
        ));
        if config.profile_id.is_some() && selected.is_none() {
            col = col.child(Alert::new(Status::Warning, "").outlined().content(para("This host references a profile that is not installed. Import it again or choose another profile.", body2(), alert_ink(&theme, Status::Warning, AlertVariant::Outlined))));
        }
        let entity = cx.entity().downgrade();
        col = col.child(
            FormControlLabel::new(
                "inherit-global",
                FormControl::Switch(config.inherit_global),
                "",
            )
            .label_element(
                div()
                    .flex()
                    .flex_col()
                    .child(para(
                        "Include global highlighting rules",
                        body2(),
                        p.text.primary,
                    ))
                    .child(inline_caption(
                        "Turn this off when this host should use only its profile and host rules.",
                        p.text.secondary,
                    )),
            )
            .small()
            .flush()
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    let mut config = this.term_highlights();
                    config.inherit_global = !config.inherit_global;
                    this.set_term_highlights(config);
                    this.changed(cx);
                });
            }),
        );
        col = col.child(keyword_rules_editor(
            self,
            "host-highlights",
            &config.rules,
            "No additional host-specific keyword rules.",
            window,
            cx,
            |this, rules, cx| {
                let mut config = this.term_highlights();
                config.rules = rules;
                this.set_term_highlights(config);
                this.changed(cx);
            },
        ));
        col.into_any_element()
    }
}

/// The user's custom schemes in the shape the pickers read.
pub fn custom_schemes(prefs: &PrefsState) -> Vec<TerminalScheme> {
    serde_json::to_value(&prefs.custom_terminal_schemes)
        .ok()
        .and_then(|value| {
            serde_json::from_value::<Vec<muxus_term::custom_schemes::CustomTerminalScheme>>(value)
                .ok()
        })
        .map(|schemes| muxus_term::custom_schemes::terminal_schemes_from_custom(&schemes))
        .unwrap_or_default()
}

/// The text and background colors a host gets when it overrides neither.
pub fn terminal_color_defaults(
    prefs: &PrefsState,
    custom: &[TerminalScheme],
    host_scheme: Option<&str>,
    dark: bool,
) -> (String, String) {
    let application = if dark {
        &prefs.dark_terminal_scheme
    } else {
        &prefs.light_terminal_scheme
    };
    let id = terminal_scheme_id_for_host(application, host_scheme, custom);
    let scheme = terminal_scheme(Some(id), custom);
    (
        terminal_color_for_host(&scheme.theme.foreground, Some(&prefs.font_color)).to_owned(),
        terminal_color_for_host(&scheme.theme.background, Some(&prefs.background_color)).to_owned(),
    )
}

/// The preview a scheme row shows: a 60×28 swatch box with six ANSI bars.
fn scheme_preview(theme: &Theme, scheme: &TerminalScheme) -> impl IntoElement {
    let bg = try_hex(&scheme.theme.background).unwrap_or(theme.palette.background_paper);
    let border = if scheme.light {
        theme.components.outline_on_light
    } else {
        theme.components.outline_on_dark
    };
    let mut swatch = div()
        .w(px(60.))
        .h(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .rounded(px(theme.radius * 0.75));
    for key in ["red", "green", "yellow", "blue", "magenta", "cyan"] {
        let color = scheme
            .theme
            .color(key)
            .and_then(try_hex)
            .unwrap_or(theme.palette.text.primary);
        swatch = swatch.child(
            div()
                .w(px(5.))
                .h(px(12.))
                .rounded(px(theme.radius * 0.25))
                .bg(color),
        );
    }
    swatch
}

fn scheme_label(theme: &Theme, scheme: &TerminalScheme, show_mode: bool) -> AnyElement {
    let p = &theme.palette;
    let mut line = row(1.25)
        .items_center()
        .w_full()
        .min_w_0()
        .child(scheme_preview(theme, scheme))
        .child(text(
            scheme.name.clone(),
            body2().weight(weight::MEDIUM),
            p.text.primary,
        ));
    if show_mode {
        line = line.child(
            div().ml_auto().pr(Theme::spacing(0.5)).child(
                text(
                    if scheme.light { "Light" } else { "Dark" },
                    caption(),
                    p.text.secondary,
                )
                .no_truncate(),
            ),
        );
    }
    line.into_any_element()
}

/// `TerminalSchemeSelect` with an inherit choice.
#[allow(clippy::too_many_arguments)]
pub fn scheme_select<V: form::Form>(
    view: &mut V,
    key: &str,
    value: String,
    mixed: bool,
    custom: &[TerminalScheme],
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, String, &mut Context<V>) + 'static,
) -> AnyElement {
    const MIXED: &str = "__mixed__";
    let theme = *cx.theme();
    let current = if mixed {
        MIXED.to_owned()
    } else {
        value.clone()
    };
    let mut choices = Vec::new();
    if mixed {
        choices.push(Choice::new(MIXED.to_owned(), "Multiple values").hidden());
    }
    choices.push(Choice::new(String::new(), "Use application default"));
    let builtins = terminal_schemes();
    let mut groups: Vec<(&str, Vec<&TerminalScheme>)> = Vec::new();
    if !custom.is_empty() {
        groups.push(("Custom schemes", custom.iter().collect()));
    }
    groups.push((
        "Light schemes",
        builtins.iter().filter(|s| s.light).collect(),
    ));
    groups.push((
        "Dark schemes",
        builtins.iter().filter(|s| !s.light).collect(),
    ));
    for (label, schemes) in groups {
        choices.push(Choice::header(format!("header-{label}"), label));
        for scheme in schemes {
            choices.push(Choice::new(scheme.id.clone(), scheme.name.clone()));
        }
    }
    let display = if mixed {
        FieldBody::Muted("Multiple values".into())
    } else if value.is_empty() {
        FieldBody::Element(
            text(
                "Use application default",
                body2(),
                theme.palette.text.secondary,
            )
            .into_any_element(),
        )
    } else {
        FieldBody::Element(scheme_label(
            &theme,
            terminal_scheme(Some(&value), custom),
            true,
        ))
    };
    form::select(
        view,
        key,
        |body| {
            FormField::new(SharedString::from(format!("{key}-field")), body)
                .label("Color scheme")
                .shrink(true)
                .medium()
        },
        &current,
        choices,
        Some(display),
        window,
        cx,
        move |this, id, cx| {
            if id != MIXED && !id.starts_with("header-") {
                on_change(this, id, cx);
            }
        },
    )
}
