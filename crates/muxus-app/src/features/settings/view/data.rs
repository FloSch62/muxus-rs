//! `components/DataTransferSection.tsx`: back up and restore the setup,
//! export an OpenSSH config, import sessions from other clients. The
//! transfer logic is in [`data_transfer`](super::super::data_transfer).

use std::rc::Rc;

use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, Context, ExternalPaths, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::{Button, Checkbox, Chip};

use super::super::data_transfer::{
    self, DataSummary, PendingFile, RestoreSelection, TransferConflictStrategy, section as transfer,
};
use super::super::hooks::{SessionImportSource, settings_hooks};
use super::kit::{BODY2, CAPTION, DialogParts, dialog, group, page, row, secondary_text, snap};
use super::state::SectionDialog;
use super::window::SettingsWindow;

/// What the section is busy with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Backup,
    OpenSsh,
}

#[derive(Default)]
pub struct DataState {
    pub summary: Option<DataSummary>,
    pub summary_loading: bool,
    pub last_backup: Option<String>,
    pub busy: Option<Busy>,
    pub drag_active: bool,
    loaded: bool,
}

/// `RestoreReviewDialog`'s state.
pub struct RestoreReview {
    pub pending: PendingFile,
    pub selection: RestoreSelection,
    pub conflicts: TransferConflictStrategy,
    pub restoring: bool,
    pub error: Option<String>,
}

