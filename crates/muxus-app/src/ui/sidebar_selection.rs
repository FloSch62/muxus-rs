//! `components/sidebar/SelectionBar.tsx`: pinned under the host tree while hosts
//! are selected: how many, the actions that apply to all of them, and the way
//! out. One host opens its own editor; several open the bulk editor.

use muxus_ui::gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement, Styled, Window, div, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::{Button, IconButton};

use crate::ui::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};

impl SessionSidebar {
    /// The bar, while the selection resolves to at least one host.
    pub(crate) fn selection_bar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let hosts = self.selected_hosts();
        if hosts.is_empty() {
            return None;
        }
        let p = theme.palette;
        let count = hosts.len();
        fn action(
            action: SidebarAction,
            cx: &mut Context<SessionSidebar>,
        ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
            cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(SidebarEvent::Action(action.clone()))
            })
        }
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .px(px(10.))
                .py(px(6.))
                .border_t_1()
                .border_color(p.divider)
                .child(
                    div().flex_1().min_w_0().child(
                        label(format!("{count} selected"))
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.primary),
                    ),
                )
                .child(
                    Button::new("selection-edit", "Edit…")
                        .contained()
                        .start_icon("EditOutlined")
                        .on_click(action(SidebarAction::EditHosts(hosts.clone()), cx)),
                )
                .child(
                    IconButton::new("selection-move", "DriveFileMoveOutlined")
                        .tooltip("Move to folder…")
                        .on_click(action(SidebarAction::MoveHosts(hosts.clone()), cx)),
                )
                .child(
                    IconButton::new("selection-delete", "DeleteOutlined")
                        .tooltip("Delete")
                        .on_click(action(SidebarAction::DeleteHosts(hosts), cx)),
                )
                .child(
                    IconButton::new("selection-clear", "Close")
                        .tooltip("Clear selection (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| this.clear_selection(cx))),
                )
                .into_any_element(),
        )
    }

    /// Let go of every selected host.
    pub(crate) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selection.is_empty() || self.anchor.is_some() {
            self.selection.clear();
            self.anchor = None;
            cx.notify();
        }
    }
}
