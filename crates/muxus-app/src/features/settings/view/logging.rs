//! `SessionLoggingSection` of `SettingsDialog.tsx` with
//! `SessionLoggingPolicyFields.tsx`: the default and local-terminal
//! policies, log files, and history storage. Unlike the rest of Settings,
//! these forms save explicitly; unsaved edits mark the section and hold the
//! window.

use muxus_core::api::{SessionHistorySettings, SessionLogFileSettings};
use muxus_ui::gpui_kit::base::input::InputState;
use muxus_ui::gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{Button, Switch, Tooltip};

use super::super::draft::{
    HistoryStorageDraft, LogFileDraft, SessionLoggingDraft, bytes_to_gib, clamp_integer,
    format_retention, format_storage_bytes, history_storage_values, host_session_logging_draft,
    js_number, session_logging_policy_input,
};
use super::super::js::number_to_string;
use super::super::texts;
use super::fields::field;
use super::kit::{
    BODY1, BODY2, CAPTION, alert_text, caption, group, outlined_alert, page, row, secondary_text,
    snap,
};
use super::window::SettingsWindow;

const MIB: u64 = 1024 * 1024;
/// `fontFamily: 'monospace'` as Chrome resolves it on Linux.
const MONOSPACE: &str = "DejaVu Sans Mono";

/// Which policy form a control belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PolicyForm {
    Default,
    Local,
}

impl PolicyForm {
    fn key(self) -> &'static str {
        match self {
            PolicyForm::Default => "default",
            PolicyForm::Local => "local",
        }
    }
}

impl SettingsWindow {
    fn policy_draft(&self, form: PolicyForm) -> SessionLoggingDraft {
        match form {
            PolicyForm::Default => *self.state.logging.default.draft(),
            PolicyForm::Local => *self.state.logging.local.draft(),
        }
    }

    fn edit_policy(&mut self, form: PolicyForm, change: impl FnOnce(&mut SessionLoggingDraft)) {
        match form {
            PolicyForm::Default => self.state.logging.default.edit(change),
            PolicyForm::Local => self.state.logging.local.edit(change),
        }
        self.nav.logging_dirty = self.state.logging.dirty();
    }

    /// The stored values arrived: drafts without edits follow them.
    pub fn logging_loaded(&mut self, cx: &mut Context<Self>) {
        if let Some(policy) = &self.data.logging_default {
            let stored = host_session_logging_draft(policy, false);
            self.state.logging.default.observe(&stored);
        }
        if let Some(policy) = &self.data.logging_local {
            let stored = host_session_logging_draft(policy, !policy.overridden);
            self.state.logging.local.observe(&stored);
        }
        if let Some(status) = &self.data.log_files {
            let stored = LogFileDraft {
                directory: status.settings.directory.clone().unwrap_or_default(),
                filename_pattern: status.settings.filename_pattern.clone(),
                timestamps: status.settings.timestamps,
            };
            self.state.logging.files.observe(&stored);
        }
        if let Some(status) = &self.data.history_storage {
            let settings = &status.settings;
            let stored = HistoryStorageDraft {
                storage_location: settings.storage_location.clone().unwrap_or_default(),
                max_total_gib: bytes_to_gib(settings.max_total_bytes),
                min_free_gib: bytes_to_gib(settings.min_free_bytes),
                min_free_percent: number_to_string(settings.min_free_percent),
                max_age_days: settings
                    .max_age_days
                    .filter(|days| *days > 0)
                    .map(|days| days.to_string())
                    .unwrap_or_default(),
            };
            self.state.logging.storage.observe(&stored);
        }
        self.nav.logging_dirty = self.state.logging.dirty();
        cx.notify();
    }

