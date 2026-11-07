//! Tabs on the move (`TabStrip.tsx`, `tab-drag.ts`, `tab-transfer.ts`,
//! `electron/src/tab-detach.ts`): a dragged tab resorts its own strip live
//! and shows where it would land in another pane; let go outside the window,
//! it moves to the Muxus window under the pointer, or to a window of its own;
//! tabs slide to their new places when the order changes; and a strip scrolls
//! its active tab into view.
//!
//! A drag never leaves the window it started in (the pointer stays grabbed
//! by it), so the window under the pointer is found from the window bounds.
//! Wayland does not tell a window where it is on the screen: there a tab let
//! go outside its window always gets a window of its own.

use std::collections::HashMap;
use std::time::Instant;

use muxus_app::features::notices::tab_strip::{
    SLIDE_SETTLE, ScreenRect, TabSlides, point_inside_any_window, reveal_offset,
    should_detach_tab_drag,
};
use muxus_app::state::tabs::{Edge, TabPatch, TerminalTab};
use muxus_app::state::workspace_layout::flatten_pane_layout;
use muxus_ui::gpui_kit::{
    AnyElement, Bounds, Context, DispatchPhase, Entity, IntoElement, MouseButton, MouseUpEvent,
    Pixels, Point, ScrollHandle, Styled, Window, canvas, point, px,
};

use crate::ui::shell::AppShell;

/// Where a tab dragged from another pane (or window) would land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabDropIndicator {
    pub pane_id: String,
    /// Beside this tab, or past the strip's last tab (`None`).
    pub target: Option<String>,
    pub edge: Edge,
}

/// The window's tabs in motion.
#[derive(Default)]
pub struct TabMotion {
    /// The tab being dragged, from the moment the drag starts until the button is let go.
    pub dragging: Option<String>,
    /// The insertion line for a drop arriving from another pane.
    pub drop: Option<TabDropIndicator>,
    /// Each pane's slides (FLIP).
    pub slides: HashMap<String, TabSlides>,
    /// Live resorting waits for the slides to settle until then.
    pub settle_until: Option<Instant>,
    /// What each strip last scrolled into view: its active tab and order.
    revealed: HashMap<String, String>,
}

/// A tab's width in a window `viewport` CSS px wide: `clamp(168px, 20vw, 210px)`
/// active, `clamp(128px, 15vw, 190px)` otherwise.
pub fn tab_width(active: bool, viewport: f32) -> f32 {
    if active {
        (viewport * 0.2).clamp(168., 210.)
    } else {
        (viewport * 0.15).clamp(128., 190.)
    }
}

/// The window's width in CSS px, without the frame's shadow margin.
pub fn viewport_width(window: &Window) -> f32 {
    let inset = window.client_inset().map_or(0., f32::from);
    muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * inset
}

#[cfg(test)]
mod tests {
    use super::tab_width;

    #[test]
    fn tabs_narrow_with_the_window_between_their_bounds() {
        assert_eq!(tab_width(true, 1440.), 210.);
        assert_eq!(tab_width(false, 1440.), 190.);
        assert_eq!(tab_width(true, 900.), 180.);
        assert_eq!(tab_width(false, 1000.), 150.);
        assert_eq!(tab_width(true, 600.), 168.);
        assert_eq!(tab_width(false, 600.), 128.);
    }
}

impl AppShell {
    /// Take a pane's order for this frame; tabs that moved start sliding.
    /// `true` while slides still run (the window asks for another frame).
    pub fn update_tab_slides(&mut self, pane_id: &str, order: Vec<(String, f32)>) -> bool {
        let now = Instant::now();
        let slides = self
            .tab_motion
            .slides
            .entry(pane_id.to_string())
            .or_default();
        if slides.update(order, now) {
            self.tab_motion.settle_until = Some(now + SLIDE_SETTLE);
        }
        slides.running(now)
    }

    /// A tab's offset from its slot while it slides.
    pub fn tab_slide_offset(&self, pane_id: &str, tab_id: &str) -> f32 {
        self.tab_motion
            .slides
            .get(pane_id)
            .map_or(0., |slides| slides.offset(tab_id, Instant::now()))
    }

