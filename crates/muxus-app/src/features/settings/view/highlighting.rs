//! `components/HighlightProfilesSection.tsx` and the rule list it shows
//! twice, `components/KeywordHighlightRulesEditor.tsx`.

use crate::features::settings::palette::MISSING_SCHEME_COLOR;
use std::rc::Rc;

use muxus_core::api::{KeywordHighlightProfile, KeywordHighlightRule};
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::command_buttons::new_preference_id;
use muxus_term::highlight_profiles::{
    MAX_HIGHLIGHT_PROFILE_FILE_BYTES, MAX_KEYWORD_HIGHLIGHT_PROFILES, builtin_highlight_profiles,
    create_highlight_profile_document, keyword_highlight_rules_to_json, merge_highlight_profiles,
    parse_highlight_profile_document, parse_keyword_highlight_rules_json,
};
use muxus_term::keywords::keyword_pattern_error;
use muxus_ui::color::{alpha, hex, try_hex};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, Context, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::{Button, ButtonColor, IconButton, Switch, Tooltip, bounds_probe};

use super::super::files::export_filename;
use super::super::texts;
use super::fields::field;
use super::kit::{
    BODY2, alert_text, caption, color_well, group, outlined_alert, page, row, secondary_text, snap,
};
use super::window::{MenuEntry, SettingsWindow};

/// Matches a small outlined field so every control in a rule row lines up.
const ROW_HEIGHT: f32 = 40.;

/// Which rule list an editor edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RulesTarget {
    Global,
    Profile(String),
}

impl RulesTarget {
    pub fn key(&self) -> String {
        match self {
            RulesTarget::Global => "global".into(),
            RulesTarget::Profile(id) => format!("profile-{id}"),
        }
    }

    fn from_key(key: &str) -> Self {
        match key.strip_prefix("profile-") {
            Some(id) => RulesTarget::Profile(id.to_string()),
            None => RulesTarget::Global,
        }
    }

    pub fn rules(&self, prefs: &PrefsState) -> Vec<KeywordHighlightRule> {
        match self {
            RulesTarget::Global => prefs.keyword_highlights.clone(),
            RulesTarget::Profile(id) => prefs
                .keyword_highlight_profiles
                .iter()
                .find(|profile| &profile.id == id)
                .map(|profile| profile.rules.clone())
                .unwrap_or_default(),
        }
    }

    pub fn set(&self, prefs: &mut PrefsState, rules: Vec<KeywordHighlightRule>) {
        match self {
            RulesTarget::Global => prefs.keyword_highlights = rules,
            RulesTarget::Profile(id) => {
                if let Some(profile) = prefs
                    .keyword_highlight_profiles
                    .iter_mut()
                    .find(|profile| &profile.id == id)
                {
                    profile.rules = rules;
                }
            }
        }
    }
}

/// `DEFAULT_RULE` with a fresh id.
fn default_rule() -> KeywordHighlightRule {
    KeywordHighlightRule {
        id: new_preference_id("highlight"),
        name: None,
        keyword: "ERROR".into(),
        foreground: KeywordHighlightRule::DEFAULT_FOREGROUND.into(),
        background: Some(KeywordHighlightRule::DEFAULT_BACKGROUND.into()),
        case_sensitive: false,
        whole_word: true,
        regex: None,
    }
}

/// Swap a rule with its neighbour (`move`); out-of-range moves do nothing.
pub fn move_rule(rules: &mut [KeywordHighlightRule], id: &str, offset: isize) {
    let Some(index) = rules.iter().position(|rule| rule.id == id) else {
        return;
    };
    let target = index as isize + offset;
    if target >= 0 && (target as usize) < rules.len() {
        rules.swap(index, target as usize);
    }
}

impl SettingsWindow {
    fn update_rules(
        &self,
        target: &RulesTarget,
        cx: &mut App,
        change: impl FnOnce(&mut Vec<KeywordHighlightRule>),
    ) {
        let target = target.clone();
        self.set_prefs(cx, move |prefs| {
            let mut rules = target.rules(prefs);
            change(&mut rules);
            target.set(prefs, rules);
        });
    }

