//! The multi-exec control of the top bar (`client/src/components/MultiExecControl.tsx`):
//! the button (with the selection count) and its popover, where terminals
//! are picked for mirrored input, saved as workspace groups and cleared.
//!
//! It works on the window's [`MultiExec`] model and reads the window's tabs
//! through [`MultiExecControl::set_tabs`].

use std::collections::HashSet;

use muxus_core::api::WorkspaceMultiExecGroup;
use muxus_ui::color::{alpha, try_hex};
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    Anchor, AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::display::{badge, divider};
use muxus_ui::widgets::list::{ListItemButton, list_item_text};
use muxus_ui::widgets::menu::{self, popover_paper};
use muxus_ui::widgets::tooltip::AnchorBounds;
use muxus_ui::widgets::{Button, ButtonColor, Checkbox, Chip, IconButton, TextField};

use super::model::MultiExec;
use crate::features::launcher::kit::{follow_anchor, overline, text};
use crate::keymap::bindings::KeybindingOverrides;
use crate::keymap::hints::chord_label;
use crate::state::tabs::{TabsState, TerminalTab, is_multi_exec_target};
use crate::state::workspace_layout::visible_tab_ids;

/// The control's view of the window's tabs.
#[derive(Clone, Debug, Default)]
struct TabsView {
    connected: Vec<TerminalTab>,
    visible: Vec<String>,
    active_pane_id: String,
}