    pub fn logging(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let default = self.policy_group(&theme, PolicyForm::Default, window, cx);
        let local = self.policy_group(&theme, PolicyForm::Local, window, cx);
        let files = self.log_file_group(&theme, window, cx);
        let storage = self.storage_group(&theme, window, cx);
        page(
            &theme,
            "Session logging",
            Some(secondary_text(
                &theme,
                "Record terminal sessions to search and replay them later, or write them to plain-text log files. Off by default. Unlike the rest of Settings, changes here apply once saved, and only to sessions opened afterwards.",
            )),
            vec![default, local, files, storage],
        )
        .into_any_element()
    }

    fn policy_group(
        &mut self,
        theme: &Theme,
        form: PolicyForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let draft = self.policy_draft(form);
        let allow_inherit = form == PolicyForm::Local;
        let fields = self.policy_fields(theme, form, draft, allow_inherit, window, cx);
        let (dirty, saving, loaded) = match form {
            PolicyForm::Default => (
                self.state.logging.default.dirty(),
                self.state.logging.saving_default,
                self.data.logging_default.is_some(),
            ),
            PolicyForm::Local => (
                self.state.logging.local.dirty(),
                self.state.logging.saving_local,
                self.data.logging_local.is_some(),
            ),
        };
        let disabled = !loaded || !draft.loaded || !dirty || saving;
        let (title, description, button) = match form {
            PolicyForm::Default => (
                "Default policy",
                "Inherited by every host without a logging setting of its own.",
                "Save default policy",
            ),
            PolicyForm::Local => (
                "Local terminals",
                "Local shells have no host entry, so their optional override lives here. It applies to newly opened local terminals.",
                "Save local terminal policy",
            ),
        };
        let bar = save_bar(
            theme,
            SharedString::from(format!("save-policy-{}", form.key())),
            dirty,
            disabled,
            button,
            cx.listener(move |this, _, _, cx| this.save_policy(form, cx)),
        );
        group(title)
            .description(secondary_text(theme, description))
            .flush()
            .row(div().p(px(16.)).child(fields))
            .row(bar)
            .render(theme)
    }

    fn save_policy(&mut self, form: PolicyForm, cx: &mut Context<Self>) {
        let draft = self.policy_draft(form);
        let backend = self.backend.clone();
        match form {
            PolicyForm::Default => self.state.logging.saving_default = true,
            PolicyForm::Local => self.state.logging.saving_local = true,
        }
        let key = match form {
            PolicyForm::Default => "*",
            PolicyForm::Local => "local",
        };
        self.fetch(
            cx,
            async move {
                if form == PolicyForm::Local && draft.inherit {
                    backend.delete_session_logging_policy(key).await?;
                } else {
                    backend
                        .put_session_logging_policy(key, session_logging_policy_input(&draft))
                        .await?;
                }
                backend.session_logging_policy(key).await
            },
            move |this, result, cx| {
                match form {
                    PolicyForm::Default => this.state.logging.saving_default = false,
                    PolicyForm::Local => this.state.logging.saving_local = false,
                }
                match result {
                    Ok(policy) => {
                        match form {
                            PolicyForm::Default => {
                                this.state.logging.default.saved();
                                this.data.logging_default = Some(policy);
                            }
                            PolicyForm::Local => {
                                this.state.logging.local.saved();
                                this.data.logging_local = Some(policy);
                            }
                        }
                        this.logging_loaded(cx);
                        this.show_toast(
                            Status::Success,
                            match form {
                                PolicyForm::Default => "Default session logging settings saved.",
                                PolicyForm::Local => "Local terminal logging settings saved.",
                            },
                            cx,
                        );
                    }
                    Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                }
            },
        );
    }

