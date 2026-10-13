//! Command buttons and multi-execution on the documentation's demo sandbox,
//! with the command groups of the `command-buttons` screenshot
//! (`hack/capture.mjs`) and web-01 connected.
//!
//! ```bash
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_COMMAND_SCENE=bar out.png \
//!   -- target-ui-c/debug/examples/command_buttons
//! ```
//!
//! - `MUXUS_THEME=light|dark` (default light)
//! - `MUXUS_COMMAND_SCENE`: `bar` (the command bar under the top bar, the
//!   default), `group-menu` (the bar's group picker open), `menu` (the
//!   Ctrl+Space menu at the terminal's caret), `dialog` (the editor),
//!   `multi-exec` (two split sessions mirrored), `multi-exec-off` (one
//!   session, nothing mirrored)
//! - `MUXUS_WINDOW=fullscreen` fills the monitor
//! - `MUXUS_DATA_DIR=<dir>` keeps the sandbox there instead of a temp dir

use muxus_app::features::command_buttons::multi_exec_control::multi_exec_control;
use muxus_app::features::command_buttons::{
    CommandButtons, CommandHooks, MultiExec, action_bar, open_command_buttons, open_command_menu,
    set_command_hooks,
};
use muxus_app::features::launcher::demo::{connected_tab, demo_env, run_demo};
use muxus_app::state::tabs::{IdGen, TabPatch, TabsState};
use muxus_app::state::workspace_layout::PaneDirection;
use muxus_store::prefs::command_buttons::{CommandButton, CommandButtonColor, CommandButtonGroup};
use muxus_ui::gpui_kit::{
    AnyView, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
    point, px,
};
use muxus_ui::theme::{ActiveTheme, layout};

fn button(
    id: &str,
    label: &str,
    command: &str,
    send_enter: bool,
    group: Option<&str>,
    color: Option<CommandButtonColor>,
) -> CommandButton {
    CommandButton {
        id: id.into(),
        label: label.into(),
        command: command.into(),
        send_enter,
        group_id: group.map(Into::into),
        color,
        secret_id: None,
    }
}

/// The window around the screen: the top bar's height of chrome (the multi-exec
/// button sits where the top bar puts it), then the bar.
struct Chrome {
    bar: Option<AnyView>,
    control: Option<AnyView>,
    overlay: Option<AnyView>,
}

impl Render for Chrome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let mut top = div()
            .relative()
            // The bar's height plus its bottom border.
            .h(px(layout::TOP_BAR_HEIGHT + 1.))
            .flex_none()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.divider);
        if let Some(control) = &self.control {
            // Where the top bar puts the multi-exec button at 1420px.
            top = top.child(
                div()
                    .absolute()
                    .left(px(1009.7))
                    .top(px(11.7))
                    .child(control.clone()),
            );
        }
        let mut root = div().size_full().flex().flex_col().child(top);
        if let Some(bar) = &self.bar {
            root = root.child(bar.clone());
        }
        // Where the terminal would be.
        root = root.child(div().flex_1().bg(theme.components.session_surface));
        if let Some(overlay) = &self.overlay {
            root = root.child(overlay.clone());
        }
        div().size_full().relative().child(root)
    }
}

fn main() {
    let scene = std::env::var("MUXUS_COMMAND_SCENE").unwrap_or_else(|_| "bar".into());
    let env = demo_env("command-buttons", |prefs| {
        prefs.command_button_groups = vec![
            CommandButtonGroup {
                id: "default".into(),
                name: "Web servers".into(),
            },
            CommandButtonGroup {
                id: "network".into(),
                name: "Network".into(),
            },
            CommandButtonGroup {
                id: "troubleshooting".into(),
                name: "Troubleshooting".into(),
            },
        ];
        prefs.command_buttons = vec![
            button(
                "a",
                "status",
                "status",
                true,
                None,
                Some(CommandButtonColor::Green),
            ),
            button(
                "b",
                "tail access log",
                "tailaccess",
                true,
                None,
                Some(CommandButtonColor::Blue),
            ),
            button("c", "disk", "df -h", true, None, None),
            button(
                "d",
                "restart edge",
                "sudo systemctl restart edge",
                false,
                None,
                Some(CommandButtonColor::Red),
            ),
            button(
                "e",
                "BGP summary",
                "show bgp summary",
                true,
                Some("network"),
                Some(CommandButtonColor::Purple),
            ),
            button(
                "f",
                "interfaces",
                "show interfaces brief",
                true,
                Some("network"),
                None,
            ),
            button(
                "g",
                "ping gateway",
                "ping -c 3 10.0.0.1",
                true,
                Some("troubleshooting"),
                Some(CommandButtonColor::Orange),
            ),
        ];
    });
    let backend = env.backend.clone();
    run_demo(&env, move |window, cx| {
        let mut tabs = TabsState::with_pane("pane-1", IdGen::sequential());
        let mut ids = vec![connected_tab(&mut tabs, "web-01")];
        if scene == "multi-exec" {
            let pane = tabs.active_pane_id.clone();
            tabs.split(&pane, PaneDirection::Right);
            ids.push(connected_tab(&mut tabs, "web-01"));
        }
        // Connecting a host carries its color into the tab.
        for id in &ids {
            tabs.update(
                id,
                TabPatch {
                    color: Some(Some("#3b82f6".into())),
                    ..Default::default()
                },
            );
        }
        // The caret of web-01's prompt, where the Ctrl+Space menu opens.
        set_command_hooks(
            CommandHooks {
                cursor_anchor: Some(std::rc::Rc::new(|_, _, _| Some(point(px(410.), px(161.))))),
                ..CommandHooks::default()
            },
            cx,
        );
        let model: Entity<CommandButtons> = cx.new(|cx| CommandButtons::new(backend.clone(), cx));
        model.update(cx, |model, cx| model.set_tabs(&tabs, cx));
        let (bar, control, overlay): (Option<AnyView>, Option<AnyView>, Option<AnyView>) =
            match scene.as_str() {
                "dialog" => (
                    Some(action_bar(model.clone(), cx).into()),
                    None,
                    Some(open_command_buttons(model.clone(), window, cx).into()),
                ),
                "menu" => (
                    Some(action_bar(model.clone(), cx).into()),
                    None,
                    Some(open_command_menu(model.clone(), window, cx).into()),
                ),
                "multi-exec" | "multi-exec-off" => {
                    let multi = cx.new(|_| MultiExec::new());
                    multi.update(cx, |multi, cx| multi.set_tabs(&tabs, cx));
                    if scene == "multi-exec" {
                        let ids: Vec<String> = tabs.tabs.iter().map(|tab| tab.id.clone()).collect();
                        multi.update(cx, |multi, cx| {
                            multi.update(cx, |state| state.set_selection(ids))
                        });
                    }
                    let control = multi_exec_control(multi, window, cx);
                    control.update(cx, |control, cx| {
                        control.set_tabs(&tabs, cx);
                        control.open(cx);
                    });
                    (None, Some(control.into()), None)
                }
                _ => {
                    let bar = action_bar(model.clone(), cx);
                    if scene == "group-menu" {
                        // The group button needs a frame to know where it is.
                        let bar = bar.clone();
                        window.on_next_frame(move |_, cx| {
                            bar.update(cx, |bar, cx| bar.open_group_menu(cx));
                        });
                    }
                    (Some(bar.into()), None, None)
                }
            };
        cx.new(|_| Chrome {
            bar,
            control,
            overlay,
        })
        .into()
    });
}