pub struct MultiExecControl {
    model: Entity<MultiExec>,
    tabs: TabsView,
    open: bool,
    anchor: AnchorBounds,
    group_name: Entity<InputState>,
    /// `terminal.multi-exec` as the user bound it.
    toggle_chord: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// The control for the window's multi-exec model.
pub fn multi_exec_control(
    model: Entity<MultiExec>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<MultiExecControl> {
    cx.new(|cx| MultiExecControl::new(model, window, cx))
}

impl MultiExecControl {
    pub fn new(model: Entity<MultiExec>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let group_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Save selection as a group"));
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &group_name,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.save_group(window, cx),
                    _ => cx.notify(),
                },
            ),
        ];
        let prefs = super::model::prefs(cx);
        let overrides: KeybindingOverrides = prefs
            .keybindings
            .iter()
            .map(|(id, chords)| (id.clone(), chords.clone()))
            .collect();
        Self {
            model,
            tabs: TabsView::default(),
            open: false,
            anchor: AnchorBounds::default(),
            group_name,
            toggle_chord: chord_label("terminal.multi-exec", &overrides),
            _subscriptions: subscriptions,
        }
    }

    /// The window's tabs changed (the model reconciles its own selection).
    pub fn set_tabs(&mut self, tabs: &TabsState, cx: &mut Context<Self>) {
        let connected: Vec<TerminalTab> = tabs
            .tabs
            .iter()
            .filter(|tab| is_multi_exec_target(tab))
            .cloned()
            .collect();
        let ids: HashSet<&str> = connected.iter().map(|tab| tab.id.as_str()).collect();
        self.tabs = TabsView {
            visible: visible_tab_ids(&tabs.root, tabs.zoomed_pane_id.as_deref())
                .into_iter()
                .filter(|id| ids.contains(id.as_str()))
                .collect(),
            active_pane_id: tabs.active_pane_id.clone(),
            connected,
        };
        cx.notify();
    }

    /// Open the popover (the window's shortcut or the top bar).
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        cx.notify();
    }

    fn connected_ids(&self) -> Vec<String> {
        self.tabs
            .connected
            .iter()
            .map(|tab| tab.id.clone())
            .collect()
    }

    fn save_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = muxus_core::js::js_trim(&self.group_name.read(cx).value()).to_owned();
        if name.is_empty() {
            return;
        }
        self.model.update(cx, |model, cx| {
            model.update(cx, |state| {
                state.save_group(&name, None);
            })
        });
        self.group_name
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    fn select(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.update(cx, |state| state.set_selection(ids))
        });
    }

    fn group_row(
        &self,
        index: usize,
        group: &WorkspaceMultiExecGroup,
        theme: &Theme,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let connected: HashSet<String> = self.connected_ids().into_iter().collect();
        let count = group
            .tab_ids
            .iter()
            .filter(|id| connected.contains(*id))
            .count();
        let (activate, delete) = (self.model.clone(), self.model.clone());
        let (id, delete_id) = (group.id.clone(), group.id.clone());
        let available = self.connected_ids();
        ListItemButton::new(("multi-exec-group", index))
            .disabled(count == 0)
            .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
            .margin_x(Theme::spacing_css(0.75))
            .on_click(move |_, _, cx| {
                let available = available.clone();
                activate.update(cx, |model, cx| {
                    model.update(cx, |state| state.activate_group(&id, &available))
                });
            })
            .child(div().mr(Theme::spacing(1.25)).child(icon(
                "PodcastsOutlined",
                SMALL,
                if count >= 2 {
                    p.warning.main
                } else {
                    p.action.disabled
                },
            )))
            .child(list_item_text(
                text(group.name.clone(), t.body2, p.text.primary),
                Some(
                    text(
                        format!("{count} of {} connected", group.tab_ids.len()),
                        t.body2,
                        p.text.secondary,
                    )
                    .into_any_element(),
                ),
            ))
            .child(
                IconButton::new(("multi-exec-group-delete", index), "DeleteOutlined")
                    .tooltip(format!("Delete {}", group.name))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        delete.update(cx, |model, cx| {
                            model.update(cx, |state| state.delete_group(&delete_id))
                        });
                    }),
            )
            .into_any_element()
    }

    fn popover(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let state = self.model.read(cx).state().clone();
        let active = state.active();
        let selected: HashSet<&str> = state.selected_ids.iter().map(String::as_str).collect();
        let weak = cx.entity().downgrade();

        let mut paper = popover_paper(theme).w(px(360.)).overflow_hidden();
        // The caption is an inline span: it sits in the body1 line box.
        let mut header = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.5))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(text("Multi-execution", t.subtitle2, p.text.primary))
                    .child(
                        muxus_ui::label::paragraph(
                            "Typing in any selected terminal mirrors to all selected terminals.",
                        )
                        .size(t.caption.size)
                        .line_height(t.body1.line_height)
                        .color(p.text.secondary),
                    ),
            );
        if active {
            header = header.child(Chip::new("Active").outlined().palette(p.warning));
        }
        paper = paper.child(header).child(divider(theme));

        if !state.groups.is_empty() {
            paper = paper.child(
                div()
                    .px(Theme::spacing(2.))
                    .pt(Theme::spacing(0.75))
                    .child(overline("Workspace groups", theme, p.text.secondary)),
            );
            let mut groups = div()
                .id("multi-exec-groups")
                .flex()
                .flex_col()
                .max_h(px(140.))
                .overflow_y_scroll()
                .pb(Theme::spacing(0.5));
            for (index, group) in state.groups.iter().enumerate() {
                groups = groups.child(self.group_row(index, group, theme));
            }
            paper = paper.child(groups).child(divider(theme));
        }

        let current_pane: Vec<String> = self
            .tabs
            .connected
            .iter()
            .filter(|tab| tab.pane_id == self.tabs.active_pane_id)
            .map(|tab| tab.id.clone())
            .collect();
        let visible = self.tabs.visible.clone();
        let all = self.connected_ids();
        let (w1, w2, w3) = (weak.clone(), weak.clone(), weak.clone());
        let (this_split, visible_splits, all_live) =
            (current_pane.clone(), visible.clone(), all.clone());
        paper = paper
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(0.75))
                    .px(Theme::spacing(1.5))
                    .py(Theme::spacing(1.))
                    .child(
                        Button::new("multi-exec-this-split", "This split")
                            .outlined()
                            .disabled(current_pane.is_empty())
                            .on_click(move |_, _, cx| {
                                let ids = this_split.clone();
                                let _ = w1.update(cx, |this, cx| this.select(ids, cx));
                            }),
                    )
                    .child(
                        Button::new("multi-exec-visible", "Visible splits")
                            .outlined()
                            .disabled(visible.is_empty())
                            .on_click(move |_, _, cx| {
                                let ids = visible_splits.clone();
                                let _ = w2.update(cx, |this, cx| this.select(ids, cx));
                            }),
                    )
                    .child(
                        Button::new("multi-exec-all", "All live")
                            .outlined()
                            .disabled(all.is_empty())
                            .on_click(move |_, _, cx| {
                                let ids = all_live.clone();
                                let _ = w3.update(cx, |this, cx| this.select(ids, cx));
                            }),
                    ),
            )
            .child(divider(theme));

        let mut list = div()
            .id("multi-exec-targets")
            .flex()
            .flex_col()
            .max_h(px(300.))
            .overflow_y_scroll()
            .py(Theme::spacing(0.5));
        for (index, tab) in self.tabs.connected.iter().enumerate() {
            let checked = selected.contains(tab.id.as_str());
            let toggle = self.model.clone();
            let id = tab.id.clone();
            let dot = tab
                .color
                .as_deref()
                .and_then(try_hex)
                .unwrap_or(p.success.main);
            list = list.child(
                ListItemButton::new(("multi-exec-target", index))
                    .margin_x(Theme::spacing_css(0.75))
                    // A dense list's 4px rows.
                    .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
                    .on_click(move |_, _, cx| {
                        toggle.update(cx, |model, cx| {
                            model.update(cx, |state| state.toggle_target(&id))
                        });
                    })
                    .child(
                        div().mr(Theme::spacing(0.75)).child(
                            Checkbox::new(("multi-exec-check", index), checked)
                                .small()
                                .padding(Theme::spacing_css(0.5))
                                .checked_color(p.warning.main),
                        ),
                    )
                    .child(
                        div()
                            .size(Theme::spacing(1.))
                            .rounded_full()
                            .mr(Theme::spacing(1.25))
                            .flex_none()
                            .bg(dot),
                    )
                    .child(list_item_text(
                        text(tab.title.clone(), t.body2, p.text.primary),
                        Some(
                            text(
                                if tab.pane_id == self.tabs.active_pane_id {
                                    "Current split"
                                } else {
                                    "Live session"
                                },
                                t.body2,
                                p.text.secondary,
                            )
                            .into_any_element(),
                        ),
                    )),
            );
        }
        if self.tabs.connected.is_empty() {
            list = list.child(
                div()
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(3.))
                    .flex()
                    .justify_center()
                    .child(text(
                        "Connect at least two sessions to start mirroring input.",
                        t.body2,
                        p.text.secondary,
                    )),
            );
        }
        paper = paper.child(list);

        if state.selected_ids.len() >= 2 {
            let save = weak.clone();
            let has_name = !muxus_core::js::js_trim(&self.group_name.read(cx).value()).is_empty();
            paper = paper.child(
                // A stretching row: the button is as tall as the field.
                div()
                    .flex()
                    .gap(Theme::spacing(0.75))
                    .px(Theme::spacing(1.5))
                    .py(Theme::spacing(1.))
                    .border_t_1()
                    .border_color(p.divider)
                    .child(div().flex_1().min_w_0().child(
                        TextField::new("multi-exec-group-name", &self.group_name).full_width(),
                    ))
                    .child(
                        Button::new("multi-exec-save", "Save")
                            .outlined()
                            .start_icon("SaveOutlined")
                            .disabled(!has_name)
                            .on_click(move |_, window, cx| {
                                let _ = save.update(cx, |this, cx| this.save_group(window, cx));
                            }),
                    ),
            );
        }
        if state.selected_ids.len() == 1 {
            paper = paper.child(
                div()
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(1.))
                    .child(text(
                        "Select one more terminal to enable multi-execution.",
                        t.caption,
                        theme.status_text_color(muxus_ui::theme::Status::Warning),
                    )),
            );
        }
        if active {
            let clear = weak.clone();
            paper = paper.child(
                div()
                    .flex()
                    .items_center()
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(1.))
                    .border_t_1()
                    .border_color(p.divider)
                    .child(div().flex_1().child(text(
                        "Input is mirrored automatically.",
                        t.caption,
                        theme.status_text_color(muxus_ui::theme::Status::Warning),
                    )))
                    .child(
                        Button::new("multi-exec-clear", "Clear selection")
                            .color(ButtonColor::Inherit)
                            .on_click(move |_, _, cx| {
                                let _ = clear.update(cx, |this, cx| this.select(Vec::new(), cx));
                            }),
                    ),
            );
        }
        paper.into_any_element()
    }
}

