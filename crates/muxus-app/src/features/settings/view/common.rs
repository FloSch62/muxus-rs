//! Controls bound to preferences, shared by the sections: a switch row, a
//! select row and number fields, each writing through [`SettingsPrefs`].
//!
//! [`SettingsPrefs`]: super::super::prefs_model::SettingsPrefs

use std::rc::Rc;

use muxus_store::prefs::PrefsState;
use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, SharedString, Window};
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Switch;

use super::kit::{Row, row, row_select, select_value};
use super::window::{MenuEntry, SettingsWindow};

impl SettingsWindow {
    /// A small switch bound to a boolean preference.
    pub fn pref_switch(
        &self,
        id: &'static str,
        checked: bool,
        disabled: bool,
        set: fn(&mut PrefsState, bool),
    ) -> AnyElement {
        let prefs = self.prefs.clone();
        Switch::new(id, checked)
            .small()
            .disabled(disabled)
            .on_click(move |_, _, cx| {
                prefs.update(cx, |model, cx| model.update(cx, |p| set(p, !checked)));
            })
            .into_any_element()
    }

    /// `SettingRow` with a switch on the right.
    pub fn switch_row(
        &self,
        theme: &Theme,
        id: &'static str,
        label: &str,
        description: Option<&str>,
        checked: bool,
        set: fn(&mut PrefsState, bool),
    ) -> AnyElement {
        let mut setting = row(theme, label.to_string());
        if let Some(description) = description {
            setting = setting.description_text(theme, description.to_string());
        }
        setting
            .control(self.pref_switch(id, checked, false, set))
            .render()
    }

    /// `RowSelect`: the closed select and the menu it opens.
    #[allow(clippy::too_many_arguments)]
    pub fn row_select(
        &mut self,
        theme: &Theme,
        key: &'static str,
        width: f32,
        options: Vec<(String, String)>,
        value: &str,
        disabled: bool,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let anchor = self.anchor(key);
        let shown = options
            .iter()
            .find(|(option, _)| option == value)
            .map(|(_, text)| text.clone())
            .unwrap_or_default();
        let open = self.menu_open(key);
        let on_pick = Rc::new(on_pick);
        let entries: Vec<MenuEntry> = options
            .iter()
            .map(|(value, text)| MenuEntry::item(value.clone(), text.clone()))
            .collect();
        let selected = value.to_string();
        let this = cx.entity().downgrade();
        row_select(
            SharedString::from(format!("select-{key}")),
            width,
            select_value(theme, shown),
            open,
            disabled,
            &anchor,
            theme,
            Rc::new(move |_, cx| {
                let entries = entries.clone();
                let selected = selected.clone();
                let on_pick = on_pick.clone();
                let _ = this.update(cx, |this, cx| {
                    this.open_menu(
                        key,
                        width,
                        None,
                        entries,
                        Some(selected),
                        Rc::new(move |this, value, window, cx| on_pick(this, value, window, cx)),
                        cx,
                    );
                });
            }),
        )
        .into_any_element()
    }

    /// A settings row whose control is a [`SettingsWindow::row_select`].
    #[allow(clippy::too_many_arguments)]
    pub fn select_row(
        &mut self,
        theme: &Theme,
        label: &str,
        description: Option<&str>,
        key: &'static str,
        width: f32,
        options: Vec<(String, String)>,
        value: &str,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let control = self.row_select(theme, key, width, options, value, false, cx, on_pick);
        let mut setting: Row = row(theme, label.to_string());
        if let Some(description) = description {
            setting = setting.description_text(theme, description.to_string());
        }
        setting.control(control).render()
    }
}
