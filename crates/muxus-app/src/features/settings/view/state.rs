//! What the sections remember while the window is open: drafts of the forms
//! that save explicitly, the entry being edited in list sections, open
//! dialogs and popovers.

use std::time::Duration;

use muxus_ui::gpui_kit::{App, Context};

use super::super::draft::{
    HistoryStorageDraft, LogFileDraft, SessionLoggingDraft, SettingsDraft,
    fallback_session_logging_draft, same_history_storage_draft, same_log_file_draft,
    same_session_logging_draft,
};
use super::super::section::{SettingsNavigation, SettingsSection};
use super::window::SettingsWindow;

/// How long the entry the section was opened for stays outlined.
pub const HIGHLIGHT_MS: u64 = 2500;

/// The four explicitly saved forms of the Session logging section.
pub struct LoggingForms {
    pub default: SettingsDraft<SessionLoggingDraft>,
    pub local: SettingsDraft<SessionLoggingDraft>,
    pub files: SettingsDraft<LogFileDraft>,
    pub storage: SettingsDraft<HistoryStorageDraft>,
    pub saving_default: bool,
    pub saving_local: bool,
    pub saving_files: bool,
    pub saving_storage: bool,
}

impl Default for LoggingForms {
    fn default() -> Self {
        Self {
            default: SettingsDraft::new(
                fallback_session_logging_draft(false, false),
                same_session_logging_draft,
            ),
            local: SettingsDraft::new(
                fallback_session_logging_draft(true, false),
                same_session_logging_draft,
            ),
            files: SettingsDraft::new(
                LogFileDraft {
                    directory: String::new(),
                    filename_pattern:
                        muxus_core::session_log_file::DEFAULT_SESSION_LOG_FILE_PATTERN.into(),
                    timestamps: false,
                },
                same_log_file_draft,
            ),
            storage: SettingsDraft::new(HistoryStorageDraft::default(), same_history_storage_draft),
            saving_default: false,
            saving_local: false,
            saving_files: false,
            saving_storage: false,
        }
    }
}

impl LoggingForms {
    pub fn dirty(&self) -> bool {
        self.default.dirty() || self.local.dirty() || self.files.dirty() || self.storage.dirty()
    }
}

/// A modal opened from a section.
pub enum SectionDialog {
    /// The master-password dialog in one of its modes.
    Master(super::passwords::MasterDialog),
    EditPassword(super::passwords::EditPasswordDialog),
    Secret(super::passwords::SecretDialog),
    Restore(super::data::RestoreReview),
}

#[derive(Default)]
pub struct SectionState {
    pub logging: LoggingForms,
    /// The highlighting profile being edited.
    pub highlight_selected: String,
    /// The custom scheme being edited.
    pub scheme_selected: String,
    /// Rule lists being edited as JSON: editor key → error.
    pub rules_json: std::collections::HashMap<String, Option<String>>,
    /// The rule whose color popover is open.
    pub rule_colors: Option<(String, String)>,
    /// The entry a section was opened for, outlined until the timer runs out.
    pub highlighted: Option<String>,
    highlight_generation: u64,
    pub dialog: Option<SectionDialog>,
    pub color_picker: Option<super::color::ColorPicker>,
    pub installed_fonts: Option<Option<Vec<String>>>,
    pub font_input_focused: bool,
    /// The vault is being deleted.
    pub vault_busy: bool,
    /// Installed WSL distributions (Windows), read when Local shells opens.
    pub wsl: Option<super::window::Load<Vec<muxus_core::api::WslDistribution>>>,
    pub update: super::about::UpdateState,
    pub data: super::data::DataState,
}

impl SectionState {
    /// Bookkeeping before a frame.
    pub fn before_render(&mut self, _section: SettingsSection, _cx: &App) {}

    /// The fonts installed on this machine (`listLocalFontFamilies`), read once.
    pub fn installed_fonts(&mut self, cx: &App) -> Option<Vec<String>> {
        self.installed_fonts
            .get_or_insert_with(|| {
                let mut names = cx.text_system().all_font_names();
                names.sort();
                names.dedup();
                Some(names)
            })
            .clone()
    }

    /// A section was shown, possibly for one entry: outline it for a moment.
    pub fn focus_started(&mut self, nav: &SettingsNavigation, cx: &mut Context<SettingsWindow>) {
        self.highlighted = nav.focus_item.clone();
        self.highlight_generation += 1;
        if self.highlighted.is_none() {
            return;
        }
        let generation = self.highlight_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(HIGHLIGHT_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.state.highlight_generation == generation {
                    this.state.highlighted = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Leaving the logging section with its edits discarded.
    pub fn reset_logging(&mut self) {
        self.logging = LoggingForms::default();
    }

    /// Close the topmost popover or dialog; true when there was one.
    pub fn close_dialog(&mut self, _cx: &mut App) -> bool {
        if self.color_picker.take().is_some() || self.rule_colors.take().is_some() {
            return true;
        }
        self.dialog.take().is_some()
    }
}