impl Render for MultiExecControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let state = self.model.read(cx).state().clone();
        let active = state.active();
        let count = state.selected_ids.len();
        let tooltip = {
            let title = if active {
                format!("Mirroring input across {count} terminals")
            } else {
                "Select terminals for multi-execution".to_owned()
            };
            // The chord toggles mirroring rather than opening this picker, so
            // it says so instead of standing alone as the button's shortcut.
            match &self.toggle_chord {
                Some(chord) => format!("{title} · {chord} toggles mirroring"),
                None => title,
            }
        };
        let weak = cx.entity().downgrade();
        let mut button = IconButton::new("multi-exec", "PodcastsOutlined")
            .tooltip(tooltip)
            .track_bounds(&self.anchor)
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.open = !this.open;
                    cx.notify();
                });
            });
        if active {
            button = button
                .color(ButtonColor::Warning)
                .background(alpha(p.warning.main, 0.12), alpha(p.warning.main, 0.2));
        }
        if count > 0 {
            let (fill, ink) = if active {
                (p.warning.main, p.warning.contrast_text)
            } else {
                (p.action.selected, p.text.primary)
            };
            button = button.badge(badge(count, fill, ink, 5., SMALL));
        }
        let mut root = div()
            .relative()
            .flex_none()
            .on_key_down(
                cx.listener(|this, event: &muxus_ui::gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" && this.open {
                        cx.stop_propagation();
                        this.open = false;
                        cx.notify();
                    }
                }),
            )
            .child(button);
        if self.open
            && let Some(bounds) = self.anchor.get()
        {
            follow_anchor(&self.anchor, bounds, window, cx);
            let close = cx.entity().downgrade();
            let popover = self.popover(&theme, cx);
            root = root.child(menu::popup_at(
                // `anchorOrigin` bottom-right, `transformOrigin` top-right,
                // `mt: 0.75`; the popup keeps 16px of room on its far side.
                bounds.bottom_right()
                    + muxus_ui::gpui_kit::point(Theme::spacing(2.), Theme::spacing(0.75)),
                Anchor::TopRight,
                move |_, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.open = false;
                        cx.notify();
                    });
                },
                popover,
            ));
        }
        root
    }
}