    fn update_rule(
        &self,
        target: &RulesTarget,
        id: &str,
        cx: &mut App,
        change: impl FnOnce(&mut KeywordHighlightRule),
    ) {
        let id = id.to_string();
        self.update_rules(target, cx, move |rules| {
            if let Some(rule) = rules.iter_mut().find(|rule| rule.id == id) {
                change(rule);
            }
        });
    }

    pub fn highlighting(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let global = self.rules_editor(
            &theme,
            RulesTarget::Global,
            &prefs.keyword_highlights,
            "No keyword rules yet.",
            window,
            cx,
        );
        let global = group("Global rules")
            .description(secondary_text(
                &theme,
                "Highlighted in every terminal. A host can include these rules and add its assigned profile and own rules, or replace the global set entirely.",
            ))
            .flush()
            .row(div().p(px(16.)).child(global))
            .render(&theme);
        let profiles = self.profiles_group(&theme, &prefs, window, cx);
        page(
            &theme,
            "Highlighting",
            Some(secondary_text(
                &theme,
                "Keywords and regular expressions to color in terminal output.",
            )),
            vec![global, profiles],
        )
        .into_any_element()
    }

    fn profiles_group(
        &mut self,
        theme: &Theme,
        prefs: &PrefsState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profiles = &prefs.keyword_highlight_profiles;
        let selected = profiles
            .iter()
            .find(|profile| profile.id == self.state.highlight_selected)
            .or_else(|| profiles.first())
            .cloned();
        let at_limit = profiles.len() >= MAX_KEYWORD_HIGHLIGHT_PROFILES;
        let builtin_anchor = self.anchor("builtin-profiles");
        let mut section = group(texts::counted_title("Reusable profiles", profiles.len()))
            .description(secondary_text(
                theme,
                "Define a platform-specific rule set once, assign it in any host editor, and export it to share with another Muxus installation. Profiles for Nokia SR OS and SR Linux output are included; edit them freely, or reset them from Built-in.",
            ))
            .action(
                Button::new("highlight-new-profile", "New")
                    .start_icon("Add")
                    .disabled(at_limit)
                    .on_click(cx.listener(|this, _, _, cx| this.add_highlight_profile(cx))),
            )
            .action(
                div()
                    .relative()
                    .child(
                        Button::new("highlight-builtin", "Built-in")
                            .start_icon("LibraryAddOutlined")
                            .on_click(cx.listener(|this, _, _, cx| this.open_builtin_menu(cx))),
                    )
                    .child(bounds_probe(&builtin_anchor)),
            )
            .action(
                Button::new("highlight-import", "Import")
                    .start_icon("UploadFileOutlined")
                    .on_click(cx.listener(|this, _, _, cx| this.import_highlight_profiles(cx))),
            );
        let Some(profile) = selected else {
            return section
                .row(
                    div().px(px(16.)).py(px(20.)).child(
                        paragraph(
                            "No profiles yet. Create, import or add a built-in profile to reuse highlighting rules across hosts.",
                        )
                        .size(BODY2)
                        .line_height(snap(BODY2 * 1.43))
                        .color(theme.palette.text.secondary)
                        .center(),
                    ),
                )
                .render(theme);
        };
        let options: Vec<(String, String)> = profiles
            .iter()
            .map(|profile| (profile.id.clone(), texts::highlight_profile_option(profile)))
            .collect();
        let picker = self.row_select(
            theme,
            "highlight-profile",
            260.,
            options,
            &profile.id,
            false,
            cx,
            |this, id, _, cx| {
                this.state.highlight_selected = id;
                cx.notify();
            },
        );
        let name_key = format!("highlight-profile-name-{}", profile.id);
        let profile_id = profile.id.clone();
        let name = profile.name.clone();
        let name_input = self.input(
            &name_key,
            || name.clone(),
            None,
            false,
            window,
            cx,
            move |this, value, _, cx| {
                let id = profile_id.clone();
                this.set_prefs(cx, move |p| {
                    if let Some(profile) =
                        p.keyword_highlight_profiles.iter_mut().find(|p| p.id == id)
                    {
                        profile.name = value.chars().take(200).collect();
                    }
                });
            },
        );
        // A cleared name is filled in when the field is left.
        let blur_id = profile.id.clone();
        self.on_input_blur(&name_key, move |this, value, window, cx| {
            if muxus_core::js::js_trim(&value).is_empty() {
                let id = blur_id.clone();
                this.set_prefs(cx, move |p| {
                    if let Some(profile) =
                        p.keyword_highlight_profiles.iter_mut().find(|p| p.id == id)
                    {
                        profile.name = "Untitled profile".into();
                    }
                });
                let key = format!("highlight-profile-name-{blur_id}");
                this.set_input_value(&key, "Untitled profile", window, cx);
            }
        });
        self.sync_input(&name_key, &profile.name, window, cx);
        let editor = self.rules_editor(
            theme,
            RulesTarget::Profile(profile.id.clone()),
            &profile.rules,
            "No rules in this profile yet.",
            window,
            cx,
        );
        let export_one = profile.clone();
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .child(
                Button::new("highlight-export-profile", "Export profile")
                    .outlined()
                    .start_icon("DownloadOutlined")
                    .disabled(muxus_core::js::js_trim(&profile.name).is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let title = format!("{} highlighting profile", export_one.name);
                        let message = format!("Exported {}.", export_one.name);
                        this.export_highlight_profiles(
                            vec![export_one.clone()],
                            &title,
                            message,
                            cx,
                        );
                    })),
            );
        if profiles.len() > 1 {
            let all = profiles.clone();
            actions = actions.child(
                Button::new("highlight-export-all", "Export all")
                    .start_icon("DownloadOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let message = format!("Exported {} highlighting profiles.", all.len());
                        this.export_highlight_profiles(
                            all.clone(),
                            "highlighting profiles",
                            message,
                            cx,
                        );
                    })),
            );
        }
        let delete = profile.clone();
        actions = actions.child(
            div().ml_auto().child(
                Button::new("highlight-delete-profile", "Delete profile")
                    .color(ButtonColor::Error)
                    .start_icon("DeleteOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_highlight_profile(delete.clone(), cx)
                    })),
            ),
        );
        section = section
            .row(
                row(theme, "Profile")
                    .description_text(theme, "The profile to edit below.")
                    .control(picker)
                    .render(),
            )
            .row(
                row(theme, "Name")
                    .control(
                        field("settings-highlight-profile-name", &name_input)
                            .width(260.)
                            .render(theme, window, cx),
                    )
                    .render(),
            )
            .row(div().p(px(16.)).child(editor))
            .row(actions);
        section.render(theme)
    }

    fn add_highlight_profile(&mut self, cx: &mut Context<Self>) {
        let current = self.prefs(cx).keyword_highlight_profiles.clone();
        if current.len() >= MAX_KEYWORD_HIGHLIGHT_PROFILES {
            self.show_toast(
                Status::Error,
                format!(
                    "Muxus supports up to {MAX_KEYWORD_HIGHLIGHT_PROFILES} highlighting profiles. Delete one before creating another."
                ),
                cx,
            );
            return;
        }
        let profile = KeywordHighlightProfile {
            id: new_preference_id("highlight-profile"),
            name: texts::unused_profile_name(&current),
            rules: Vec::new(),
        };
        self.state.highlight_selected = profile.id.clone();
        self.set_prefs(cx, move |p| p.keyword_highlight_profiles.push(profile));
    }

    fn delete_highlight_profile(
        &mut self,
        profile: KeywordHighlightProfile,
        cx: &mut Context<Self>,
    ) {
        self.confirm(
            format!("Delete {}?", profile.name),
            "Hosts assigned to this profile will stop receiving its shared rules. Their own rules are not changed.",
            "Delete profile",
            true,
            move |this, _, cx| {
                let id = profile.id.clone();
                this.set_prefs(cx, move |p| {
                    p.keyword_highlight_profiles.retain(|c| c.id != id)
                });
                this.state.highlight_selected.clear();
            },
            cx,
        );
    }

    fn open_builtin_menu(&mut self, cx: &mut Context<Self>) {
        let installed: Vec<String> = self
            .prefs(cx)
            .keyword_highlight_profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect();
        let entries = builtin_highlight_profiles()
            .iter()
            .map(|builtin| MenuEntry::Item {
                value: builtin.id.clone(),
                label: builtin.name.clone().into(),
                secondary: Some(
                    if installed.contains(&builtin.id) {
                        "Reset to the shipped rules"
                    } else {
                        "Add to your profiles"
                    }
                    .into(),
                ),
                rich: None,
            })
            .collect();
        self.open_menu(
            "builtin-profiles",
            0.,
            None,
            entries,
            None,
            Rc::new(|this, id, _, cx| this.install_builtin_profile(&id, cx)),
            cx,
        );
    }

    /// Merging by id adds a missing built-in, or resets an installed one in
    /// place so hosts assigned to it keep following it.
    fn install_builtin_profile(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(builtin) = builtin_highlight_profiles()
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
        else {
            return;
        };
        let name = builtin.name.clone();
        let install = move |this: &mut Self, cx: &mut Context<Self>| {
            let current = this.prefs(cx).keyword_highlight_profiles.clone();
            match merge_highlight_profiles(&current, std::slice::from_ref(&builtin)) {
                Ok(merged) => {
                    this.state.highlight_selected = builtin.id.clone();
                    this.drop_inputs(&format!("rule-profile-{}-", builtin.id));
                    this.drop_inputs(&format!("highlight-profile-name-{}", builtin.id));
                    this.set_prefs(cx, move |p| p.keyword_highlight_profiles = merged);
                }
                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
            }
        };
        let installed = self
            .prefs(cx)
            .keyword_highlight_profiles
            .iter()
            .any(|profile| profile.id == id);
        if !installed {
            install(self, cx);
            return;
        }
        self.confirm(
            format!("Reset {name}?"),
            "Its name and rules are replaced with the defaults shipped with Muxus. Hosts assigned to it keep the assignment.",
            "Reset profile",
            true,
            move |this, _, cx| install(this, cx),
            cx,
        );
    }

    fn export_highlight_profiles(
        &mut self,
        profiles: Vec<KeywordHighlightProfile>,
        title: &str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        match create_highlight_profile_document(&profiles) {
            Ok(document) => {
                let text = serde_json::to_string_pretty(&document).unwrap_or_default() + "\n";
                let name = export_filename(title, "muxus-highlight.json");
                self.save_text_file_then(name, text, cx, move |this, cx| {
                    this.show_toast(Status::Success, message, cx);
                });
            }
            Err(error) => self.show_toast(Status::Error, error.to_string(), cx),
        }
    }

    fn import_highlight_profiles(&mut self, cx: &mut Context<Self>) {
        self.pick_file(cx, |this, path, cx| {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if size as usize > MAX_HIGHLIGHT_PROFILE_FILE_BYTES {
                this.show_toast(
                    Status::Error,
                    "That highlighting profile file is too large.",
                    cx,
                );
                return;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    this.show_toast(Status::Error, error.to_string(), cx);
                    return;
                }
            };
            let current = this.prefs(cx).keyword_highlight_profiles.clone();
            let result = parse_highlight_profile_document(&text).and_then(|document| {
                merge_highlight_profiles(&current, &document.profiles)
                    .map(|merged| (document, merged))
            });
            match result {
                Ok((document, merged)) => {
                    this.state.highlight_selected = document
                        .profiles
                        .first()
                        .map(|p| p.id.clone())
                        .unwrap_or_default();
                    for profile in &document.profiles {
                        this.drop_inputs(&format!("rule-profile-{}-", profile.id));
                        this.drop_inputs(&format!("highlight-profile-name-{}", profile.id));
                    }
                    let count = document.profiles.len();
                    this.set_prefs(cx, move |p| p.keyword_highlight_profiles = merged);
                    this.show_toast(
                        Status::Success,
                        format!(
                            "Imported {count} highlighting profile{}. Matching IDs were updated.",
                            if count == 1 { "" } else { "s" }
                        ),
                        cx,
                    );
                }
                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
            }
        });
    }

    /// `KeywordHighlightRulesEditor`.
    pub fn rules_editor(
        &mut self,
        theme: &Theme,
        target: RulesTarget,
        rules: &[KeywordHighlightRule],
        empty_message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor_key = target.key();
        if self.state.rules_json.contains_key(&editor_key) {
            return self.rules_json_editor(theme, target, window, cx);
        }
        let mut list = div().flex().flex_col().gap(px(6.));
        if rules.is_empty() {
            list = list.child(
                label(empty_message.to_string())
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .color(theme.palette.text.secondary),
            );
        }
        let count = rules.len();
        for (index, rule) in rules.iter().enumerate() {
            let row = self.rule_row(
                theme,
                &target,
                rule,
                index == 0,
                index + 1 == count,
                window,
                cx,
            );
            list = list.child(row);
        }
        let add_target = target.clone();
        let json_target = target;
        list.child(
            div()
                .flex()
                .gap(px(8.))
                .pt(px(4.))
                .child(
                    Button::new(
                        SharedString::from(format!("{editor_key}-add-rule")),
                        "Add rule",
                    )
                    .start_icon("Add")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_rules(&add_target, cx, |rules| rules.push(default_rule()));
                    })),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("{editor_key}-edit-json")),
                        "Edit as JSON",
                    )
                    .start_icon("DataObjectOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = json_target.key();
                        this.drop_inputs(&format!("rules-json-{key}"));
                        this.state.rules_json.insert(key, None);
                        cx.notify();
                    })),
                ),
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn rule_row(
        &mut self,
        theme: &Theme,
        target: &RulesTarget,
        rule: &KeywordHighlightRule,
        first: bool,
        last: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = format!("rule-{}-{}", target.key(), rule.id);
        let regex = rule.regex.unwrap_or(false);
        let pattern_error = keyword_pattern_error(&rule.keyword, regex);

        let name_key = format!("{prefix}-name");
        let (t, id) = (target.clone(), rule.id.clone());
        let name = rule.name.clone().unwrap_or_default();
        let name_input = self.input(
            &name_key,
            || name.clone(),
            Some("Name (optional)"),
            false,
            window,
            cx,
            move |this, value, _, cx| {
                let value: String = value.chars().take(100).collect();
                this.update_rule(&t, &id, cx, move |rule| {
                    rule.name = (!value.is_empty()).then_some(value);
                });
            },
        );
        self.sync_input(
            &name_key,
            rule.name.as_deref().unwrap_or_default(),
            window,
            cx,
        );

        let keyword_key = format!("{prefix}-keyword");
        let (t, id) = (target.clone(), rule.id.clone());
        let keyword = rule.keyword.clone();
        let keyword_input = self.input(
            &keyword_key,
            || keyword.clone(),
            Some(if regex {
                "Regular expression"
            } else {
                "Keyword"
            }),
            false,
            window,
            cx,
            move |this, value, _, cx| {
                let value: String = value.chars().take(500).collect();
                this.update_rule(&t, &id, cx, move |rule| rule.keyword = value);
            },
        );
        self.sync_input(&keyword_key, &rule.keyword, window, cx);
        let mut keyword_field = field(
            SharedString::from(format!("{prefix}-keyword-field")),
            &keyword_input,
        )
        .error(rule.keyword.is_empty() || pattern_error.is_some())
        .helper_opt(pattern_error);
        if regex {
            keyword_field = keyword_field.font(fonts::MONO, 13.);
        }

        let flags = div()
            .flex()
            .flex_none()
            .gap(px(2.))
            .child(self.flag_toggle(
                theme,
                target,
                rule,
                ("Match case", "Aa", false),
                rule.case_sensitive,
                |rule, value| rule.case_sensitive = value,
            ))
            .child(self.flag_toggle(
                theme,
                target,
                rule,
                ("Whole word", "ab", true),
                rule.whole_word,
                |rule, value| rule.whole_word = value,
            ))
            .child(self.flag_toggle(
                theme,
                target,
                rule,
                ("Regular expression", ".*", false),
                regex,
                |rule, value| rule.regex = value.then_some(true),
            ));

        let mut actions = div().flex().flex_none().items_center().h(px(ROW_HEIGHT));
        for (tooltip, icon_name, offset, disabled) in [
            ("Move up", "ArrowUpward", -1isize, first),
            ("Move down", "ArrowDownward", 1, last),
        ] {
            let (target, id) = (target.clone(), rule.id.clone());
            actions = actions.child(
                IconButton::new(SharedString::from(format!("{prefix}-{tooltip}")), icon_name)
                    .disabled(disabled)
                    .tooltip(tooltip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.update_rules(&target, cx, move |rules| move_rule(rules, &id, offset));
                    })),
            );
        }
        let (delete_target, delete_id, delete_prefix) =
            (target.clone(), rule.id.clone(), prefix.clone());
        actions = actions.child(
            IconButton::new(
                SharedString::from(format!("{prefix}-delete")),
                "DeleteOutlined",
            )
            .color(ButtonColor::Error)
            .tooltip("Delete rule")
            .on_click(cx.listener(move |this, _, _, cx| {
                let id = delete_id.clone();
                this.drop_inputs(&delete_prefix);
                this.update_rules(&delete_target, cx, move |rules| {
                    rules.retain(|rule| rule.id != id)
                });
            })),
        );

        div()
            .flex()
            .items_start()
            .gap(px(6.))
            .child(self.rule_style_button(theme, target, rule, cx))
            .child(
                div().w(px(170.)).flex_none().child(
                    field(
                        SharedString::from(format!("{prefix}-name-field")),
                        &name_input,
                    )
                    .width(170.)
                    .render(theme, window, cx),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .child(keyword_field.render(theme, window, cx)),
            )
            .child(flags)
            .child(actions)
            .into_any_element()
    }

    /// A VS Code–style search option: a monospace glyph with its name as tooltip.
    fn flag_toggle(
        &self,
        theme: &Theme,
        target: &RulesTarget,
        rule: &KeywordHighlightRule,
        (name, glyph, underline): (&'static str, &'static str, bool),
        selected: bool,
        set: fn(&mut KeywordHighlightRule, bool),
    ) -> AnyElement {
        let p = theme.palette;
        let ink = if selected {
            p.text.primary
        } else {
            p.action.active
        };
        let base = if selected {
            alpha(p.text.primary, p.action.selected_opacity)
        } else {
            Hsla::transparent_black()
        };
        let hover = alpha(
            p.text.primary,
            p.action.hover_opacity
                + if selected {
                    p.action.selected_opacity
                } else {
                    0.
                },
        );
        let (target, id) = (target.clone(), rule.id.clone());
        let prefs = self.prefs.clone();
        let mut text = div()
            .font_family(fonts::MONO)
            .text_size(px(13.))
            .font_weight(FontWeight(550.))
            .line_height(px(snap(13. * 1.75)))
            .text_color(ink)
            .child(glyph);
        if underline {
            text = text.underline();
        }
        div()
            .id(SharedString::from(format!(
                "rule-{}-{}-{name}",
                target.key(),
                rule.id
            )))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(34.))
            .h(px(ROW_HEIGHT))
            .border_1()
            .border_color(p.divider)
            .rounded(px(8.))
            .bg(base)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .tooltip(Tooltip::text(name))
            .on_click(move |_, _, cx| {
                let (target, id) = (target.clone(), id.clone());
                prefs.update(cx, |model, cx| {
                    model.update(cx, |prefs| {
                        let mut rules = target.rules(prefs);
                        if let Some(rule) = rules.iter_mut().find(|rule| rule.id == id) {
                            set(rule, !selected);
                        }
                        target.set(prefs, rules);
                    })
                });
            })
            .child(text)
            .into_any_element()
    }

    /// A preview of the rule's colors that opens its color settings.
    fn rule_style_button(
        &mut self,
        theme: &Theme,
        target: &RulesTarget,
        rule: &KeywordHighlightRule,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let key = format!("rule-{}-{}-style", target.key(), rule.id);
        let anchor = self.anchor(&key);
        let fg = try_hex(&rule.foreground).unwrap_or(theme.palette.common_white);
        let bg = rule.background.as_deref().and_then(try_hex);
        let open_key = (target.key(), rule.id.clone());
        let hover_border = p.text.primary;
        div()
            .id(SharedString::from(key))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(44.))
            .h(px(ROW_HEIGHT))
            .border_1()
            .border_color(p.divider)
            .rounded(px(8.))
            .bg(p.background_default)
            .cursor_pointer()
            .hover(move |s| s.border_color(hover_border))
            .tooltip(Tooltip::text("Colors"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.rule_colors = Some(open_key.clone());
                cx.notify();
            }))
            .child(
                div()
                    .px(px(4.))
                    .rounded(px(4.))
                    .when_some(bg, |d, bg| d.bg(bg))
                    .font_family(fonts::MONO)
                    .text_size(px(13.))
                    .font_weight(FontWeight(600.))
                    .line_height(px(17.))
                    .text_color(fg)
                    .child("Ab"),
            )
            .child(bounds_probe(&anchor))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn rule_color_well(
        &mut self,
        theme: &Theme,
        target: &RulesTarget,
        rule_id: &str,
        name: &'static str,
        value: String,
        set: fn(&mut KeywordHighlightRule, String),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("rule-{}-{rule_id}-{name}", target.key());
        let anchor = self.anchor(&key);
        let color = try_hex(&value).unwrap_or_else(|| hex(MISSING_SCHEME_COLOR));
        let (target, id) = (target.clone(), rule_id.to_string());
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div().min_w(px(32.)).child(
                    label(name)
                        .size(BODY2)
                        .line_height(snap(BODY2 * 1.43))
                        .color(theme.palette.text.secondary),
                ),
            )
            .child(
                color_well(
                    SharedString::from(format!("{key}-well")),
                    theme,
                    color,
                    30.,
                    26.,
                    Rc::new(move |_, cx| {
                        let (target, id, key, value) =
                            (target.clone(), id.clone(), key.clone(), value.clone());
                        let _ = weak.update(cx, |this, cx| {
                            this.open_color_picker(
                                &key,
                                &value,
                                Rc::new(move |this, value, cx| {
                                    this.update_rule(&target, &id, cx, |rule| set(rule, value));
                                }),
                                cx,
                            );
                        });
                    }),
                )
                .child(bounds_probe(&anchor)),
            )
            .into_any_element()
    }

    /// The rule color popover (`RuleStyleButton`'s Popover).
    pub fn render_rule_colors(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target_key, rule_id) = self.state.rule_colors.clone()?;
        let target = RulesTarget::from_key(&target_key);
        let rule = target
            .rules(self.prefs(cx))
            .into_iter()
            .find(|rule| rule.id == rule_id)?;
        let theme = *cx.theme();
        let p = theme.palette;
        let anchor = self.anchor(&format!("rule-{target_key}-{rule_id}-style"));
        let position = muxus_ui::widgets::menu::below(&anchor)?;
        let text_well = self.rule_color_well(
            &theme,
            &target,
            &rule_id,
            "Text",
            rule.foreground.clone(),
            |rule, value| rule.foreground = value,
            cx,
        );
        let has_background = rule.background.is_some();
        let fill_well = rule.background.clone().map(|background| {
            self.rule_color_well(
                &theme,
                &target,
                &rule_id,
                "Fill",
                background,
                |rule, value| rule.background = Some(value),
                cx,
            )
        });
        let (t, id) = (target.clone(), rule.id.clone());
        let content = muxus_ui::widgets::menu::paper(&theme).py(px(0.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(16.))
                .child(text_well)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(
                                    Switch::new("rule-background-switch", has_background)
                                        .small()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.update_rule(&t, &id, cx, move |rule| {
                                                rule.background = (!has_background).then(|| {
                                                    KeywordHighlightRule::BACKGROUND_ON.to_string()
                                                });
                                            });
                                        })),
                                )
                                .child(
                                    label("Background")
                                        .size(BODY2)
                                        .line_height(snap(BODY2 * 1.43))
                                        .color(p.text.primary),
                                ),
                        )
                        .children(fill_well),
                ),
        );
        let weak = cx.entity().downgrade();
        Some(
            muxus_ui::widgets::menu::popup(
                position,
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.state.rule_colors = None;
                        cx.notify();
                    });
                },
                content,
            )
            .into_any_element(),
        )
    }

    /// `RulesJsonEditor`: the list as JSON text, applied explicitly.
    fn rules_json_editor(
        &mut self,
        theme: &Theme,
        target: RulesTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = target.key();
        let input_key = format!("rules-json-{key}");
        let error = self.state.rules_json.get(&key).cloned().flatten();
        let initial = keyword_highlight_rules_to_json(&target.rules(self.prefs(cx)));
        let error_key = key.clone();
        let state = self.textarea(
            &input_key,
            || initial.clone(),
            None,
            (8, 24),
            window,
            cx,
            move |this, _, _, cx| {
                if let Some(error) = this.state.rules_json.get_mut(&error_key) {
                    *error = None;
                }
                cx.notify();
            },
        );
        let (apply_target, apply_key, apply_input) = (target, key.clone(), input_key.clone());
        let apply = Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
            let Some(text) = this.textarea_value(&apply_input, cx) else {
                return;
            };
            let current = apply_target.rules(this.prefs(cx));
            match parse_keyword_highlight_rules_json(&text, &current) {
                Ok(rules) => {
                    this.state.rules_json.remove(&apply_key);
                    this.drop_inputs(&format!("rule-{apply_key}-"));
                    this.drop_inputs(&apply_input);
                    let target = apply_target.clone();
                    this.set_prefs(cx, move |p| target.set(p, rules));
                }
                Err(error) => {
                    this.state
                        .rules_json
                        .insert(apply_key.clone(), Some(error.to_string()));
                }
            }
            cx.notify();
        });
        let apply_click = apply.clone();
        let (cancel_key, cancel_input) = (key.clone(), input_key.clone());
        let note: AnyElement = match &error {
            Some(error) => outlined_alert(
                theme,
                Status::Error,
                alert_text(theme, Status::Error, error.clone()),
            )
            .into_any_element(),
            None => caption(
                theme,
                "An array of rules. Each needs a keyword and a foreground color; name, background, regex, caseSensitive and wholeWord are optional. Backslashes in a regex are written twice in JSON, as in \\\\d+.",
                1.66,
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        let keystroke = &event.keystroke;
                        if keystroke.key == "enter"
                            && (keystroke.modifiers.control || keystroke.modifiers.platform)
                        {
                            cx.stop_propagation();
                            apply(this, cx);
                        }
                    }))
                    .child(
                        field(SharedString::from(format!("{input_key}-field")), &state)
                            .font(fonts::MONO, 12.5)
                            .min_rows(8)
                            .error(error.is_some())
                            .render(theme, window, cx),
                    ),
            )
            .child(note)
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new(SharedString::from(format!("{key}-apply-json")), "Apply")
                            .contained()
                            .on_click(cx.listener(move |this, _, _, cx| apply_click(this, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{key}-cancel-json")), "Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.rules_json.remove(&cancel_key);
                                this.drop_inputs(&cancel_input);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str) -> KeywordHighlightRule {
        KeywordHighlightRule {
            id: id.into(),
            ..default_rule()
        }
    }

    #[test]
    fn moving_rules_swaps_neighbours_within_bounds() {
        let mut rules = vec![rule("a"), rule("b"), rule("c")];
        move_rule(&mut rules, "a", -1);
        move_rule(&mut rules, "c", 1);
        move_rule(&mut rules, "b", -1);
        let ids: Vec<&str> = rules.iter().map(|rule| rule.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn targets_read_and_write_their_own_list() {
        let mut prefs = PrefsState::default();
        let profile_id = prefs.keyword_highlight_profiles[0].id.clone();
        let target = RulesTarget::Profile(profile_id.clone());
        target.set(&mut prefs, vec![rule("x")]);
        assert_eq!(target.rules(&prefs).len(), 1);
        assert!(RulesTarget::Global.rules(&prefs).is_empty());
        assert_eq!(RulesTarget::from_key(&target.key()), target);
        assert_eq!(default_rule().keyword, "ERROR");
    }
}
