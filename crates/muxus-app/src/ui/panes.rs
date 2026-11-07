//! The pane canvas (`PaneCanvas` in AppShell.tsx), the per-pane tab strip
//! (`components/TabStrip.tsx`, its drags and slides in `ui::tab_drag`), the
//! blank-tab chooser (`components/EmptyPane.tsx`) and the framed surface a
//! session view stands on for the frame before it is created.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use muxus_app::model::SessionProfile;
use muxus_app::sidebar::kind_icon::host_kind_icon_name;
use muxus_app::state::tabs::{Edge, TabStatus, TerminalTab};
use muxus_app::state::workspace_layout::{
    FlatDivider, LayoutRect, PaneDirection, SplitDirection, flatten_pane_layout,
};
use muxus_ui::color::{alpha, over, try_hex};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    Animation, AnimationExt, AnyElement, App, AppContext, Bounds, BoxShadow, ClickEvent, Context,
    CursorStyle, Div, DragMoveEvent, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Transformation, Window, canvas, div, linear_color_stop,
    linear_gradient, point, px, relative, size,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Status, Theme, layout};
use muxus_ui::widgets::display::vertical_divider;
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::{IconButton, Tooltip};

use crate::ui::paste_progress::tab_paste_progress;
use crate::ui::prefs::TabNumberVisibility;
use crate::ui::shell::{AppShell, ShellMenu};
use crate::ui::tab_drag::{tab_width, viewport_width};
use crate::ui::top_bar::with_chord;
use muxus_app::features::notices::tab_strip::{
    TabScrollState, earlier_tabs_button, later_tabs_button, scroll_row_by, scroll_step, wheel_delta,
};

const MIN_SPLIT_RATIO: f64 = 0.1;
const MAX_SPLIT_RATIO: f64 = 0.9;

/// A tab being dragged within or between strips.
#[derive(Clone, Debug)]
pub struct DraggedTab {
    pub tab_id: String,
    pub title: SharedString,
    pub icon: &'static str,
}

/// The drag image: the tab as it looks in the strip.
pub struct DraggedTabView(DraggedTab);

impl Render for DraggedTabView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let p = theme.palette;
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(37.))
            .w(px(190.))
            .px(px(10.))
            .bg(p.background_default)
            .border_1()
            .border_color(p.divider)
            .opacity(0.9)
            .child(icon(self.0.icon, 15., p.text.secondary))
            .child(
                div()
                    .ui_font(13., 450.)
                    .text_color(p.text.primary)
                    .child(self.0.title.clone()),
            )
    }
}

/// Bounds of the canvas and of each tab from the last paint, for drags.
#[derive(Default)]
pub struct PaneGeometry {
    pub canvas: Cell<Bounds<Pixels>>,
    pub tabs: std::cell::RefCell<HashMap<String, Bounds<Pixels>>>,
    /// Each empty pane's "Saved host" card, which the host picker hangs from.
    pub picker_cards: std::cell::RefCell<HashMap<String, Bounds<Pixels>>>,
}

/// A split being resized.
#[derive(Clone, Debug)]
pub struct SplitDrag {
    pub split_id: String,
    pub direction: SplitDirection,
    pub bounds: LayoutRect,
}

fn tab_icon(tab: &TerminalTab) -> &'static str {
    if let Some(icon) = tab
        .profile
        .as_ref()
        .and_then(muxus_app::sidebar::kind_icon::environment_tab_icon)
    {
        return icon;
    }
    match &tab.profile {
        None => "Add",
        Some(SessionProfile::Local { .. }) => "Terminal",
        Some(profile) => host_kind_icon_name(profile.kind()),
    }
}

fn tab_status_label(tab: &TerminalTab) -> String {
    if tab.profile.is_none() {
        return "Choose a session".into();
    }
    match tab.status {
        TabStatus::Connected => "Connected".into(),
        TabStatus::Connecting => "Connecting".into(),
        TabStatus::Interrupted => tab
            .failure_reason
            .clone()
            .unwrap_or_else(|| "Connection interrupted".into()),
        TabStatus::Closed => tab
            .failure_reason
            .clone()
            .unwrap_or_else(|| "Disconnected".into()),
        TabStatus::Idle => "Ready".into(),
    }
}

fn status_dot(status: TabStatus) -> Option<Status> {
    match status {
        TabStatus::Connecting | TabStatus::Interrupted => Some(Status::Warning),
        TabStatus::Connected => Some(Status::Success),
        TabStatus::Closed => Some(Status::Error),
        TabStatus::Idle => None,
    }
}