    /// `SessionLoggingPolicyFields`.
    fn policy_fields(
        &mut self,
        theme: &Theme,
        form: PolicyForm,
        draft: SessionLoggingDraft,
        allow_inherit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let disabled = !draft.loaded || (allow_inherit && draft.inherit);
        let mut stack = div().flex().flex_col().gap(px(18.));
        let key = form.key();
        if allow_inherit {
            stack = stack.child(self.policy_switch(
                theme,
                format!("logging-{key}-inherit"),
                "Use default session logging settings",
                texts::inherited_logging_caption(draft.inherit, draft.enabled, draft.log_to_file),
                (draft.inherit, !draft.loaded),
                form,
                |d, value| d.inherit = value,
                cx,
            ));
        }
        stack = stack
            .child(self.policy_switch(
                theme,
                format!("logging-{key}-enabled"),
                "Log terminal sessions",
                "Store timestamped raw activity and a searchable normalized transcript.".into(),
                (draft.enabled, disabled),
                form,
                |d, value| d.enabled = value,
                cx,
            ))
            .child(self.policy_switch(
                theme,
                format!("logging-{key}-files"),
                "Write log files",
                "Save each new session to a plain-text log file as it runs, independent of history."
                    .into(),
                (draft.log_to_file, disabled),
                form,
                |d, value| d.log_to_file = value,
                cx,
            ))
            .child(
                div()
                    .id(SharedString::from(format!("logging-{key}-input-tip")))
                    .tooltip(Tooltip::text(
                        "When disabled, client input bytes are never persisted. Commands echoed by the remote shell are still output.",
                    ))
                    .child(self.policy_switch(
                        theme,
                        format!("logging-{key}-input"),
                        "Record terminal input",
                        "Off is safer: typed commands may contain passwords or tokens.".into(),
                        (
                            draft.capture_input,
                            disabled || (!draft.enabled && !draft.log_to_file),
                        ),
                        form,
                        |d, value| d.capture_input = value,
                        cx,
                    )),
            );

        let numbers_disabled = disabled || !draft.enabled;
        let part_key = format!("logging-{key}-part");
        let part_mib = (draft.max_part_bytes as f64 / MIB as f64).round();
        let part_input = self.input(
            &part_key,
            || number_to_string(part_mib),
            None,
            false,
            window,
            cx,
            move |this, value, window, cx| {
                let mib = clamp_integer(js_number(&value), 1., 1024.);
                this.edit_policy(form, |d| d.max_part_bytes = mib as u64 * MIB);
                let shown = number_to_string(mib);
                if shown != value {
                    let key = format!("logging-{}-part", form.key());
                    this.set_input_value(&key, &shown, window, cx);
                }
            },
        );
        self.sync_input(&part_key, &number_to_string(part_mib), window, cx);
        let parts_key = format!("logging-{key}-parts");
        let parts = f64::from(draft.max_parts);
        let parts_input = self.input(
            &parts_key,
            || number_to_string(parts),
            None,
            false,
            window,
            cx,
            move |this, value, window, cx| {
                let parts = clamp_integer(js_number(&value), 1., 1000.);
                this.edit_policy(form, |d| d.max_parts = parts as u32);
                let shown = number_to_string(parts);
                if shown != value {
                    let key = format!("logging-{}-parts", form.key());
                    this.set_input_value(&key, &shown, window, cx);
                }
            },
        );
        self.sync_input(&parts_key, &number_to_string(parts), window, cx);
        let paper = theme.palette.background_paper;
        stack = stack.child(
            div()
                .flex()
                .gap(px(12.))
                .child(
                    field(SharedString::from(format!("{part_key}-field")), &part_input)
                        .label("Rotate part at", paper)
                        .end_text("MiB")
                        .width(190.)
                        .disabled(numbers_disabled)
                        .render(theme, window, cx),
                )
                .child(
                    field(
                        SharedString::from(format!("{parts_key}-field")),
                        &parts_input,
                    )
                    .label("Retain newest parts", paper)
                    .width(190.)
                    .disabled(numbers_disabled)
                    .render(theme, window, cx),
                ),
        );
        if draft.enabled && !disabled {
            stack = stack.child(caption(
                theme,
                format!(
                    "Maximum retained raw data per session: approximately {}.",
                    format_retention(&draft)
                ),
                1.66,
            ));
        }
        stack.into_any_element()
    }

