//! "Organize host" (`HostOrganizationDialog.tsx`): display name, folder and
//! color, without touching the connection.

use std::rc::Rc;

use muxus_core::api::OpenSshMetadataPatch;
use muxus_session::Backend;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Task, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::dialog::modal;

use crate::features::host_editor::logic::native_draft::{optional_patch, text_patch};
use crate::features::host_editor::services::{self, EditorData};
use crate::features::host_editor::view::form::{self, Form};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_XS, form_dialog_frame};
use crate::features::host_editor::widgets::visuals::host_color_picker;
use crate::hosts::managed::{ManagedHost, managed_host_address, managed_host_display_name};
use crate::hosts::tree::folder_segments;
use crate::sidebar::kind_icon::host_kind_icon_name;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{body2, caption, para, text};

#[derive(Debug, Clone, PartialEq)]
pub enum OrganizeEvent {
    Close,
    Saved,
    Toast { status: Status, message: String },
}

pub struct OrganizeDialog {
    backend: Backend,
    host: ManagedHost,
    display_name: String,
    group: String,
    color: Option<String>,
    data: EditorData,
    busy: bool,
    inputs: InputCache,
    menus: Menus,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<OrganizeEvent> for OrganizeDialog {}

impl Focusable for OrganizeDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for OrganizeDialog {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open "Organize & color…" for a host.
pub fn open_organize(
    window: &mut Window,
    cx: &mut App,
    backend: Backend,
    host: ManagedHost,
) -> Entity<OrganizeDialog> {
    cx.new(|cx: &mut Context<OrganizeDialog>| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let metadata = host.metadata();
        let mut dialog = OrganizeDialog {
            backend: backend.clone(),
            display_name: metadata
                .and_then(|m| m.display_name.clone())
                .unwrap_or_default(),
            group: metadata.and_then(|m| m.group.clone()).unwrap_or_default(),
            color: metadata.and_then(|m| m.color.clone()),
            host,
            data: EditorData::default(),
            busy: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            focus,
            _tasks: Vec::new(),
        };
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let _ = this.update(cx, |this, cx| {
                this.data = data;
                cx.notify();
            });
        });
        dialog._tasks.push(task);
        dialog
    })
}

impl OrganizeDialog {
    fn fallback_name(&self) -> String {
        match &self.host {
            ManagedHost::Ssh(entry) => entry.alias.clone(),
            ManagedHost::Profile(entry) => entry.name.clone(),
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let patch = OpenSshMetadataPatch {
            display_name: text_patch(&self.display_name),
            group: text_patch(&self.group),
            color: optional_patch(&self.color),
            ..OpenSshMetadataPatch::default()
        };
        let backend = self.backend.clone();
        let host = self.host.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = services::patch_metadata(&backend, &host, patch).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(OrganizeEvent::Saved),
                    Err(message) => cx.emit(OrganizeEvent::Toast {
                        status: Status::Error,
                        message,
                    }),
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if self.menus.open.is_some() {
                    self.menus.close();
                    cx.notify();
                } else {
                    cx.emit(OrganizeEvent::Close);
                }
            }
            // The dialog is a form: Enter submits it.
            "enter" if self.menus.open.is_none() => {
                cx.stop_propagation();
                self.save(cx);
            }
            _ => {}
        }
    }
}

impl Render for OrganizeDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let fallback = self.fallback_name();
        let name_input = form::text_input(
            self,
            "organize-name",
            window,
            cx,
            self.display_name.clone(),
            |this, v, cx| {
                this.display_name = v;
                cx.notify();
                None
            },
        );
        let known = self.data.known_folders();
        let group = self.group.clone();
        let folder = form::folder_field(
            self,
            "organize-group",
            "Folder",
            &group,
            known,
            Some("Choose a folder or type a new one — use / to nest.".into()),
            None,
            false,
            window,
            cx,
            |this, group, cx| {
                this.group = group;
                cx.notify();
            },
        );
        let entity = cx.entity().downgrade();
        let picker = host_color_picker(
            &theme,
            "organize-color",
            "Color",
            self.color.as_deref(),
            false,
            30.,
            Rc::new(move |color, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.color = color;
                    cx.notify();
                });
            }),
        );
        let name = if self.display_name.trim().is_empty() {
            fallback.clone()
        } else {
            self.display_name.trim().to_owned()
        };
        let segments = folder_segments(Some(&self.group));
        let detail = format!(
            "{}{}",
            if segments.is_empty() {
                String::new()
            } else {
                format!("{} · ", segments.join(" / "))
            },
            managed_host_address(&self.host)
        );
        let accent = self.color.as_deref().and_then(try_hex);
        let preview = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(1.25))
            .p(Theme::spacing(1.25))
            .border_1()
            .border_l_4()
            .border_color(p.divider)
            .rounded(px(theme.radius * 1.5))
            .bg(p.sidebar)
            .when_accent(accent, theme.radius * 1.5)
            .child(icon(
                host_kind_icon_name(self.host.kind()),
                muxus_ui::icon::MEDIUM,
                accent.unwrap_or(p.text.secondary),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(text(name, body2().weight(weight::SEMIBOLD), p.text.primary))
                    .child(text(detail, caption(), p.text.secondary)),
            );
        let content = div()
            .flex()
            .flex_col()
            .child(div().mb(Theme::spacing(2.5)).child(para(
                "Display name, folder, and color are local to Muxus. Connection settings stay unchanged.",
                body2(),
                p.text.secondary,
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::spacing(2.25))
                    .child(
                        FormField::input("organize-name-field", &name_input)
                            .label("Display name")
                            .placeholder(fallback)
                            .helper("Optional — only changes how this host appears in Muxus."),
                    )
                    .child(folder)
                    .child(picker)
                    .child(preview),
            );
        let entity = cx.entity().downgrade();
        let (cancel, save, close) = (entity.clone(), entity.clone(), entity);
        let actions = vec![
            Button::new("organize-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(OrganizeEvent::Close));
                })
                .into_any_element(),
            Button::new("organize-save", "Save")
                .contained()
                .disabled(self.busy)
                .on_click(move |_, _, cx| {
                    let _ = save.update(cx, |this, cx| this.save(cx));
                })
                .into_any_element(),
        ];
        let title = SharedString::from(format!(
            "Organize {}",
            managed_host_display_name(&self.host)
        ));
        div()
            .id("organize-host")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                form_dialog_frame(
                    &theme,
                    window,
                    "organize-content",
                    DIALOG_XS,
                    title,
                    6.,
                    content.into_any_element(),
                    actions,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(OrganizeEvent::Close));
                })),
            ))
    }
}

trait Accent: Sized {
    fn when_accent(self, color: Option<muxus_ui::gpui_kit::Hsla>, radius: f32) -> Self;
}

impl Accent for muxus_ui::gpui_kit::Div {
    fn when_accent(self, color: Option<muxus_ui::gpui_kit::Hsla>, radius: f32) -> Self {
        match color {
            // Only the left edge takes the host color; GPUI borders share one
            // color, so the stripe is drawn over the left border.
            Some(color) => self.relative().child(
                div()
                    .absolute()
                    .left(px(-4.))
                    .top(px(-1.))
                    .bottom(px(-1.))
                    .w(px(4.))
                    .rounded_l(px(radius))
                    .bg(color),
            ),
            None => self,
        }
    }
}
