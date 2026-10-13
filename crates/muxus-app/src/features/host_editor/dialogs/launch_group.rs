//! "Launch “Folder”" (`sidebar/LaunchGroupDialog.tsx`): start every host of a
//! folder, file group or subtree in one layout.

use std::rc::Rc;

use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Window, div,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::Alert;
use muxus_ui::widgets::dialog::modal;

use crate::features::host_editor::widgets::shell::{DIALOG_XS, dialog_frame};
use crate::hosts::managed::ManagedHost;
use crate::state::tabs::SessionSetLayout;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::Button;
use muxus_ui::widgets::toggle_button::ToggleButton;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::toggle_ink;
use muxus_ui::widgets::typography::{body2, caption, para, text};

/// The set of hosts a launch targets: one folder, one file group, or a subtree.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchTarget {
    pub label: String,
    pub hosts: Vec<ManagedHost>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LaunchGroupEvent {
    Close,
    /// Start these hosts in this layout, replacing the current pane layout;
    /// show [`launch_toast`] once they start.
    Launch {
        hosts: Vec<ManagedHost>,
        layout: SessionSetLayout,
        label: String,
    },
}

const LAYOUTS: [(SessionSetLayout, &str, &str); 4] = [
    (SessionSetLayout::Tabs, "Tabs", "TabOutlined"),
    (SessionSetLayout::Columns, "Columns", "ViewColumnOutlined"),
    (SessionSetLayout::Rows, "Rows", "TableRowsOutlined"),
    (SessionSetLayout::Grid, "Grid", "GridViewOutlined"),
];

pub fn layout_name(layout: SessionSetLayout) -> &'static str {
    match layout {
        SessionSetLayout::Tabs => "tabs",
        SessionSetLayout::Columns => "columns",
        SessionSetLayout::Rows => "rows",
        SessionSetLayout::Grid => "grid",
    }
}

/// The success toast after a launch started `count` sessions.
pub fn launch_toast(count: usize, label: &str, layout: SessionSetLayout) -> String {
    format!(
        "Launching {count} session{} from “{label}” in {}.",
        if count == 1 { "" } else { "s" },
        layout_name(layout)
    )
}

pub struct LaunchGroupDialog {
    target: LaunchTarget,
    /// Live sessions exist and would be closed by the new layout.
    live_sessions: bool,
    layout: SessionSetLayout,
    focus: FocusHandle,
}

impl EventEmitter<LaunchGroupEvent> for LaunchGroupDialog {}

impl Focusable for LaunchGroupDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the launch dialog. Every launch starts from the default layout.
pub fn open_launch_group(
    window: &mut Window,
    cx: &mut App,
    target: LaunchTarget,
    live_sessions: bool,
) -> Entity<LaunchGroupDialog> {
    cx.new(|cx| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        LaunchGroupDialog {
            target,
            live_sessions,
            layout: SessionSetLayout::Tabs,
            focus,
        }
    })
}

impl LaunchGroupDialog {
    fn launch(&mut self, cx: &mut Context<Self>) {
        if self.target.hosts.is_empty() {
            return;
        }
        cx.emit(LaunchGroupEvent::Launch {
            hosts: self.target.hosts.clone(),
            layout: self.layout,
            label: self.target.label.clone(),
        });
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(LaunchGroupEvent::Close);
        }
    }
}

impl Render for LaunchGroupDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let entity = cx.entity().downgrade();
        let toggles = LAYOUTS
            .iter()
            .map(|(layout, label, icon_name)| {
                let selected = *layout == self.layout;
                let ink = toggle_ink(&theme, selected);
                let entity = entity.clone();
                let layout = *layout;
                ToggleButton::new(
                    SharedString::from(format!("launch-{label}")),
                    *label,
                    selected,
                )
                // `Stack spacing={0.25}`: the icon over a caption.
                .content(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(Theme::spacing(0.25))
                        .child(icon(icon_name, SMALL, ink))
                        .child(text(*label, caption(), ink).no_truncate()),
                )
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.layout = layout;
                        cx.notify();
                    });
                })
            })
            .collect();
        let mut content = div()
            .flex()
            .flex_col()
            .child(div().mb(Theme::spacing(2.)).child(para(
                format!(
                    "Start all {} hosts and replace the current pane layout.",
                    self.target.hosts.len()
                ),
                body2(),
                p.text.secondary,
            )))
            .child(ToggleButtonGroup::new(toggles));
        if self.live_sessions {
            content = content.child(div().mt(Theme::spacing(2.)).child(Alert::new(
                Status::Warning,
                "Existing live sessions will be closed when this group replaces the current layout.",
            )));
        }
        let (cancel, launch, close) = (entity.clone(), entity.clone(), entity);
        let actions = vec![
            Button::new("launch-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(LaunchGroupEvent::Close));
                })
                .into_any_element(),
            Button::new("launch-go", "Launch group")
                .contained()
                .disabled(self.target.hosts.is_empty())
                .start_icon("PlayArrowOutlined")
                .on_click(move |_, _, cx| {
                    let _ = launch.update(cx, |this, cx| this.launch(cx));
                })
                .into_any_element(),
        ];
        div()
            .id("launch-group")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                dialog_frame(
                    &theme,
                    window,
                    "launch-group-content",
                    DIALOG_XS,
                    SharedString::from(format!("Launch “{}”", self.target.label)),
                    16.,
                    content.into_any_element(),
                    actions,
                    false,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(LaunchGroupEvent::Close));
                })),
            ))
    }
}