impl AppShell {
    pub fn render_pane_canvas(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let flat = flatten_pane_layout(&self.tabs.root);
        let zoomed = self.tabs.zoomed_pane_id.clone();
        let active_pane = self.tabs.active_pane_id.clone();
        let geometry = self.geometry.clone();
        let multi = flat.panes.len() > 1;

        let mut container = div()
            .id("pane-canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, _, _| geometry.canvas.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        for flat_pane in &flat.panes {
            let pane_id = flat_pane.pane.id.clone();
            let is_zoomed = zoomed.as_deref() == Some(pane_id.as_str());
            if zoomed.is_some() && !is_zoomed {
                continue;
            }
            let rect = if is_zoomed {
                LayoutRect {
                    x: 0.,
                    y: 0.,
                    width: 1.,
                    height: 1.,
                }
            } else {
                flat_pane.rect
            };
            let focused = pane_id == active_pane;
            let opacity = crate::ui::prefs::pane_opacity(&self.prefs, focused);
            // The pane's width last frame (CSS px), for its container queries.
            let pane_width =
                muxus_ui::zoom::unzoom(self.geometry.canvas.get().size.width) * rect.width as f32;
            let content = self.render_pane_content(
                &pane_id,
                flat_pane.pane.active_tab_id.as_deref(),
                pane_width,
                window,
                cx,
            );
            let focus_id = pane_id.clone();
            let mut pane = div()
                .id(ElementId::Name(format!("pane-{pane_id}").into()))
                .absolute()
                .left(relative(rect.x as f32))
                .top(relative(rect.y as f32))
                .w(relative(rect.width as f32))
                .h(relative(rect.height as f32))
                .flex()
                .flex_col()
                .overflow_hidden()
                .opacity(opacity)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        if this.tabs.active_pane_id != focus_id {
                            this.tabs.focus_pane(&focus_id);
                            cx.notify();
                        }
                    }),
                );
            if is_zoomed {
                pane = pane.bg(theme.palette.background_default);
            }
            if !self.focus_mode {
                pane = pane.child(
                    self.render_tab_strip(&pane_id, focused, is_zoomed, pane_width, window, cx),
                );
            }
            pane = pane.child(div().flex_1().min_h_0().child(content));
            if self.prefs.active_pane_border && multi && zoomed.is_none() && focused {
                pane = pane.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(alpha(theme.palette.primary.main, 0.8)),
                );
            }
            container = container.child(pane);
        }
        if zoomed.is_none() {
            for divider in &flat.dividers {
                container = container.child(self.render_divider(divider, &theme, cx));
            }
        }
        container
            .on_mouse_move(cx.listener(
                |this, event: &muxus_ui::gpui_kit::MouseMoveEvent, _, cx| {
                    let Some(drag) = this.split_drag.clone() else {
                        return;
                    };
                    if !event.dragging() {
                        this.split_drag = None;
                        return;
                    }
                    let canvas = this.geometry.canvas.get();
                    let (fraction, start, size) = match drag.direction {
                        SplitDirection::Horizontal => (
                            f64::from(
                                muxus_ui::zoom::unzoom(event.position.x - canvas.origin.x)
                                    / muxus_ui::zoom::unzoom(canvas.size.width),
                            ),
                            drag.bounds.x,
                            drag.bounds.width,
                        ),
                        SplitDirection::Vertical => (
                            f64::from(
                                muxus_ui::zoom::unzoom(event.position.y - canvas.origin.y)
                                    / muxus_ui::zoom::unzoom(canvas.size.height),
                            ),
                            drag.bounds.y,
                            drag.bounds.height,
                        ),
                    };
                    if size <= 0. {
                        return;
                    }
                    let ratio = ((fraction - start) / size).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                    this.tabs.resize_split(&drag.split_id, ratio);
                    cx.notify();
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.split_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    fn render_divider(
        &self,
        divider: &FlatDivider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let b = divider.bounds;
        let hover = theme.palette.primary.main;
        let line = theme.palette.divider;
        let group: SharedString = format!("divider-{}", divider.split_id).into();
        let drag = SplitDrag {
            split_id: divider.split_id.clone(),
            direction: divider.direction,
            bounds: b,
        };
        let reset_id = divider.split_id.clone();
        let el = div()
            .id(ElementId::Name(
                format!("divider-{}", divider.split_id).into(),
            ))
            .group(group.clone())
            .absolute()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count == 2 {
                        this.tabs.resize_split(&reset_id, 0.5);
                    } else {
                        this.split_drag = Some(drag.clone());
                    }
                    cx.notify();
                }),
            );
        let el = match divider.direction {
            SplitDirection::Horizontal => el
                .left(relative((b.x + b.width * divider.ratio) as f32))
                .ml(px(-3.))
                .top(relative(b.y as f32))
                .w(px(6.))
                .h(relative(b.height as f32))
                .cursor(CursorStyle::ResizeLeftRight)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(3.))
                        .w(px(1.))
                        .bg(line)
                        .group_hover(group, move |s| s.bg(hover)),
                ),
            SplitDirection::Vertical => el
                .top(relative((b.y + b.height * divider.ratio) as f32))
                .mt(px(-3.))
                .left(relative(b.x as f32))
                .h(px(6.))
                .w(relative(b.width as f32))
                .cursor(CursorStyle::ResizeUpDown)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(3.))
                        .h(px(1.))
                        .bg(line)
                        .group_hover(group, move |s| s.bg(hover)),
                ),
        };
        el.into_any_element()
    }

    fn render_pane_content(
        &mut self,
        pane_id: &str,
        active_tab_id: Option<&str>,
        pane_width: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let tab = active_tab_id
            .and_then(|id| self.tabs.tab(id))
            .filter(|tab| tab.pane_id == pane_id)
            .cloned();
        match tab {
            None => self.render_empty_pane(pane_id, None, pane_width, &theme, cx),
            Some(tab) if tab.profile.is_none() => {
                self.render_empty_pane(pane_id, Some(tab.id.clone()), pane_width, &theme, cx)
            }
            Some(tab) => match self.terminals.get(&tab.id) {
                // Cached: a terminal that did not change is not rendered again
                // when another one (or the chrome) redraws, as when a cursor blinks.
                Some(terminal) => {
                    let view = muxus_ui::gpui_kit::AnyView::from(terminal.view.clone())
                        .cached(muxus_ui::gpui_kit::StyleRefinement::default().size_full())
                        .into_any_element();
                    self.render_session_body(&tab, view, cx)
                }
                None => match self.session_views.get(&tab.id) {
                    Some(view) => div().size_full().child(view.any_view()).into_any_element(),
                    None => session_placeholder(&tab, &theme),
                },
            },
        }
    }

    /// `EmptyPane`: "Start a session" with the three launch cards.
    fn render_empty_pane(
        &mut self,
        pane_id: &str,
        replace: Option<String>,
        pane_width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let h6 = theme.typography.h6;
        let body2 = theme.typography.body2;
        let replace_local = replace.clone();
        let replace_saved = replace.clone();
        // `repeat(auto-fit, minmax(132px, 1fr))` over `min(100%, 570px)`: as many
        // columns as fit, the cards sharing a row's width, 8px apart.
        let available = if pane_width > 0. {
            (pane_width - 40.).clamp(0., 570.)
        } else {
            570.
        };
        let columns = empty_pane_columns(available, 3);
        let card_width = (available - EMPTY_CARD_GAP * (columns as f32 - 1.)) / columns as f32;
        let picker_card = (self.geometry.clone(), pane_id.to_string());
        let picker_pane = pane_id.to_string();
        let cards = div()
            .flex()
            .flex_wrap()
            .gap(px(EMPTY_CARD_GAP))
            .w(px(available))
            .child(empty_action(
                "empty-local",
                "Terminal",
                "Local terminal",
                "This device",
                true,
                card_width,
                None,
                theme,
                cx.listener(move |this, _, _, cx| {
                    this.open_local(replace_local.as_deref(), cx);
                }),
            ))
            .child(empty_action(
                "empty-saved",
                "DnsOutlined",
                "Saved host",
                "Choose any host",
                false,
                card_width,
                Some(picker_card),
                theme,
                cx.listener(move |this, event: &ClickEvent, window, cx| {
                    // The picker hangs below the card (`anchorEl`).
                    let position = this
                        .geometry
                        .picker_cards
                        .borrow()
                        .get(&picker_pane)
                        .map(|bounds| bounds.bottom_left())
                        .unwrap_or_else(|| click_position(event));
                    this.open_host_picker(position, replace_saved.clone(), window, cx);
                }),
            ))
            .child(empty_action(
                "empty-add",
                "Add",
                "Add host",
                "SSH, Telnet, serial, RDP, VNC, gNMI, NETCONF",
                false,
                card_width,
                None,
                theme,
                cx.listener(|this, _, window, cx| this.add_host(None, window, cx)),
            ));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(20.))
            .py(px(32.))
            .gap(px(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div().mb(px(4.)).child(
                            label("Start a session")
                                .size(h6.size)
                                .weight(650.)
                                .line_height(h6.line_height)
                                .tracking(h6.letter_spacing)
                                .color(p.text.primary)
                                .no_truncate(),
                        ),
                    )
                    .child(
                        paragraph("Open a shell, connect over SSH, Telnet or serial, open a remote desktop, or manage a device over gNMI or NETCONF.")
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.secondary)
                            .center(),
                    ),
            )
            .child(cards)
            .into_any_element()
    }

    pub fn render_tab_strip(
        &mut self,
        pane_id: &str,
        focused: bool,
        zoomed: bool,
        pane_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let tabs: Vec<TerminalTab> = self.tabs.pane_tabs(pane_id).into_iter().cloned().collect();
        let active_id = self.active_tab_of(pane_id);
        // FLIP: tabs whose place changed slide there from their old slots.
        let viewport = viewport_width(window);
        let order: Vec<(String, f32)> = tabs
            .iter()
            .map(|tab| {
                let active = active_id.as_deref() == Some(tab.id.as_str());
                (tab.id.clone(), tab_width(active, viewport))
            })
            .collect();
        if self.update_tab_slides(pane_id, order) {
            window.request_animation_frame();
        }
        // `@container (max-width: 560px)`: the split and zoom buttons fold into
        // "More pane actions".
        let compact = pane_width > 0. && pane_width <= 560.;
        let multi_exec = self.multi_exec.read(cx).state().selected_ids.clone();
        let numbers: HashMap<String, usize> =
            muxus_app::state::tabs::tabs_in_order(&self.tabs.root, &self.tabs.tabs)
                .into_iter()
                .enumerate()
                .map(|(index, tab)| (tab.id.clone(), index + 1))
                .collect();
        let can_close_pane = matches!(
            self.tabs.root,
            muxus_app::state::workspace_layout::PaneNode::Split(_)
        );

        let border = if focused {
            alpha(p.text.primary, 0.18)
        } else {
            p.divider
        };
        let scroll = self
            .tab_scroll
            .entry(pane_id.to_string())
            .or_default()
            .clone();
        let scroll_state = TabScrollState::of(&scroll);
        self.reveal_active_tab(pane_id, active_id.as_deref(), &scroll);
        let mut strip = div()
            .id(ElementId::Name(format!("tab-strip-{pane_id}").into()))
            .flex()
            .flex_none()
            .items_stretch()
            .h(px(layout::TAB_STRIP_HEIGHT))
            .bg(p.sidebar)
            .border_b_1()
            .border_color(border)
            .overflow_hidden();
        // The tabs scroll sideways when they do not fit; the wheel scrolls them too.
        let mut row = div()
            .id(ElementId::Name(format!("tab-row-{pane_id}").into()))
            .flex()
            .flex_1()
            .min_w_0()
            .items_stretch()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .on_scroll({
                let scroll = scroll.clone();
                cx.listener(
                    move |_, event: &muxus_ui::gpui_kit::ScrollEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        let step = wheel_delta(
                            muxus_ui::zoom::unzoom(delta.x),
                            muxus_ui::zoom::unzoom(delta.y),
                        );
                        if step != 0. {
                            scroll_row_by(&scroll, -step);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    },
                )
            });
        {
            let pane = pane_id.to_string();
            strip = strip.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.tabs.focus_pane(&pane);
                    this.menu = Some(ShellMenu::Pane {
                        pane_id: pane.clone(),
                        position: event.position,
                    });
                    cx.notify();
                }),
            );
        }
        // A dragged tab over the strip: live resorting in its own strip, the
        // insertion line in another; a drop past the last tab goes last.
        {
            let pane = pane_id.to_string();
            let drop_pane = pane_id.to_string();
            strip = strip
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                        let dragged = event.drag(cx).tab_id.clone();
                        this.tab_dragged_over(
                            &pane,
                            &dragged,
                            event.event.position,
                            event.bounds,
                            cx,
                        );
                    }),
                )
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                    this.drop_tab(&dragged.tab_id, &drop_pane, None, Edge::After, cx);
                }));
        }
        let drop = self
            .tab_motion
            .drop
            .clone()
            .filter(|drop| drop.pane_id == pane_id);
        for tab in &tabs {
            let active = active_id.as_deref() == Some(tab.id.as_str());
            let indicator = drop
                .as_ref()
                .filter(|drop| drop.target.as_deref() == Some(tab.id.as_str()))
                .map(|drop| drop.edge);
            row = row.child(
                self.render_tab(
                    tab,
                    TabLook {
                        active,
                        width: tab_width(active, viewport),
                        focused,
                        number: numbers.get(&tab.id).copied(),
                        indicator,
                        mirrored: multi_exec
                            .iter()
                            .any(|id| id == &tab.id)
                            .then_some(multi_exec.len() >= 2),
                    },
                    &theme,
                    cx,
                ),
            );
        }
        if drop.as_ref().is_some_and(|drop| drop.target.is_none()) {
            row = row.child(drop_line(&theme).flex_none().my(px(7.)).ml(px(-1.)));
        }
        row = row.child(div().flex_1().min_w(px(4.)));

        let mut tabs_area = div().relative().flex().flex_1().min_w_0().child(row);
        if scroll_state.can_scroll_left {
            let scroll = scroll.clone();
            tabs_area = tabs_area.child(earlier_tabs_button(&theme, move |window, cx| {
                let width = muxus_ui::zoom::unzoom(scroll.bounds().size.width);
                scroll_row_by(&scroll, -scroll_step(width));
                window.refresh();
                let _ = cx;
            }));
        }
        if scroll_state.can_scroll_right {
            let scroll = scroll.clone();
            tabs_area = tabs_area.child(div().absolute().top(px(5.)).right(px(2.)).child(
                later_tabs_button(&theme, move |window, cx| {
                    let width = muxus_ui::zoom::unzoom(scroll.bounds().size.width);
                    scroll_row_by(&scroll, scroll_step(width));
                    window.refresh();
                    let _ = cx;
                }),
            ));
        }
        strip = strip.child(tabs_area);

        let pane = pane_id.to_string();
        let mut actions = div()
            .flex()
            .flex_none()
            .items_center()
            .pl(px(2.))
            .border_l_1()
            .border_color(p.divider)
            .bg(p.sidebar)
            .shadow(vec![BoxShadow {
                color: alpha(p.sidebar, 0.72),
                offset: point(px(-8.), px(0.)),
                blur_radius: px(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(
                IconButton::new(ElementId::Name(format!("new-tab-{pane_id}").into()), "Add")
                    .tooltip(with_chord("New tab", "tab.new"))
                    .on_click(cx.listener({
                        let pane = pane.clone();
                        move |this, _, _, cx| {
                            this.tabs.focus_pane(&pane);
                            this.tabs.open_empty();
                            this.tabs_changed(cx);
                        }
                    })),
            );
        if tabs.len() > 1 {
            let count = tabs.len();
            actions = actions.child(
                IconButton::new(
                    ElementId::Name(format!("all-tabs-{pane_id}").into()),
                    "KeyboardArrowDown",
                )
                .ink(if scroll_state.overflow {
                    p.text.primary
                } else {
                    p.text.secondary
                })
                .tooltip(format!("Show all {count} tabs"))
                .on_click(cx.listener({
                    let pane = pane.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        let position = event.mouse_position().unwrap_or_default();
                        this.menu = Some(ShellMenu::AllTabs {
                            pane_id: pane.clone(),
                            position,
                        });
                        cx.notify();
                    }
                })),
            );
        }
        actions = actions.child(vertical_divider(&theme, 6., 2.));
        if compact {
            actions = actions.child(
                IconButton::new(
                    ElementId::Name(format!("pane-more-{pane_id}").into()),
                    "MoreHoriz",
                )
                .tooltip("More pane actions")
                .on_click(cx.listener({
                    let pane = pane.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        this.menu = Some(ShellMenu::Pane {
                            pane_id: pane.clone(),
                            position: click_position(event),
                        });
                        cx.notify();
                    }
                })),
            );
        }
        if !compact {
            actions = actions
                .child(
                    IconButton::new(
                        ElementId::Name(format!("split-right-{pane_id}").into()),
                        "VerticalSplitOutlined",
                    )
                    .icon_size(17.)
                    .tooltip(with_chord("Split right", "pane.split.right"))
                    .on_click(cx.listener({
                        let pane = pane.clone();
                        move |this, _, _, cx| this.split_pane(&pane, PaneDirection::Right, cx)
                    })),
                )
                .child(
                    IconButton::new(
                        ElementId::Name(format!("split-down-{pane_id}").into()),
                        "HorizontalSplitOutlined",
                    )
                    .icon_size(17.)
                    .tooltip(with_chord("Split down", "pane.split.down"))
                    .on_click(cx.listener({
                        let pane = pane.clone();
                        move |this, _, _, cx| this.split_pane(&pane, PaneDirection::Down, cx)
                    })),
                );
        }
        if can_close_pane && !compact {
            actions = actions.child(
                IconButton::new(
                    ElementId::Name(format!("zoom-{pane_id}").into()),
                    if zoomed {
                        "CloseFullscreenOutlined"
                    } else {
                        "OpenInFullOutlined"
                    },
                )
                .icon_size(15.)
                .padding(6.)
                .ink(if zoomed {
                    p.primary.main
                } else {
                    p.action.active
                })
                .tooltip(with_chord(
                    if zoomed {
                        "Restore layout"
                    } else {
                        "Zoom pane"
                    },
                    "pane.zoom",
                ))
                .on_click(cx.listener({
                    let pane = pane.clone();
                    move |this, _, _, cx| {
                        this.tabs.toggle_zoom(Some(&pane));
                        cx.notify();
                    }
                })),
            );
        }
        if can_close_pane {
            actions = actions.child(
                div().mr(px(4.)).child(
                    IconButton::new(
                        ElementId::Name(format!("close-pane-{pane_id}").into()),
                        "Close",
                    )
                    .icon_size(16.)
                    .tooltip(with_chord("Close pane", "pane.close"))
                    .on_click(cx.listener({
                        let pane = pane.clone();
                        move |this, _, window, cx| {
                            this.request_close_pane(&pane, window, cx);
                        }
                    })),
                ),
            );
        }
        strip.child(actions).into_any_element()
    }

    fn active_tab_of(&self, pane_id: &str) -> Option<String> {
        let flat = flatten_pane_layout(&self.tabs.root);
        flat.panes
            .into_iter()
            .find(|entry| entry.pane.id == pane_id)
            .and_then(|entry| entry.pane.active_tab_id)
    }

    fn split_pane(&mut self, pane_id: &str, direction: PaneDirection, cx: &mut Context<Self>) {
        self.tabs.focus_pane(pane_id);
        self.tabs
            .split_active_pane(direction, self.prefs.split_inherits_session);
        self.tabs_changed(cx);
    }

    fn render_tab(
        &mut self,
        tab: &TerminalTab,
        look: TabLook,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let TabLook {
            active,
            width,
            focused,
            number,
            indicator,
            mirrored,
        } = look;
        let p = theme.palette;
        let unread = self.tabs.unread_output_ids.contains(&tab.id);
        let tab_icon_name = tab_icon(tab);
        let icon_ink = if unread {
            p.info.main
        } else if active && focused {
            p.primary.main
        } else {
            p.text.secondary
        };
        let label_ink = if active {
            p.text.primary
        } else if unread {
            p.info.main
        } else {
            p.text.secondary
        };
        let top_border = tab
            .color
            .as_deref()
            .and_then(try_hex)
            .unwrap_or(Hsla::transparent_black());
        let dot_ring = if active {
            p.background_default
        } else {
            p.sidebar
        };
        let group: SharedString = format!("tab-{}", tab.id).into();

        // Tab numbers stand in for the icons while Alt is held (or always).
        let show_number = match self.prefs.tab_number_visibility {
            TabNumberVisibility::Always => true,
            TabNumberVisibility::Shortcut => self.alt_held,
        } && number.is_some();
        let mut icon_box = div().relative().flex().flex_none().w(px(18.)).h(px(15.));
        if show_number {
            icon_box = icon_box.justify_center().child(
                label(number.unwrap_or_default().to_string())
                    .size(11.)
                    .weight(600.)
                    .line_height(15.)
                    .tabular_nums()
                    .color(p.text.primary)
                    .no_truncate(),
            );
        } else {
            icon_box = icon_box.child(icon(tab_icon_name, 15., icon_ink));
        }
        if let Some(status) = (tab.profile.is_some())
            .then(|| status_dot(tab.status))
            .flatten()
        {
            // 5px dot at right 1 / bottom 1 of the icon box, ringed 1px in
            // the color behind it.
            icon_box = icon_box.child(
                div()
                    .absolute()
                    .left(px(11.))
                    .top(px(8.))
                    .size(px(7.))
                    .rounded(px(3.5))
                    .bg(dot_ring)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(5.))
                            .rounded(px(2.5))
                            .bg(theme.status_text_color(status)),
                    ),
            );
        }
        if unread {
            icon_box = icon_box.child(
                div()
                    .absolute()
                    .left(px(11.))
                    .top(px(-1.))
                    .size(px(7.))
                    .rounded(px(3.5))
                    .bg(dot_ring)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(5.)).rounded(px(2.5)).bg(p.info.main)),
            );
        }

        let id = tab.id.clone();
        let close_id = tab.id.clone();
        let menu_id = tab.id.clone();
        let middle_id = tab.id.clone();
        let drop_target = tab.id.clone();
        let drop_pane = tab.pane_id.clone();
        let bounds_id = tab.id.clone();
        let geometry = self.geometry.clone();
        let dragged = DraggedTab {
            tab_id: tab.id.clone(),
            title: tab.title.clone().into(),
            icon: tab_icon_name,
        };
        // The status shows on the icon, except for a tab that has not connected yet.
        let mut status_icon = div()
            .id(ElementId::Name(format!("tab-icon-{}", tab.id).into()))
            .flex()
            .child(icon_box);
        if tab.status != TabStatus::Idle {
            status_icon = status_icon.tooltip(Tooltip::text(tab_status_label(tab)));
        }

        let mut el = div()
            .id(ElementId::Name(format!("tab-{}", tab.id).into()))
            .group(group.clone())
            .relative()
            // Sliding to a new place (FLIP): offset from its slot, easing to 0.
            .left(px(self.tab_slide_offset(&tab.pane_id, &tab.id)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .w(px(width))
            // The 2px top border carries the tab's color flag (transparent
            // without one); the right border is the divider between tabs.
            .pt(px(2.))
            .border_r_1()
            .border_color(p.divider)
            // A tab stop (`tabIndex={0}`): Enter or Space activates it; the
            // pointer leaves the keyboard where it was (the terminal).
            .tab_stop(true)
            .focusable()
            .focus_visible({
                let ring = alpha(p.text.primary, 0.35);
                move |s| {
                    s.shadow(vec![BoxShadow {
                        color: ring,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                        inset: true,
                    }])
                }
            })
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(2.))
                    .bg(top_border),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        geometry.tabs.borrow_mut().insert(bounds_id.clone(), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(status_icon)
            // Flat weight on purpose: the underline, icon tint and lifted
            // background already mark the active tab, and a weight jump would
            // re-measure the title on every switch.
            .child(
                div()
                    .id(ElementId::Name(format!("tab-title-{}", tab.id).into()))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .tooltip(Tooltip::text(tab.title.clone()))
                    .child(
                        label(tab.title.clone())
                            .size(13.)
                            .weight(450.)
                            .line_height(18.59)
                            .tracking(-0.1)
                            .color(label_ink)
                            .nudge(-0.5)
                            .grow(),
                    ),
            );
        if tab.pinned {
            el = el.child(svg_pin(13., p.text.secondary));
        }
        if let Some(progress) = self.paste_progress(&tab.id) {
            el = el.child(tab_paste_progress(&tab.id, progress, theme, cx));
        }
        if let Some(badge) = self.program_status_badge(&tab.id, theme) {
            el = el.child(badge);
        }
        if let Some(login) = &tab.login_sequence {
            el = el.child(
                div()
                    .id(ElementId::Name(format!("tab-login-{}", tab.id).into()))
                    .flex()
                    .flex_none()
                    .tooltip(Tooltip::text(format!(
                        "Login sequence, step {} of {}: {}",
                        login.step, login.steps, login.detail
                    )))
                    .child(icon("LoginOutlined", 14., p.info.main).with_animation(
                        ElementId::Name(format!("tab-login-pulse-{}", tab.id).into()),
                        Animation::new(std::time::Duration::from_millis(1200)).repeat(),
                        // `muxus-pulse`: down to 0.35 halfway, ease-in-out.
                        |icon, delta| {
                            let wave = (1. - (delta * std::f32::consts::TAU).cos()) / 2.;
                            icon.opacity(1. - 0.65 * wave)
                        },
                    )),
            );
        }
        if let Some(mirroring) = mirrored {
            el = el.child(
                div()
                    .id(ElementId::Name(format!("tab-multi-exec-{}", tab.id).into()))
                    .flex()
                    .flex_none()
                    .tooltip(Tooltip::text(if mirroring {
                        "Input is mirrored with this terminal"
                    } else {
                        "Selected for multi-execution"
                    }))
                    .child(icon(
                        "PodcastsOutlined",
                        14.,
                        if mirroring {
                            p.warning.main
                        } else {
                            p.action.disabled
                        },
                    )),
            );
        }
        let close = IconButton::new(
            ElementId::Name(format!("close-tab-{}", tab.id).into()),
            "Close",
        )
        .icon_size(14.)
        .padding(2.)
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.request_close_tabs(vec![close_id.clone()], window, cx);
        }));
        el = if active {
            el.child(close)
        } else {
            el.child(
                div()
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .child(close),
            )
        };
        if active {
            el = el.bg(p.background_default);
            if focused {
                el = el.bg(linear_gradient(
                    180.,
                    linear_color_stop(over(alpha(p.primary.main, 0.1), p.background_default), 0.),
                    linear_color_stop(p.background_default, 0.72),
                ));
                // The underline: 2px in the primary color, 10px in from each side,
                // with a soft glow.
                el = el.child(
                    div()
                        .absolute()
                        .left(px(10.))
                        .right(px(10.))
                        .bottom(px(0.))
                        .h(px(2.))
                        .rounded_t(px(2.))
                        .bg(if unread { p.info.main } else { p.primary.main })
                        .shadow(vec![BoxShadow {
                            color: alpha(p.primary.main, 0.32),
                            offset: point(px(0.), px(-1.)),
                            blur_radius: px(8.),
                            spread_radius: px(0.),
                            inset: false,
                        }]),
                );
            }
        } else if unread {
            el = el.bg(alpha(p.info.main, 0.12));
        }
        // A tab from another pane or window lands on this side of it.
        if let Some(edge) = indicator {
            let line = drop_line(theme).absolute().top(px(7.)).bottom(px(7.));
            el = el.child(match edge {
                Edge::Before => line.left(px(-1.)),
                Edge::After => line.right(px(-1.)),
            });
        }
        // The strip copy stays in place, dimmed, while its drag image follows
        // the pointer, the way browser tabs behave.
        if self.tab_motion.dragging.as_deref() == Some(tab.id.as_str()) {
            el = el.opacity(0.4);
        }
        let shell = cx.entity().downgrade();
        el.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.tabs.activate(&id);
                cx.notify();
                // A double-click renames the tab.
                if event.click_count() == 2 {
                    this.open_rename_tab(&id, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    this.request_close_tabs(vec![middle_id.clone()], window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some(ShellMenu::Tab {
                        tab_id: menu_id.clone(),
                        position: event.position,
                    });
                    cx.notify();
                }),
            )
            .on_drag(dragged, move |dragged, _, _, cx| {
                let _ = shell.update(cx, |shell, cx| shell.tab_drag_started(&dragged.tab_id, cx));
                cx.new(|_| DraggedTabView(dragged.clone()))
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                let edge = geometry_edge(&this.geometry, &drop_target, window);
                this.drop_tab(&dragged.tab_id, &drop_pane, Some(&drop_target), edge, cx);
            }))
            .into_any_element()
    }

    pub fn pane_menu(&mut self, pane_id: &str, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let can_close = matches!(
            self.tabs.root,
            muxus_app::state::workspace_layout::PaneNode::Split(_)
        );
        let zoomed = self.tabs.zoomed_pane_id.as_deref() == Some(pane_id);
        let overrides = self.keybinding_overrides();
        let chord = |command: &str| muxus_app::keymap::hints::chord_label(command, &overrides);
        let item = |id: &'static str,
                    label: &'static str,
                    glyph: &'static str,
                    direction: PaneDirection,
                    command: &str,
                    cx: &mut Context<Self>| {
            let pane = pane_id.to_string();
            // Split left and up mirror the right and down glyphs.
            let mirror = match direction {
                PaneDirection::Left => Some(size(-1., 1.)),
                PaneDirection::Up => Some(size(1., -1.)),
                _ => None,
            };
            let ink = theme.palette.action.active;
            let item = MenuItem::new(id, label);
            let item = match mirror {
                Some(scale) => item.leading(
                    icon(glyph, muxus_ui::icon::SMALL, ink)
                        .with_transformation(Transformation::scale(scale)),
                ),
                None => item.icon(glyph),
            };
            item.chord(chord(command))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.split_pane(&pane, direction, cx);
                }))
        };
        let pane = pane_id.to_string();
        menu::paper(&theme)
            .child(item(
                "split-right",
                "Split right",
                "VerticalSplitOutlined",
                PaneDirection::Right,
                "pane.split.right",
                cx,
            ))
            .child(item(
                "split-down",
                "Split down",
                "HorizontalSplitOutlined",
                PaneDirection::Down,
                "pane.split.down",
                cx,
            ))
            .child(item(
                "split-left",
                "Split left",
                "VerticalSplitOutlined",
                PaneDirection::Left,
                "pane.split.left",
                cx,
            ))
            .child(item(
                "split-up",
                "Split up",
                "HorizontalSplitOutlined",
                PaneDirection::Up,
                "pane.split.up",
                cx,
            ))
            .child(menu_divider(&theme))
            .child(
                MenuItem::new(
                    "zoom",
                    if zoomed {
                        "Restore layout"
                    } else {
                        "Zoom pane"
                    },
                )
                .icon(if zoomed {
                    "CloseFullscreenOutlined"
                } else {
                    "OpenInFullOutlined"
                })
                .chord(chord("pane.zoom"))
                .disabled(!can_close)
                .on_click(cx.listener({
                    let pane = pane.clone();
                    move |this, _, _, cx| {
                        this.tabs.toggle_zoom(Some(&pane));
                        this.menu = None;
                        cx.notify();
                    }
                })),
            )
            .child(
                MenuItem::new("even-out", "Even out panes")
                    .icon("GridViewOutlined")
                    .disabled(!can_close)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tabs.equalize_panes();
                        this.menu = None;
                        cx.notify();
                    })),
            )
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("close-pane", "Close pane")
                    .icon("Close")
                    .chord(chord("pane.close"))
                    .disabled(!can_close)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.request_close_pane(&pane, window, cx);
                    })),
            )
    }

    pub fn all_tabs_menu(&mut self, pane_id: &str, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let active = self.active_tab_of(pane_id);
        // `maxHeight: min(460px, calc(100vh - 48px))`; the list scrolls inside.
        let viewport = muxus_ui::zoom::unzoom(window.viewport_size().height);
        let max_height = (viewport - 48.).min(460.) - 16.;
        let mut items = div()
            .id("all-tabs-items")
            .flex()
            .flex_col()
            .max_h(px(max_height.max(48.)))
            .overflow_y_scroll();
        for tab in self
            .tabs
            .pane_tabs(pane_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            let id = tab.id.clone();
            let is_active = active.as_deref() == Some(tab.id.as_str());
            let badge = self
                .program_status_badge(&tab.id, &theme)
                .map(|badge| div().ml(px(8.)).child(badge));
            let check = is_active.then(|| {
                div()
                    .when(badge.is_some(), |check| check.ml(px(4.)))
                    .child(icon("Check", 18., theme.palette.action.active))
            });
            let pin = tab.pinned.then(|| {
                div()
                    .ml(px(8.))
                    .child(svg_pin(14., theme.palette.text.secondary))
            });
            let unread = self.tabs.unread_output_ids.contains(&tab.id);
            let glyph_ink = if unread {
                theme.palette.info.main
            } else if is_active {
                theme.palette.primary.main
            } else {
                theme.palette.action.active
            };
            items = items.child(
                MenuItem::new(
                    ElementId::Name(format!("all-tabs-{}", tab.id).into()),
                    tab.title.clone(),
                )
                .detail(tab_status_label(&tab))
                .icon(tab_icon(&tab))
                .icon_color(glyph_ink)
                .selected(is_active)
                .trailing(
                    div()
                        .flex()
                        .items_center()
                        .children(badge)
                        .children(pin)
                        .children(check)
                        .into_any_element(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tabs.activate(&id);
                    this.menu = None;
                    cx.notify();
                })),
            );
        }
        menu::paper(&theme).w(px(320.)).child(items)
    }
}