    /// `revealTab`: when a strip's active tab or order changed, scroll the row
    /// so the active tab shows with room to spare.
    pub fn reveal_active_tab(
        &mut self,
        pane_id: &str,
        active: Option<&str>,
        scroll: &ScrollHandle,
    ) {
        let Some(active) = active else {
            return;
        };
        let Some(slides) = self.tab_motion.slides.get(pane_id) else {
            return;
        };
        let key = format!("{active}\n{:?}", slides.slot(active));
        let order_key = format!(
            "{key}\n{}",
            self.tabs
                .pane_tabs(pane_id)
                .iter()
                .map(|tab| tab.id.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        if self.tab_motion.revealed.get(pane_id) == Some(&order_key) {
            return;
        }
        let client_width = muxus_ui::zoom::unzoom(scroll.bounds().size.width);
        // Not laid out yet: try again next frame.
        if client_width <= 0. {
            return;
        }
        self.tab_motion
            .revealed
            .insert(pane_id.to_string(), order_key);
        let Some((left, width)) = slides.slot(active) else {
            return;
        };
        let scroll_left = -muxus_ui::zoom::unzoom(scroll.offset().x);
        if let Some(next) = reveal_offset(scroll_left, client_width, 0., left, width) {
            // GPUI clamps the offset to the row once it is laid out.
            let offset = scroll.offset();
            scroll.set_offset(point(px(-next.max(0.)), offset.y));
        }
    }

    /// The drag of a tab started.
    pub fn tab_drag_started(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        self.tab_motion.dragging = Some(tab_id.to_string());
        cx.notify();
    }

    fn set_tab_drop(&mut self, drop: Option<TabDropIndicator>, cx: &mut Context<Self>) {
        if self.tab_motion.drop != drop {
            self.tab_motion.drop = drop;
            cx.notify();
        }
    }

    /// A dragged tab moved over a pane's strip (`onDragOver` on the strip and
    /// its tabs). Same-strip drags resort live, browser style, the moving tab
    /// being its own indicator; a tab from another pane shows where it lands.
    pub fn tab_dragged_over(
        &mut self,
        pane_id: &str,
        dragged_id: &str,
        position: Point<Pixels>,
        strip: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !strip.contains(&position) {
            if self
                .tab_motion
                .drop
                .as_ref()
                .is_some_and(|drop| drop.pane_id == pane_id)
            {
                self.set_tab_drop(None, cx);
            }
            return;
        }
        let Some(dragged) = self.tabs.tab(dragged_id).cloned() else {
            return;
        };
        let same_pane = dragged.pane_id == pane_id;
        let settled = self
            .tab_motion
            .settle_until
            .is_none_or(|until| Instant::now() >= until);
        let hovered = {
            let bounds = self.geometry.tabs.borrow();
            self.tabs.pane_tabs(pane_id).into_iter().find_map(|tab| {
                let tab_bounds = bounds.get(&tab.id)?;
                tab_bounds.contains(&position).then(|| {
                    let edge = if position.x < tab_bounds.center().x {
                        Edge::Before
                    } else {
                        Edge::After
                    };
                    (tab.id.clone(), tab.pinned, edge)
                })
            })
        };
        match hovered {
            Some((target, pinned, edge)) => {
                if same_pane {
                    // A pinned tab stays among the pinned ones, and the others among theirs.
                    if dragged.pinned != pinned {
                        return;
                    }
                    self.set_tab_drop(None, cx);
                    if target != dragged.id
                        && settled
                        && self.tabs.reorder_tab(&dragged.id, &target, edge)
                    {
                        self.tabs_changed(cx);
                    }
                } else {
                    self.set_tab_drop(
                        Some(TabDropIndicator {
                            pane_id: pane_id.to_string(),
                            target: Some(target),
                            edge,
                        }),
                        cx,
                    );
                }
            }
            None if same_pane => {
                self.set_tab_drop(None, cx);
                // Hovering past the last tab sends a same-strip drag to the end of its group.
                let past_last = self
                    .tabs
                    .pane_tabs(pane_id)
                    .last()
                    .and_then(|tab| self.geometry.tabs.borrow().get(&tab.id).copied())
                    .is_some_and(|last| position.x >= last.right());
                if past_last
                    && settled
                    && self
                        .tabs
                        .move_tab_to_pane(&dragged.id, pane_id, None, Edge::After)
                {
                    self.tabs_changed(cx);
                }
            }
            None => self.set_tab_drop(
                Some(TabDropIndicator {
                    pane_id: pane_id.to_string(),
                    target: None,
                    edge: Edge::After,
                }),
                cx,
            ),
        }
    }

    /// A tab was dropped on a strip: beside `target`, or at the strip's end.
    pub fn drop_tab(
        &mut self,
        tab_id: &str,
        pane_id: &str,
        target: Option<&str>,
        edge: Edge,
        cx: &mut Context<Self>,
    ) {
        self.tab_motion.drop = None;
        if target == Some(tab_id) {
            cx.notify();
            return;
        }
        let same_pane = self.tabs.tab(tab_id).map(|tab| tab.pane_id.as_str()) == Some(pane_id);
        if same_pane
            && let Some(target) = target
            && self.tabs.tab(target).map(|tab| tab.pinned)
                != self.tabs.tab(tab_id).map(|tab| tab.pinned)
        {
            cx.notify();
            return;
        }
        if self.tabs.move_tab_to_pane(tab_id, pane_id, target, edge) {
            self.tabs_changed(cx);
        } else {
            cx.notify();
        }
    }

    /// While a tab is dragged: a window-wide listener for the button's release,
    /// wherever it happens (the pointer may have left the window).
    pub fn render_tab_drag_listener(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.tab_motion.dragging.as_ref()?;
        let shell = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let shell = shell.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        let position = event.position;
                        let _ = shell
                            .update(cx, |shell, cx| shell.tab_drag_ended(position, window, cx));
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    /// The button was let go. Inside the window the strips took the drop (or
    /// nothing did); outside, the tab goes to the Muxus window under the
    /// pointer, or into a window of its own (`detachTabToNewWindow`).
    fn tab_drag_ended(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tab_motion.dragging.take() else {
            return;
        };
        self.tab_motion.drop = None;
        cx.notify();
        let viewport = window.viewport_size();
        let here = ScreenRect {
            x: 0.,
            y: 0.,
            width: f32::from(viewport.width),
            height: f32::from(viewport.height),
        };
        if !should_detach_tab_drag(false, f32::from(position.x), f32::from(position.y), here) {
            return;
        }
        let Some(tab) = self.tabs.tab(&tab_id).cloned() else {
            return;
        };
        match window_under_pointer(position, window, cx) {
            Some((target, local)) => self.move_tab_to_window(&tab, &target, local, cx),
            None => self.detach_tab(&tab, cx),
        }
    }

    /// A tab dragged out of the window: a window of its own, the live session moving along.
    fn detach_tab(&mut self, tab: &TerminalTab, cx: &mut Context<Self>) {
        if tab.profile.is_some() {
            self.open_tab_in_new_window(tab, true, cx);
        } else {
            crate::ui::open_main_window(crate::ui::MainWindowLaunch::Blank, cx);
            self.tabs.close(&tab.id);
            self.tabs_changed(cx);
        }
    }

    /// A tab let go over another Muxus window: it moves there (`receiveTabTransfer`),
    /// the live session along with it.
    fn move_tab_to_window(
        &mut self,
        tab: &TerminalTab,
        target: &Entity<AppShell>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let transfer = self.detach_terminal(&tab.id, cx);
        let moved = tab.clone();
        target.update(cx, |shell, cx| {
            shell.adopt_tab(moved, transfer, position, cx)
        });
        self.tabs.close(&tab.id);
        self.tabs_changed(cx);
        if let Some(handle) = target.read(cx).window_handle {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    }

    /// `adoptTransferredTab`: a tab from another window, placed where it was let
    /// go (beside the tab under `position`, else at the end of the strip or the
    /// pane under it, else the active pane), active.
    pub fn adopt_tab(
        &mut self,
        tab: TerminalTab,
        transfer: Option<(String, String)>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let id = match tab.profile.clone() {
            Some(profile) => self.tabs.open(profile, tab.title.clone()),
            None => self.tabs.open_empty(),
        };
        if let Some(transfer) = transfer {
            self.transfers.insert(id.clone(), transfer);
        }
        self.tabs.update(
            &id,
            TabPatch {
                title: Some(tab.title.clone()),
                color: Some(tab.color.clone()),
                ..Default::default()
            },
        );
        if tab.pinned {
            self.tabs.set_pinned(&id, true);
        }
        let (pane, target, edge) = self.drop_place(position);
        if let Some(pane) = pane {
            self.tabs
                .move_tab_to_pane(&id, &pane, target.as_deref(), edge);
        }
        self.tabs.activate(&id);
        self.tabs_changed(cx);
    }

    /// The pane, and the tab beside which, a tab let go at `position` lands.
    fn drop_place(&self, position: Point<Pixels>) -> (Option<String>, Option<String>, Edge) {
        if let Some((id, bounds)) = self
            .geometry
            .tabs
            .borrow()
            .iter()
            .find(|(id, bounds)| bounds.contains(&position) && self.tabs.tab(id).is_some())
        {
            let edge = if position.x < bounds.center().x {
                Edge::Before
            } else {
                Edge::After
            };
            let pane = self.tabs.tab(id).map(|tab| tab.pane_id.clone());
            return (pane, Some(id.clone()), edge);
        }
        let canvas = self.geometry.canvas.get();
        if canvas.size.width > px(0.) && canvas.size.height > px(0.) {
            let x =
                f64::from(f32::from(position.x - canvas.origin.x) / f32::from(canvas.size.width));
            let y =
                f64::from(f32::from(position.y - canvas.origin.y) / f32::from(canvas.size.height));
            let flat = flatten_pane_layout(&self.tabs.root);
            if let Some(entry) = flat.panes.iter().find(|entry| {
                let rect = entry.rect;
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
            }) {
                return (Some(entry.pane.id.clone()), None, Edge::After);
            }
        }
        (None, None, Edge::After)
    }
}

/// The other Muxus window under a pointer at `position` in `window`, and the
/// pointer's place in it. `None` where window positions are unknown (Wayland).
fn window_under_pointer(
    position: Point<Pixels>,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> Option<(Entity<AppShell>, Point<Pixels>)> {
    if cx.compositor_name() == "Wayland" {
        return None;
    }
    let origin = window.bounds().origin;
    let screen = origin + position;
    let this = cx.entity_id();
    for shell in crate::ui::features::shells(cx) {
        if shell.entity_id() == this {
            continue;
        }
        let Some(handle) = shell.read(cx).window_handle else {
            continue;
        };
        if !shell.read(cx).window_visible {
            continue;
        }
        let Ok(bounds) = handle.update(cx, |_, window, _| window.bounds()) else {
            continue;
        };
        let rect = ScreenRect {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        };
        if point_inside_any_window(f32::from(screen.x), f32::from(screen.y), &[rect]) {
            return Some((shell, screen - bounds.origin));
        }
    }
    None
}