    /// A medium switch with a label and its caption (`FormControlLabel`).
    #[allow(clippy::too_many_arguments)]
    fn policy_switch(
        &self,
        theme: &Theme,
        id: String,
        title: &str,
        detail: String,
        (checked, disabled): (bool, bool),
        form: PolicyForm,
        set: fn(&mut SessionLoggingDraft, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let ink = if disabled {
            p.text.disabled
        } else {
            p.text.primary
        };
        div()
            .flex()
            .items_center()
            .child(
                Switch::new(SharedString::from(id), checked)
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_policy(form, |d| set(d, !checked));
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        label(title.to_string())
                            .size(BODY2)
                            .line_height(snap(BODY2 * 1.43))
                            .color(ink),
                    )
                    // The caption sits in the label's body1 line box.
                    .child(
                        paragraph(detail)
                            .size(CAPTION)
                            .line_height(snap(BODY1 * 1.5))
                            .color(if disabled {
                                p.text.disabled
                            } else {
                                p.text.secondary
                            }),
                    ),
            )
            .into_any_element()
    }

    fn log_file_group(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let draft = self.state.logging.files.draft().clone();
        let status = self.data.log_files.clone();
        let paper = theme.palette.background_paper;
        let directory = draft.directory.clone();
        let folder = self.input(
            "log-folder",
            || directory.clone(),
            status.as_ref().map(|s| s.active_directory.as_str()),
            false,
            window,
            cx,
            |this, value, _, cx| {
                this.state.logging.files.edit(|d| d.directory = value);
                this.nav.logging_dirty = this.state.logging.dirty();
                cx.notify();
            },
        );
        self.sync_input("log-folder", &draft.directory, window, cx);
        let pattern = draft.filename_pattern.clone();
        let name = self.input(
            "log-file-name",
            || pattern.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                this.state
                    .logging
                    .files
                    .edit(|d| d.filename_pattern = value);
                this.nav.logging_dirty = this.state.logging.dirty();
                cx.notify();
            },
        );
        self.sync_input("log-file-name", &draft.filename_pattern, window, cx);
        let (helper, pattern_error) = texts::log_file_pattern_helper(
            &draft.filename_pattern,
            chrono::Local::now().naive_local(),
        );
        let dirty = self.state.logging.files.dirty();
        let disabled =
            status.is_none() || !dirty || pattern_error || self.state.logging.saving_files;
        let timestamps = draft.timestamps;
        let switch = Switch::new("settings-log-file-timestamps", timestamps)
            .small()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state
                    .logging
                    .files
                    .edit(|d| d.timestamps = !timestamps);
                this.nav.logging_dirty = this.state.logging.dirty();
                cx.notify();
            }));
        group("Log files")
            .description(secondary_text(
                theme,
                "Plain-text logs written while a session runs, started from the terminal menu or for every session by the policies above. They are ordinary files: retention and quotas never remove them.",
            ))
            .flush()
            .row(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        field("settings-log-folder", &folder)
                            .label("Log folder", paper)
                            .helper("Leave blank for the default folder.")
                            .font(MONOSPACE, BODY1)
                            .render(theme, window, cx),
                    )
                    .child(
                        field("settings-log-file-name", &name)
                            .label("File name", paper)
                            .helper(helper)
                            .error(pattern_error)
                            .font(MONOSPACE, BODY1)
                            .render(theme, window, cx),
                    ),
            )
            .row(
                row(theme, "Show timestamps (UTC)")
                    .description_text(
                        theme,
                        "Prefix each line with the time it last changed, as in the clean log.",
                    )
                    .control(switch)
                    .render(),
            )
            .row(save_bar(
                theme,
                "save-log-files".into(),
                dirty,
                disabled,
                "Save log file settings",
                cx.listener(|this, _, _, cx| this.save_log_files(cx)),
            ))
            .render(theme)
    }

    fn save_log_files(&mut self, cx: &mut Context<Self>) {
        let draft = self.state.logging.files.draft().clone();
        let directory = muxus_core::js::js_trim(&draft.directory).to_string();
        let settings = SessionLogFileSettings {
            directory: (!directory.is_empty()).then_some(directory),
            filename_pattern: muxus_core::js::js_trim(&draft.filename_pattern).to_string(),
            timestamps: draft.timestamps,
        };
        let backend = self.backend.clone();
        self.state.logging.saving_files = true;
        self.fetch(
            cx,
            async move { backend.put_session_log_files(settings).await },
            |this, result, cx| {
                this.state.logging.saving_files = false;
                match result {
                    Ok(status) => {
                        this.state.logging.files.saved();
                        this.data.log_files = Some(status);
                        this.logging_loaded(cx);
                        this.show_toast(Status::Success, "Log file settings saved.", cx);
                    }
                    Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                }
            },
        );
    }

    fn storage_group(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let draft = self.state.logging.storage.draft().clone();
        let status = self.data.history_storage.clone();
        let paper = p.background_paper;
        let usage = match &status {
            Some(status) => div()
                .flex()
                .flex_col()
                .child(
                    paragraph(format!(
                        "{} used \u{b7} {} free",
                        format_storage_bytes(status.usage_bytes),
                        format_storage_bytes(status.free_bytes)
                    ))
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.5))
                    .color(p.text.secondary),
                )
                .child(
                    label(status.active_storage_location.clone())
                        .family(MONOSPACE)
                        .size(CAPTION)
                        .line_height(snap(CAPTION * 1.5))
                        .color(p.text.secondary),
                )
                .into_any_element(),
            None => div()
                .w(px(220.))
                .py(px(2.))
                .child(super::kit::skeleton(theme, snap(CAPTION * 1.5) - 4.))
                .into_any_element(),
        };
        let mut usage_row = row(theme, "Current usage").description(usage);
        if let Some(warning) = status.as_ref().and_then(|s| s.warning.clone()) {
            usage_row = usage_row.child(div().mt(px(10.)).child(outlined_alert(
                theme,
                Status::Warning,
                alert_text(theme, Status::Warning, warning),
            )));
        }

        let location = self.storage_input(
            "history-location",
            draft.storage_location.clone(),
            status.as_ref().map(|s| s.active_storage_location.clone()),
            |d, value| d.storage_location = value,
            window,
            cx,
        );
        let max_total = self.storage_input(
            "history-max-total",
            draft.max_total_gib.clone(),
            None,
            |d, value| d.max_total_gib = value,
            window,
            cx,
        );
        let min_free = self.storage_input(
            "history-min-free",
            draft.min_free_gib.clone(),
            None,
            |d, value| d.min_free_gib = value,
            window,
            cx,
        );
        let min_percent = self.storage_input(
            "history-min-percent",
            draft.min_free_percent.clone(),
            None,
            |d, value| d.min_free_percent = value,
            window,
            cx,
        );
        let max_age = self.storage_input(
            "history-max-age",
            draft.max_age_days.clone(),
            Some("No limit".into()),
            |d, value| d.max_age_days = value,
            window,
            cx,
        );
        let values = history_storage_values(&draft);
        let dirty = self.state.logging.storage.dirty();
        let disabled =
            status.is_none() || !values.valid || !dirty || self.state.logging.saving_storage;
        let mut grid = div().flex().gap(px(12.));
        for (id, title, state) in [
            (
                "settings-history-max-total",
                "Maximum history (GiB)",
                &max_total,
            ),
            ("settings-history-min-free", "Minimum free (GiB)", &min_free),
            (
                "settings-history-min-percent",
                "Minimum free (%)",
                &min_percent,
            ),
            ("settings-history-max-age", "Maximum age (days)", &max_age),
        ] {
            grid = grid.child(
                div().flex_1().min_w_0().child(
                    field(id, state)
                        .label(title, paper)
                        .render(theme, window, cx),
                ),
            );
        }
        group("Storage & retention")
            .description(secondary_text(
                theme,
                "The quota counts compressed recordings, the search database and its WAL. Cleanup removes the oldest unpinned finished sessions down to about 85% of the limit.",
            ))
            .flush()
            .row(usage_row.render())
            .row(
                div()
                    .p(px(16.))
                    .pt(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        field("settings-history-location", &location)
                            .label("History location", paper)
                            .helper("Leave blank for the platform default. Takes effect after a restart; existing history stays at its old location.")
                            .font(MONOSPACE, BODY1)
                            .render(theme, window, cx),
                    )
                    .child(grid)
                    .child(div().mt(px(-8.)).child(caption(
                        theme,
                        "A blank maximum age keeps sessions indefinitely, subject to the size and free-space limits.",
                        1.66,
                    ))),
            )
            .row(save_bar(
                theme,
                "save-history-storage".into(),
                dirty,
                disabled,
                "Save storage limits",
                cx.listener(|this, _, _, cx| this.save_storage(cx)),
            ))
            .render(theme)
    }

    fn storage_input(
        &mut self,
        key: &'static str,
        value: String,
        placeholder: Option<String>,
        set: fn(&mut HistoryStorageDraft, String),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let initial = value.clone();
        let state = self.input(
            key,
            || initial.clone(),
            placeholder.as_deref(),
            false,
            window,
            cx,
            move |this, value, _, cx| {
                this.state.logging.storage.edit(|d| set(d, value));
                this.nav.logging_dirty = this.state.logging.dirty();
                cx.notify();
            },
        );
        self.sync_input(key, &value, window, cx);
        state
    }

    fn save_storage(&mut self, cx: &mut Context<Self>) {
        let draft = self.state.logging.storage.draft().clone();
        let values = history_storage_values(&draft);
        if !values.valid {
            return;
        }
        let location = muxus_core::js::js_trim(&draft.storage_location).to_string();
        let settings = SessionHistorySettings {
            storage_location: (!location.is_empty()).then_some(location),
            max_total_bytes: values.max_total_bytes as u64,
            min_free_bytes: values.min_free_bytes as u64,
            min_free_percent: values.min_free_percent,
            max_age_days: values.max_age_days.map(|days| days as u32),
        };
        let backend = self.backend.clone();
        self.state.logging.saving_storage = true;
        self.fetch(
            cx,
            async move { backend.put_session_history_storage(settings).await },
            |this, result, cx| {
                this.state.logging.saving_storage = false;
                match result {
                    Ok(status) => {
                        let restart = status.restart_required;
                        this.state.logging.storage.saved();
                        this.data.history_storage = Some(status);
                        this.logging_loaded(cx);
                        this.show_toast(
                            Status::Success,
                            if restart {
                                "History limits saved. Restart Muxus to use the new location."
                            } else {
                                "History storage limits saved."
                            },
                            cx,
                        );
                    }
                    Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
                }
            },
        );
    }
}

/// `SaveBar`: the foot of a form that commits explicitly.
fn save_bar(
    theme: &Theme,
    id: SharedString,
    dirty: bool,
    disabled: bool,
    text: &'static str,
    on_save: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let warning = theme.status_text_color(Status::Warning);
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(16.))
        .py(px(10.))
        .border_t_1()
        .border_color(theme.palette.divider)
        .child(div().flex_1().child(if dirty {
            label("Unsaved changes")
                .size(CAPTION)
                .weight(550.)
                .line_height(snap(CAPTION * 1.66))
                .color(warning)
                .into_any_element()
        } else {
            div().into_any_element()
        }))
        .child(
            Button::new(id, text)
                .contained()
                .disabled(disabled)
                .on_click(on_save),
        )
        .into_any_element()
}