/// The gap between the empty pane's cards.
const EMPTY_CARD_GAP: f32 = 8.;
/// The narrowest a card gets before the cards wrap (`minmax(132px, 1fr)`).
const EMPTY_CARD_MIN: f32 = 132.;

/// How many columns `repeat(auto-fit, minmax(132px, 1fr))` makes of
/// `available` px for `cards` cards (empty tracks collapse, so never more).
fn empty_pane_columns(available: f32, cards: usize) -> usize {
    let fit = ((available + EMPTY_CARD_GAP) / (EMPTY_CARD_MIN + EMPTY_CARD_GAP)).floor() as usize;
    fit.clamp(1, cards.max(1))
}

/// One `EmptyAction` card: a tab stop (Enter or Space presses it) that lifts
/// a pixel under the pointer.
#[allow(clippy::too_many_arguments)]
fn empty_action(
    id: &'static str,
    glyph: &'static str,
    title: &'static str,
    description: &'static str,
    primary: bool,
    width: f32,
    record: Option<(std::rc::Rc<PaneGeometry>, String)>,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let p = theme.palette;
    let (bg, fg, border, tile, secondary) = if primary {
        (
            p.primary.main,
            p.primary.contrast_text,
            p.primary.main,
            theme.components.on_primary_tile,
            theme.components.on_primary_secondary,
        )
    } else {
        (
            p.background_paper,
            p.text.primary,
            p.divider,
            p.action.hover,
            p.text.secondary,
        )
    };
    let caption = theme.typography.caption;
    let hover_border = p.primary.main;
    let lift = muxus_ui::widgets::button::elevation(2, theme);
    let ring = p.primary.main;
    let mut card = div()
        .id(id)
        .relative()
        .w(px(width))
        .min_h(px(92.))
        .p(px(12.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_1()
        .border_color(border)
        .rounded(px(12.))
        .bg(bg)
        .text_color(fg)
        .cursor_pointer()
        .tab_stop(true)
        .focusable()
        // `outline: 2px solid primary; outline-offset: 2px` for keyboard focus.
        .focus_visible(move |s| {
            s.shadow(vec![BoxShadow {
                color: ring,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(4.),
                inset: false,
            }])
        })
        .hover(move |s| {
            s.border_color(hover_border)
                .top(px(-1.))
                .shadow(lift.clone())
        })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default());
    if let Some((geometry, pane)) = record {
        card = card.child(
            canvas(
                move |bounds, _, _| {
                    geometry
                        .picker_cards
                        .borrow_mut()
                        .insert(pane.clone(), bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );
    }
    card.child(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(34.))
            .rounded(px(8.))
            .bg(tile)
            .child(icon(glyph, 20., fg)),
    )
    .child(
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(
                label(title)
                    .size(13.)
                    .weight(650.)
                    .line_height(18.59)
                    .color(fg),
            )
            .child(
                div().mt(px(1.2)).child(
                    paragraph(description)
                        .size(caption.size)
                        .line_height(caption.line_height)
                        .color(secondary),
                ),
            ),
    )
    .on_click(on_click)
    .into_any_element()
}

/// How a tab in the strip looks this frame.
#[derive(Clone, Copy)]
struct TabLook {
    active: bool,
    /// `clamp(168px, 20vw, 210px)` active, `clamp(128px, 15vw, 190px)` otherwise.
    width: f32,
    /// Its pane has the focus.
    focused: bool,
    /// Its number among the window's tabs (Alt+number).
    number: Option<usize>,
    /// A tab from elsewhere would land on this side of it.
    indicator: Option<Edge>,
    /// Selected for multi-execution: `Some(true)` while input is mirrored.
    mirrored: Option<bool>,
}

/// The insertion line a dragged tab lands at: 2px of the primary color with a glow.
fn drop_line(theme: &Theme) -> Div {
    let primary = theme.palette.primary.main;
    div()
        .w(px(2.))
        .rounded(px(1.))
        .bg(primary)
        .shadow(vec![BoxShadow {
            color: alpha(primary, 0.55),
            offset: point(px(0.), px(0.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
            inset: false,
        }])
}

/// Which side of tab `target` the pointer is on (`dropEdge`).
fn geometry_edge(geometry: &PaneGeometry, target: &str, window: &Window) -> Edge {
    geometry
        .tabs
        .borrow()
        .get(target)
        .map(|bounds| {
            if window.mouse_position().x < bounds.center().x {
                Edge::Before
            } else {
                Edge::After
            }
        })
        .unwrap_or(Edge::After)
}

/// Where a menu opened by a click goes: the pointer, or below the control
/// pressed with the keyboard.
fn click_position(event: &ClickEvent) -> Point<Pixels> {
    match event {
        ClickEvent::Keyboard(keyboard) => keyboard.bounds.bottom_left(),
        _ => event.mouse_position().unwrap_or_default(),
    }
}

/// The framed surface a session view stands on, empty, for the frame
/// before the tab's terminal or session view is created.
fn session_placeholder(tab: &TerminalTab, theme: &Theme) -> AnyElement {
    let p = theme.palette;
    let surface = theme.components.session_surface;
    let _ = tab;
    div()
        .size_full()
        .pt(px(6.))
        .px(px(8.))
        .pb(px(8.))
        .child(
            div()
                .size_full()
                .rounded(px(8.))
                .border_1()
                .border_color(p.divider)
                .bg(surface)
                .overflow_hidden(),
        )
        .into_any_element()
}

/// `PinIcon`: the pin glyph TabStrip.tsx draws (assets/icons/TabPin.svg).
fn svg_pin(size: f32, color: Hsla) -> impl IntoElement {
    icon("TabPin", size, color)
}

#[allow(dead_code)]
fn _types(_: Point<Pixels>, _: FontWeight, _: Rc<()>) {}
