//! `SharedSessionsDialog.tsx`: follow a new shared sessions link, or change one.

use std::rc::Rc;

use muxus_core::api::{SharedSessionSource, SharedSessionSourceInput};
use muxus_core::js::js_trim;
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render,
    SharedString, Styled, Task, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::typography::{caption, para};
use muxus_ui::widgets::{Alert, Button, IconButton};

use super::logic::{
    DEFAULT_REFRESH_MINUTES, FOLDER_HELPER, FOLLOW_NOTE, LINK_HELPER, LINK_PLACEHOLDER, link_ready,
    refresh_choices,
};
use super::store;
use crate::features::host_editor::services;
use crate::features::host_editor::view::form::{self, Choice, Form};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_SM, form_dialog_frame};

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum SharedSessionsDialogEvent {
    Close,
    /// The link is stored; whoever opened the dialog reads it now.
    Saved(Box<SharedSessionSource>),
}

const LINK: &str = "shared-sessions-link";
const NAME: &str = "shared-sessions-name";
const FOLDER: &str = "shared-sessions-folder";
const REFRESH: &str = "shared-sessions-refresh";

pub struct SharedSessionsDialog {
    backend: Backend,
    /// The link being changed; `None` follows a new one.
    source: Option<SharedSessionSource>,
    url: String,
    name: String,
    /// Follows the name until the user picks a folder of their own.
    folder: Option<String>,
    refresh_minutes: u32,
    /// Every folder of the sidebar, offered for the link's hosts.
    known_folders: Vec<String>,
    error: Option<String>,
    busy: bool,
    /// The link field still has to take the focus.
    focus_link: bool,
    inputs: InputCache,
    menus: Menus,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<SharedSessionsDialogEvent> for SharedSessionsDialog {}

impl Focusable for SharedSessionsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for SharedSessionsDialog {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open the dialog for `source`, or to follow a new link.
pub fn open_shared_sessions_dialog(
    backend: Backend,
    source: Option<SharedSessionSource>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SharedSessionsDialog> {
    cx.new(|cx: &mut Context<SharedSessionsDialog>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let folders = backend.clone();
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&folders).await;
            let _ = this.update(cx, |this, cx| {
                this.known_folders = data.known_folders();
                cx.notify();
            });
        });
        SharedSessionsDialog {
            backend,
            url: source
                .as_ref()
                .map(|source| source.url.clone())
                .unwrap_or_default(),
            name: source
                .as_ref()
                .map(|source| source.name.clone())
                .unwrap_or_default(),
            folder: source.as_ref().map(|source| source.folder.clone()),
            refresh_minutes: source
                .as_ref()
                .map_or(DEFAULT_REFRESH_MINUTES, |source| source.refresh_minutes),
            focus_link: source.is_none(),
            source,
            known_folders: Vec::new(),
            error: None,
            busy: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            focus,
            _tasks: vec![task],
        }
    })
}

impl SharedSessionsDialog {
    /// The folder the hosts go to: the one chosen, or one named like the link.
    pub fn effective_folder(&self) -> String {
        match &self.folder {
            Some(folder) => folder.clone(),
            None => js_trim(&self.name).to_owned(),
        }
    }

    /// Whether the folder is still the one named like the link.
    pub fn follows_name(&self) -> bool {
        self.folder.is_none()
    }

    pub fn ready(&self) -> bool {
        link_ready(&self.url, &self.name, &self.effective_folder())
    }

    /// What the dialog would save, as it stands.
    pub fn input(&self) -> SharedSessionSourceInput {
        SharedSessionSourceInput {
            id: self.source.as_ref().map(|source| source.id.clone()),
            name: js_trim(&self.name).to_owned(),
            url: js_trim(&self.url).to_owned(),
            folder: self.effective_folder(),
            refresh_minutes: self.refresh_minutes,
        }
    }

