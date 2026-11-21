//! `ShareFolderDialog.tsx`: write a sidebar folder as a session list that others follow.
//! Muxus only writes the file: where it is published is up to the user, so the dialog says
//! what to do with it.

use std::path::PathBuf;
use std::rc::Rc;

use muxus_import::launch::ManagedHost;
use muxus_import::{ShareFolderOptions, SharedFolder, share_folder, shared_sessions_filename};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled,
    Task, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::typography::{VariantExt, body2, para, subtitle2, text};
use muxus_ui::widgets::{Alert, Button, Checkbox};

use super::logic::{SHARE_STEPS, share_summary};
use crate::features::host_editor::services::{self, EditorData};
use crate::features::host_editor::widgets::shell::{DIALOG_SM, dialog_frame};
use crate::features::launcher::kit::show_error_toast;
use crate::hosts::tree::folder_label;

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareFolderEvent {
    Close,
}

pub struct ShareFolderDialog {
    /// The folder being shared, as the sidebar has it.
    path: String,
    include_users: bool,
    include_key_paths: bool,
    /// The file the list was last saved as.
    saved: Option<String>,
    /// The hosts and the folders' shared credentials, once read.
    data: EditorData,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ShareFolderEvent> for ShareFolderDialog {}

impl Focusable for ShareFolderDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the dialog for the sidebar folder at `path`.
pub fn open_share_folder(
    backend: Backend,
    path: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ShareFolderDialog> {
    cx.new(|cx: &mut Context<ShareFolderDialog>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let _ = this.update(cx, |this, cx| {
                this.data = data;
                cx.notify();
            });
        });
        ShareFolderDialog {
            path,
            include_users: true,
            include_key_paths: false,
            saved: None,
            data: EditorData::default(),
            focus,
            _tasks: vec![task],
        }
    })
}

fn default_directory() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

impl ShareFolderDialog {
    /// The list as it would be written now. Every host from both sources is looked at,
    /// unfiltered: the hosts of the folder, and whatever they connect through.
    pub fn shared(&self) -> SharedFolder {
        let ssh = self
            .data
            .ssh_config
            .as_ref()
            .map(|config| config.hosts.as_slice())
            .unwrap_or_default();
        let hosts: Vec<ManagedHost<'_>> = ssh
            .iter()
            .map(ManagedHost::Ssh)
            .chain(self.data.saved_hosts.iter().map(ManagedHost::Profile))
            .collect();
        share_folder(
            &self.path,
            &hosts,
            &self.data.folders,
            ShareFolderOptions {
                include_users: self.include_users,
                include_key_paths: self.include_key_paths,
            },
            &muxus_core::js::now_iso(),
        )
    }

    pub fn filename(&self) -> String {
        shared_sessions_filename(&self.path)
    }

    /// The text of the file: the list, indented, with a final line break.
    pub fn file_text(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.shared().document).unwrap_or_default();
        text.push('\n');
        text
    }

    pub fn set_include_users(&mut self, include: bool, cx: &mut Context<Self>) {
        self.include_users = include;
        cx.notify();
    }

    pub fn set_include_key_paths(&mut self, include: bool, cx: &mut Context<Self>) {
        self.include_key_paths = include;
        cx.notify();
    }

    /// The file the list was last saved as.
    pub fn saved(&self) -> Option<&str> {
        self.saved.as_deref()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (name, text) = (self.filename(), self.file_text());
        let answer = crate::util::file_dialog::ask_save_path(&default_directory(), &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = answer.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            let written = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| {
                match written {
                    Ok(()) => {
                        this.saved = Some(
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or(name),
                        );
                    }
                    Err(error) => show_error_toast(error.to_string(), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(ShareFolderEvent::Close);
        }
    }

    fn option(
        id: &'static str,
        label: &'static str,
        checked: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        set: fn(&mut Self, bool, &mut Context<Self>),
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        div()
            .ml(px(-11.))
            .flex()
            .items_center()
            .child(
                Checkbox::new(id, checked)
                    .small()
                    .on_click(move |_, _, cx| {
                        let _ = entity.update(cx, |this, cx| set(this, !checked, cx));
                    }),
            )
            .child(text(label, body2(), theme.palette.text.primary))
            .into_any_element()
    }
}

impl Render for ShareFolderDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let shared = self.shared();
        let mut steps = div().flex().flex_col().mt(Theme::spacing(0.5));
        for (index, step) in SHARE_STEPS.iter().enumerate() {
            steps = steps.child(
                div()
                    .flex()
                    .items_start()
                    .gap(Theme::spacing(1.))
                    .mt(Theme::spacing(0.5))
                    .child(div().flex_none().w(px(16.)).child(text(
                        format!("{}.", index + 1),
                        body2(),
                        p.text.secondary,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(para(*step, body2(), p.text.secondary)),
                    ),
            );
        }
        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .pt(Theme::spacing(0.5))
            .child(para(
                share_summary(shared.host_count, shared.jump_host_count),
                body2(),
                p.text.secondary,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(Self::option(
                        "share-folder-users",
                        "Include user names",
                        self.include_users,
                        &theme,
                        cx,
                        Self::set_include_users,
                    ))
                    .child(Self::option(
                        "share-folder-key-paths",
                        "Include key file paths",
                        self.include_key_paths,
                        &theme,
                        cx,
                        Self::set_include_key_paths,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(text("Next", subtitle2().weight(650.), p.text.primary))
                    .child(steps),
            );
        if let Some(saved) = &self.saved {
            stack = stack.child(Alert::new(Status::Success, format!("Saved {saved}.")).outlined());
        }

        let saved = self.saved.is_some();
        let (cancel, save, close) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let mut save_button = Button::new(
            "share-folder-save",
            if saved { "Save again" } else { "Save list…" },
        )
        .start_icon("SaveAltOutlined")
        .disabled(shared.host_count == 0)
        .on_click(move |_, _, cx| {
            let _ = save.update(cx, |this, cx| this.save(cx));
        });
        save_button = if saved {
            save_button.outlined()
        } else {
            save_button.contained()
        };
        let actions = vec![
            Button::new("share-folder-cancel", if saved { "Done" } else { "Cancel" })
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(ShareFolderEvent::Close));
                })
                .into_any_element(),
            save_button.into_any_element(),
        ];
        let title = SharedString::from(format!("Share “{}”", folder_label(&self.path)));
        div()
            .id("share-folder-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "share-folder-backdrop",
                dialog_frame(
                    &theme,
                    window,
                    "share-folder-content",
                    DIALOG_SM,
                    title,
                    6.,
                    stack.into_any_element(),
                    actions,
                    true,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(ShareFolderEvent::Close));
                })),
            ))
    }
}