impl SettingsWindow {
    fn load_data_section(&mut self, cx: &mut Context<Self>) {
        self.state.data.loaded = true;
        self.state.data.summary_loading = true;
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move {
                let summary = data_transfer::fetch_data_summary(&backend).await.ok();
                let last = transfer::read_last_backup(&backend).await;
                Ok((summary, last))
            },
            |this, result, _| {
                this.state.data.summary_loading = false;
                if let Ok((summary, last)) = result {
                    this.state.data.summary = summary;
                    this.state.data.last_backup = last;
                }
            },
        );
    }

    pub fn data_section(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.state.data.loaded {
            self.load_data_section(cx);
        }
        let theme = *cx.theme();
        let p = theme.palette;
        let busy = self.state.data.busy;
        let stat = transfer::summary_line(
            self.state.data.summary.as_ref(),
            self.state.data.summary_loading,
        );
        let backup_description = div()
            .flex()
            .flex_col()
            .child(
                // HTML collapses the summary's double spaces.
                paragraph(format!(
                    "{}. One portable file, ready for another Muxus install.",
                    stat.split_whitespace().collect::<Vec<_>>().join(" ")
                ))
                .size(CAPTION)
                .line_height(snap(CAPTION * 1.5))
                .color(p.text.secondary),
            )
            .child(
                paragraph(transfer::last_backup_line(
                    self.state.data.last_backup.as_deref(),
                ))
                .size(CAPTION)
                .line_height(snap(CAPTION * 1.5))
                .color(p.text.secondary),
            );
        let backup = row(&theme, "Back up your setup")
            .description(backup_description)
            .control(
                Button::new(
                    "data-create-backup",
                    if busy == Some(Busy::Backup) {
                        "Creating\u{2026}"
                    } else {
                        "Create backup"
                    },
                )
                .contained()
                .start_icon("DownloadOutlined")
                .disabled(busy.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.create_backup(cx))),
            )
            .render();
        let drag_active = self.state.data.drag_active;
        let primary = p.primary.main;
        let restore = div()
            .id("data-restore-drop")
            .flex()
            .flex_col()
            .when(drag_active, |d| {
                d.border_2()
                    .border_dashed()
                    .border_color(primary)
                    .bg(alpha(primary, 0.07))
            })
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style.border_2().border_color(primary).bg(alpha(primary, 0.07))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.state.data.drag_active = false;
                if let Some(path) = paths.paths().first() {
                    this.read_backup_file(path, cx);
                }
            }))
            .child(
                row(&theme, "Restore a backup")
                    .description_text(
                        &theme,
                        "Review what is inside, then restore everything or only the categories you need. You can also drop a backup file here.",
                    )
                    .control(
                        Button::new("data-choose-backup", "Choose backup")
                            .outlined()
                            .start_icon("UploadFileOutlined")
                            .disabled(busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pick_file(cx, |this, path, cx| {
                                    this.read_backup_file(&path, cx)
                                })
                            })),
                    )
                    .render(),
            );
        let export = row(&theme, "OpenSSH config")
            .description_text(
                &theme,
                "SSH hosts as a standard ssh_config for other SSH clients. Muxus-specific settings stay in the backup.",
            )
            .control(
                Button::new(
                    "data-export-openssh",
                    if busy == Some(Busy::OpenSsh) {
                        "Exporting\u{2026}"
                    } else {
                        "Export OpenSSH"
                    },
                )
                .outlined()
                .start_icon("DnsOutlined")
                .disabled(busy.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.export_openssh(cx))),
            )
            .render();
        let import = |source: SessionImportSource, id: &'static str, icon_name: &'static str| {
            Button::new(id, "Import sessions")
                .outlined()
                .start_icon(icon_name)
                .on_click(cx.listener(move |this, _, window, cx| {
                    match settings_hooks(cx).import_sessions {
                        Some(import) => import(source, window, cx),
                        None => this.show_toast(
                            Status::Info,
                            "Importing sessions is not available yet.",
                            cx,
                        ),
                    }
                }))
        };
        let note = div()
            .flex()
            .items_start()
            .gap(px(8.))
            .mt(px(-12.))
            .child(div().mt(px(2.)).child(icon("LockOutlined", 15., p.text.secondary)))
            .child(
                div().flex_1().child(
                    paragraph(
                        "Backups never include private key files, passwords or recorded session history. Key file paths are kept so profiles still know where to look.",
                    )
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.66))
                    .color(p.text.secondary),
                ),
            );
        page(
            &theme,
            "Backup & data",
            Some(secondary_text(
                &theme,
                "Keep one portable copy of your Muxus setup and restore everything or only the parts you need, or bring sessions over from another client.",
            )),
            vec![
                group("Backup & restore")
                    .row(backup)
                    .row(restore)
                    .render(&theme),
                group("Export").row(export).render(&theme),
                group("Import from other clients")
                    .row(
                        row(&theme, "MobaXterm")
                            .description_text(
                                &theme,
                                "Review and import sessions from a local Windows installation or a MobaXterm session file.",
                            )
                            .control(import(
                                SessionImportSource::MobaXterm,
                                "data-import-mobaxterm",
                                "LaptopWindowsOutlined",
                            ))
                            .render(),
                    )
                    .row(
                        row(&theme, "SecureCRT")
                            .description_text(
                                &theme,
                                "Review and import SSH and serial sessions from a SecureCRT XML settings export.",
                            )
                            .control(import(
                                SessionImportSource::SecureCrt,
                                "data-import-securecrt",
                                "TerminalOutlined",
                            ))
                            .render(),
                    )
                    .render(&theme),
                note.into_any_element(),
            ],
        )
        .into_any_element()
    }

    fn create_backup(&mut self, cx: &mut Context<Self>) {
        self.state.data.busy = Some(Busy::Backup);
        let backend = self.backend.clone();
        let version = self.data.app_info.as_ref().map(|info| info.version.clone());
        self.fetch(
            cx,
            async move {
                data_transfer::create_backup_document(&backend, version.as_deref())
                    .await
                    .map_err(|error| muxus_session::ApiError::internal(error.to_string()))
            },
            |this, result, cx| {
                this.state.data.busy = None;
                match result {
                    Ok(document) => {
                        let created_at = document.created_at().to_string();
                        this.save_text_file_then(
                            transfer::dated_transfer_filename(),
                            document.to_file_text(),
                            cx,
                            move |this, cx| {
                                let backend = this.backend.clone();
                                let at = created_at.clone();
                                this.fetch(
                                    cx,
                                    async move {
                                        transfer::remember_backup(&backend, &at).await.map_err(
                                            |error| {
                                                muxus_session::ApiError::internal(error.to_string())
                                            },
                                        )
                                    },
                                    move |this, _, _| {
                                        this.state.data.last_backup = Some(created_at.clone());
                                    },
                                );
                                this.show_toast(Status::Success, transfer::BACKUP_SAVED, cx);
                            },
                        );
                    }
                    Err(error) => {
                        let message = if error.message.is_empty() {
                            transfer::BACKUP_FAILED.to_string()
                        } else {
                            error.message
                        };
                        this.show_toast(Status::Error, message, cx)
                    }
                }
            },
        );
    }

    fn export_openssh(&mut self, cx: &mut Context<Self>) {
        self.state.data.busy = Some(Busy::OpenSsh);
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move {
                data_transfer::create_openssh_export(&backend)
                    .await
                    .map_err(|error| muxus_session::ApiError::internal(error.to_string()))
            },
            |this, result, cx| {
                this.state.data.busy = None;
                match result {
                    Ok(text) => this.save_text_file_then(
                        transfer::openssh_export_filename(),
                        text,
                        cx,
                        |this, cx| {
                            this.show_toast(Status::Success, transfer::OPENSSH_EXPORT_SAVED, cx)
                        },
                    ),
                    Err(error) => {
                        let message = if error.message.is_empty() {
                            transfer::OPENSSH_EXPORT_FAILED.to_string()
                        } else {
                            error.message
                        };
                        this.show_toast(Status::Error, message, cx)
                    }
                }
            },
        );
    }

    fn read_backup_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        match transfer::load_transfer_file(path) {
            Ok(pending) => {
                self.state.dialog = Some(SectionDialog::Restore(RestoreReview {
                    pending,
                    selection: RestoreSelection::default(),
                    conflicts: TransferConflictStrategy::default(),
                    restoring: false,
                    error: None,
                }));
                cx.notify();
            }
            Err(error) => self.show_toast(Status::Error, error.to_string(), cx),
        }
    }

    fn review(&mut self) -> Option<&mut RestoreReview> {
        match &mut self.state.dialog {
            Some(SectionDialog::Restore(review)) => Some(review),
            _ => None,
        }
    }

    fn restore_backup(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.review() else {
            return;
        };
        review.restoring = true;
        review.error = None;
        let document = review.pending.document.clone();
        let (selection, conflicts) = (review.selection, review.conflicts);
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move {
                Ok(data_transfer::restore_transfer_document(
                    &backend, &document, selection, conflicts,
                )
                .await)
            },
            move |this, result, cx| {
                let Ok(result) = result else {
                    return;
                };
                match result {
                    Ok(result) => {
                        this.state.dialog = None;
                        // What the restore wrote: preferences, policies, the summary.
                        if selection.preferences {
                            this.prefs.update(cx, |model, cx| model.reload(cx));
                        }
                        this.state.data.loaded = false;
                        this.data.logging_default = None;
                        this.load_logging(cx);
                        this.show_toast(
                            Status::Success,
                            transfer::restore_complete_message(&result),
                            cx,
                        );
                    }
                    Err(error) => {
                        if let Some(review) = this.review() {
                            review.restoring = false;
                            review.error = Some(transfer::restore_failed_message(&error.message));
                        }
                    }
                }
            },
        );
    }

    /// `RestoreReviewDialog`.
    pub fn render_restore_review(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(SectionDialog::Restore(review)) = &self.state.dialog else {
            return None;
        };
        let p = theme.palette;
        let rows = transfer::restore_categories(&review.pending.document);
        let selection = review.selection;
        let conflicts = review.conflicts;
        let restoring = review.restoring;
        let subtitle = transfer::review_subtitle(&review.pending);
        let proxy = transfer::proxy_command_count(&review.pending.document);
        let error = review.error.clone();

        let tile = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(34.))
            .rounded(px(8.))
            .bg(alpha(p.success.main, 0.12))
            .child(icon("CloudDoneOutlined", SMALL, p.success.main));
        let title = div().flex().items_center().gap(px(12.)).child(tile).child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    label("Restore this backup?")
                        .size(theme.typography.h6.size)
                        .weight(700.)
                        .line_height(snap(theme.typography.h6.line_height))
                        .color(p.text.primary),
                )
                .child(
                    label(subtitle)
                        .size(CAPTION)
                        .line_height(snap(CAPTION * 1.66))
                        .color(p.text.secondary),
                ),
        );
        let mut list = div().flex().flex_col();
        for category_row in rows {
            let category = category_row.category;
            let checked = selection.get(category);
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .py(px(4.))
                    .child(
                        div().ml(px(-12.)).child(
                            Checkbox::new(
                                SharedString::from(format!("restore-{category:?}")),
                                checked,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(review) = this.review() {
                                        review.selection.set(category, !checked);
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                    )
                    .child(div().min_w(px(36.)).child(icon(
                        category_row.icon,
                        SMALL,
                        p.action.active,
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .my(px(6.))
                            .child(
                                label(category_row.label)
                                    .size(BODY2)
                                    .line_height(snap(BODY2 * 1.43))
                                    .color(p.text.primary),
                            )
                            .child(
                                label(category_row.detail)
                                    .size(BODY2)
                                    .line_height(snap(BODY2 * 1.43))
                                    .color(p.text.secondary),
                            ),
                    )
                    .when_some(category_row.count, |row, count| {
                        row.child(
                            div()
                                .min_w(px(36.))
                                .flex()
                                .justify_center()
                                .child(Chip::new(count.to_string()).outlined()),
                        )
                    }),
            );
        }
        let radio = |value: TransferConflictStrategy, title: &'static str, detail: &'static str| {
            div()
                .flex()
                .items_center()
                .child(
                    div().ml(px(-11.)).child(
                        Checkbox::radio(
                            SharedString::from(format!("restore-conflicts-{value:?}")),
                            conflicts == value,
                        )
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(review) = this.review() {
                                review.conflicts = value;
                            }
                            cx.notify();
                        })),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            label(title)
                                .size(BODY2)
                                .line_height(snap(BODY2 * 1.43))
                                .color(p.text.primary),
                        )
                        .child(
                            label(detail)
                                .size(CAPTION)
                                .line_height(snap(super::kit::BODY1 * 1.5))
                                .color(p.text.secondary),
                        ),
                )
        };
        let mut conflict_form = div()
            .flex()
            .flex_col()
            .py(px(16.))
            .child(
                label("When an item already exists")
                    .size(theme.typography.subtitle2.size)
                    .weight(700.)
                    .line_height(snap(theme.typography.subtitle2.line_height))
                    .color(p.text.primary),
            )
            .child(radio(
                TransferConflictStrategy::Keep,
                "Keep the current item",
                "Safest. Only missing items are added.",
            ))
            .child(radio(
                TransferConflictStrategy::Replace,
                "Replace it with the file\u{2019}s version",
                "Matches by SSH alias or Muxus item ID.",
            ));
        if selection.preferences || selection.logging {
            conflict_form = conflict_form.child(
                div().mt(px(4.)).child(
                    paragraph(
                        "App and logging settings are replaced when selected; the choice above applies to saved connections and tunnels.",
                    )
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.66))
                    .color(p.text.secondary),
                ),
            );
        }
        let mut content = div()
            .flex()
            .flex_col()
            .child(list)
            .child(div().h(px(1.)).bg(p.divider))
            .child(conflict_form)
            .child(
                div().mb(px(16.)).child(
                    muxus_ui::widgets::Alert::new(
                        Status::Info,
                        "This is a merge: items that are not in the file are never deleted. Active terminal sessions are not interrupted.",
                    ),
                ),
            );
        if selection.connections && proxy > 0 {
            content = content.child(div().mb(px(16.)).child(muxus_ui::widgets::Alert::new(
                Status::Warning,
                transfer::proxy_command_warning(proxy),
            )));
        }
        if let Some(error) = error {
            content = content.child(
                div()
                    .mb(px(16.))
                    .child(muxus_ui::widgets::Alert::new(Status::Error, error)),
            );
        }
        let close = cx.entity().downgrade();
        let parts = DialogParts {
            max_width: 600.,
            title: title.into_any_element(),
            content: content.py(px(0.)).into_any_element(),
            dividers: true,
            actions: vec![
                Button::new("restore-cancel", "Cancel")
                    .disabled(restoring)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.dialog = None;
                        cx.notify();
                    }))
                    .into_any_element(),
                Button::new(
                    "restore-commit",
                    if restoring {
                        "Restoring\u{2026}"
                    } else {
                        "Restore selected"
                    },
                )
                .contained()
                .start_icon("CloudDoneOutlined")
                .disabled(restoring || selection.selected_count() == 0)
                .on_click(cx.listener(|this, _, _, cx| this.restore_backup(cx)))
                .into_any_element(),
            ],
            actions_padding: (20., 12.),
        };
        Some(dialog(
            theme,
            parts,
            (!restoring).then(|| -> super::kit::Handler {
                Rc::new(move |_, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.state.dialog = None;
                        cx.notify();
                    });
                })
            }),
        ))
    }
}