    /// Why the last save was refused.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_link(&mut self, url: impl Into<String>, cx: &mut Context<Self>) {
        self.url = url.into();
        // The field shows it the next time it is drawn.
        self.inputs.remove(LINK);
        cx.notify();
    }

    pub fn set_name(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
        self.name = name.into();
        self.inputs.remove(NAME);
        self.name_changed();
        cx.notify();
    }

    pub fn set_folder(&mut self, folder: impl Into<String>, cx: &mut Context<Self>) {
        self.folder = Some(folder.into());
        self.inputs.remove(FOLDER);
        cx.notify();
    }

    pub fn set_refresh_minutes(&mut self, minutes: u32, cx: &mut Context<Self>) {
        self.refresh_minutes = minutes;
        cx.notify();
    }

    /// The folder field echoes the name while it still follows it.
    fn name_changed(&mut self) {
        if self.folder.is_none() {
            self.inputs.remove(FOLDER);
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(SharedSessionsDialogEvent::Close);
        }
    }

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.ready() {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let input = self.input();
        cx.spawn_in(window, async move |this, cx| {
            let result = backend.save_shared_session_source(input).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(saved) => {
                        store::refresh(&this.backend, cx);
                        cx.emit(SharedSessionsDialogEvent::Saved(Box::new(saved)));
                    }
                    Err(error) => {
                        this.error = Some(if error.message.is_empty() {
                            "Could not save the link.".to_owned()
                        } else {
                            error.message
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let chosen = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Use list".into()),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(path) = chosen.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| this.set_link(path, cx));
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        cx.stop_propagation();
        if self.menus.open.is_some() {
            self.menus.close();
            cx.notify();
        } else {
            self.close(cx);
        }
    }

    fn link_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let url = self.url.clone();
        let input = form::text_input_enter(
            self,
            LINK,
            window,
            cx,
            url,
            |this, value, cx| {
                this.url = value;
                cx.notify();
                None
            },
            |this, window, cx| this.submit(window, cx),
        );
        if std::mem::take(&mut self.focus_link) {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        let field = FormField::input("shared-sessions-link-field", &input)
            .label("Link or file")
            .placeholder(LINK_PLACEHOLDER)
            .helper(LINK_HELPER);
        let entity = cx.entity().downgrade();
        div()
            .flex()
            .items_start()
            .gap(Theme::spacing(1.))
            .child(div().flex_1().min_w_0().child(field))
            .child(
                div().mt(Theme::spacing(0.75)).child(
                    IconButton::new("shared-sessions-browse", "FolderOpenOutlined")
                        .medium()
                        .icon_size(muxus_ui::icon::SMALL)
                        .tooltip("Browse for a file")
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| this.browse(cx));
                        }),
                ),
            )
            .into_any_element()
    }
}

impl Render for SharedSessionsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let editing = self.source.is_some();
        let link = self.link_field(window, cx);
        let (typed_name, folder_value, known) = (
            self.name.clone(),
            self.effective_folder(),
            self.known_folders.clone(),
        );
        let name = form::text_input_enter(
            self,
            NAME,
            window,
            cx,
            typed_name,
            |this, value, cx| {
                this.name = value;
                this.name_changed();
                cx.notify();
                None
            },
            |this, window, cx| this.submit(window, cx),
        );
        let folder = form::folder_field(
            self,
            FOLDER,
            "Folder",
            &folder_value,
            known,
            Some(FOLDER_HELPER.into()),
            None,
            false,
            window,
            cx,
            |this, folder, cx| {
                this.folder = Some(folder);
                cx.notify();
            },
        );
        let minutes = self.refresh_minutes;
        let choices: Vec<Choice<u32>> = refresh_choices(minutes)
            .into_iter()
            .map(|choice| Choice::new(choice.minutes, choice.label))
            .collect();
        let current = choices
            .iter()
            .find(|choice| choice.value == minutes)
            .map(|choice| choice.label.clone())
            .unwrap_or_default();
        let refresh = form::select(
            self,
            REFRESH,
            |body| {
                FormField::new("shared-sessions-refresh-field", body)
                    .label("Check for changes")
                    .shrink(true)
            },
            &minutes,
            choices,
            Some(FieldBody::Value(current)),
            window,
            cx,
            |this, picked, cx| this.set_refresh_minutes(picked, cx),
        );

        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.25))
            .pt(Theme::spacing(1.))
            .child(link)
            .child(
                FormField::input("shared-sessions-name-field", &name)
                    .label("Name")
                    .placeholder("Lab hosts"),
            )
            .child(folder)
            .child(refresh)
            .child(para(FOLLOW_NOTE, caption(), p.text.secondary));
        if let Some(error) = &self.error {
            stack = stack.child(Alert::new(Status::Error, error.clone()).outlined());
        }

        let (cancel, submit, close) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let actions = vec![
            Button::new("shared-sessions-cancel", "Cancel")
                .disabled(self.busy)
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |this, cx| this.close(cx));
                })
                .into_any_element(),
            Button::new(
                "shared-sessions-submit",
                if editing { "Save" } else { "Add" },
            )
            .contained()
            .disabled(self.busy || !self.ready())
            .on_click(move |_, window, cx| {
                let _ = submit.update(cx, |this, cx| this.submit(window, cx));
            })
            .into_any_element(),
        ];
        let title: SharedString = if editing {
            "Edit shared sessions".into()
        } else {
            "Add shared sessions".into()
        };
        div()
            .id("shared-sessions-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "shared-sessions-dialog-backdrop",
                form_dialog_frame(
                    &theme,
                    window,
                    "shared-sessions-dialog-content",
                    DIALOG_SM,
                    title,
                    6.,
                    stack.into_any_element(),
                    actions,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(cx));
                })),
            ))
    }
}
