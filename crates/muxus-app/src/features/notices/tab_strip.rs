//! The rules behind the window's tab strip (`components/TabStrip.tsx`,
//! `tab-drag.ts`, `electron/src/tab-detach.ts`): the "Rename tab" dialog, the
//! scroll arrows that appear when a pane holds more tabs than fit ("Earlier
//! tabs" / "Later tabs") and how the active tab is kept in view, the slide
//! that moves tabs to their new places when the order changes (FLIP), and
//! when a dragged tab leaves for a window of its own.
//!
//! The strip itself belongs to the window (`ui/panes.rs`). It keeps a
//! [`ScrollHandle`] on its tab row, asks [`TabScrollState::of`] each frame,
//! mounts [`earlier_tabs_button`] / [`later_tabs_button`] when the state says
//! so, calls [`reveal_offset`] after the active tab changes, starts
//! [`TabSlides`] when the order changes and opens a [`RenameTabDialog`] on a
//! tab's double-click or its "Rename tab" item.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, BoxShadow, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, Styled, Subscription, Window, div, point, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::dialog::{
    CloseHandler, DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::{Button, IconButton, TextField};

/// How close to an edge (CSS px) the active tab may sit before the strip
/// scrolls it further in.
pub const TAB_SCROLL_EDGE: f32 = 36.;
/// Overflow smaller than this (CSS px) does not count.
pub const TAB_SCROLL_TOLERANCE: f32 = 8.;

/// Whether the tab row overflows and which arrows to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabScrollState {
    /// Tabs do not fit; the all-tabs button turns from secondary to primary ink.
    pub overflow: bool,
    pub can_scroll_left: bool,
    pub can_scroll_right: bool,
}

impl TabScrollState {
    /// From the viewport's `scrollWidth`, `clientWidth` and `scrollLeft`.
    pub fn measure(scroll_width: f32, client_width: f32, scroll_left: f32) -> Self {
        let max_scroll_left = (scroll_width - client_width).max(0.);
        Self {
            overflow: max_scroll_left > TAB_SCROLL_TOLERANCE,
            can_scroll_left: scroll_left > TAB_SCROLL_TOLERANCE,
            can_scroll_right: scroll_left < max_scroll_left - TAB_SCROLL_TOLERANCE,
        }
    }

    /// From a GPUI scroll handle on the tab row (its offset is negative).
    pub fn of(handle: &ScrollHandle) -> Self {
        let client_width = muxus_ui::zoom::unzoom(handle.bounds().size.width);
        let max = muxus_ui::zoom::unzoom(handle.max_offset().x);
        let scroll_left = -muxus_ui::zoom::unzoom(handle.offset().x);
        Self::measure(client_width + max, client_width, scroll_left)
    }
}

/// How far one click on an arrow scrolls (`scrollBy`): 72% of the visible
/// row, at least 160px.
pub fn scroll_step(client_width: f32) -> f32 {
    (client_width * 0.72).max(160.)
}

/// A wheel over an overflowing row scrolls it sideways, by whichever axis
/// moved more.
pub fn wheel_delta(delta_x: f32, delta_y: f32) -> f32 {
    if delta_x.abs() > delta_y.abs() {
        delta_x
    } else {
        delta_y
    }
}

/// The scroll position that brings a tab into view with
/// [`TAB_SCROLL_EDGE`] to spare (`revealTab`), or `None` when it is already
/// visible. `reserved_right` is the width of the pane actions that cover the
/// row's right end.
pub fn reveal_offset(
    scroll_left: f32,
    client_width: f32,
    reserved_right: f32,
    tab_left: f32,
    tab_width: f32,
) -> Option<f32> {
    let visible_left = scroll_left + TAB_SCROLL_EDGE;
    let visible_right = scroll_left + client_width - reserved_right - TAB_SCROLL_EDGE;
    let tab_right = tab_left + tab_width;
    if tab_left < visible_left {
        Some((tab_left - TAB_SCROLL_EDGE).max(0.))
    } else if tab_right > visible_right {
        Some(tab_right - client_width + reserved_right + TAB_SCROLL_EDGE)
    } else {
        None
    }
}

/// Scroll a tab row by `delta` CSS px, clamped to its range.
pub fn scroll_row_by(handle: &ScrollHandle, delta: f32) {
    let max = muxus_ui::zoom::unzoom(handle.max_offset().x);
    let current = -muxus_ui::zoom::unzoom(handle.offset().x);
    let next = (current + delta).clamp(0., max.max(0.));
    let offset = handle.offset();
    handle.set_offset(point(px(-next), offset.y));
}

/// The arrows' fade shadow (`8px 0 14px`) and the earlier arrow's place in
/// the row (`top: 5, left: 2`), as `TabStrip.tsx` sets them.
const ARROW_SHADOW_X: f32 = 8.;
const ARROW_SHADOW_BLUR: f32 = 14.;
const EARLIER_ARROW_TOP: f32 = 5.;
const EARLIER_ARROW_LEFT: f32 = 2.;
/// The disc's opacity over the sidebar color and its shadow's.
const ARROW_DISC_ALPHA: f32 = 0.94;
const ARROW_SHADOW_ALPHA: f32 = 0.9;

/// One arrow's chrome: a small IconButton on a frosted sidebar-colored disc
/// whose shadow fades the tabs beneath it.
fn arrow_button(
    theme: &Theme,
    id: &'static str,
    glyph: &'static str,
    tooltip: &'static str,
    shadow_x: f32,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> muxus_ui::gpui_kit::Div {
    let sidebar = theme.palette.sidebar;
    div()
        .rounded_full()
        .bg(alpha(sidebar, ARROW_DISC_ALPHA))
        .shadow(vec![BoxShadow {
            color: alpha(sidebar, ARROW_SHADOW_ALPHA),
            offset: point(px(shadow_x), px(0.)),
            blur_radius: px(ARROW_SHADOW_BLUR),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            IconButton::new(id, glyph)
                .tooltip(tooltip)
                .on_click(move |_, window, cx| on_click(window, cx)),
        )
}

/// "Show earlier tabs": pinned to the row's left edge (5px down, 2px in).
pub fn earlier_tabs_button(
    theme: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .absolute()
        .top(px(EARLIER_ARROW_TOP))
        .left(px(EARLIER_ARROW_LEFT))
        .child(arrow_button(
            theme,
            "tabs-scroll-earlier",
            "KeyboardArrowLeft",
            "Earlier tabs",
            ARROW_SHADOW_X,
            on_click,
        ))
        .into_any_element()
}

/// "Show later tabs": placed by the strip just left of the pane actions
/// (`right: 100%` of the actions box), vertically centred.
pub fn later_tabs_button(
    theme: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    arrow_button(
        theme,
        "tabs-scroll-later",
        "KeyboardArrowRight",
        "Later tabs",
        -ARROW_SHADOW_X,
        on_click,
    )
    .into_any_element()
}

/// The title a rename commits: the trimmed value, or nothing when it is blank.
pub fn renamed_title(value: &str) -> Option<String> {
    let trimmed = muxus_core::js::js_trim(value);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// What the rename dialog decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTabEvent {
    /// Set this title on the tab (already trimmed).
    Rename { tab_id: String, title: String },
    /// Closed without a change (Cancel, Escape, the backdrop, or Enter on a
    /// blank value).
    Close,
}

/// `Dialog maxWidth="xs"` "Rename tab": one field holding the current title,
/// selected; Enter commits, Rename is disabled while the value is blank.
pub struct RenameTabDialog {
    tab_id: String,
    input: Entity<InputState>,
    focus: FocusHandle,
    _events: Subscription,
}

impl EventEmitter<RenameTabEvent> for RenameTabDialog {}

impl Focusable for RenameTabDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl RenameTabDialog {
    pub fn new(
        tab_id: impl Into<String>,
        title: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = title.into();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let events = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::PressEnter { .. } => this.commit(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        // `inputRef.select()` once the dialog is up (the next frame, as the
        // client's `requestAnimationFrame`): focusing places the caret first.
        let select = input.clone();
        window.on_next_frame(move |window, cx| {
            select.update(cx, |state, cx| state.select_all(window, cx));
        });
        Self {
            tab_id: tab_id.into(),
            input,
            focus: cx.focus_handle(),
            _events: events,
        }
    }

    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// `commitRename`: a blank value closes without renaming.
    fn commit(&mut self, cx: &mut Context<Self>) {
        match renamed_title(&self.value(cx)) {
            Some(title) => cx.emit(RenameTabEvent::Rename {
                tab_id: self.tab_id.clone(),
                title,
            }),
            None => cx.emit(RenameTabEvent::Close),
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(RenameTabEvent::Close);
        }
    }
}

impl Render for RenameTabDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let blank = renamed_title(&self.value(cx)).is_none();
        let entity = cx.entity().downgrade();
        let (close, cancel, rename) = (entity.clone(), entity.clone(), entity);
        let width = DialogWidth::Xs.px().min(
            muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * Theme::spacing_css(4.),
        );
        let paper = dialog_paper(&theme)
            .w(px(width))
            .child(
                dialog_title(&theme).child(
                    label("Rename tab")
                        .variant(theme.typography.h6)
                        .color(theme.palette.text.primary),
                ),
            )
            .child(
                dialog_content(&theme, true, false).child(
                    div()
                        .mt(Theme::spacing(0.5))
                        .child(TextField::new("rename-tab-field", &self.input).full_width()),
                ),
            )
            .child(
                dialog_actions()
                    .child(
                        Button::new("rename-tab-cancel", "Cancel").on_click(move |_, _, cx| {
                            let _ = cancel.update(cx, |_, cx| cx.emit(RenameTabEvent::Close));
                        }),
                    )
                    .child(
                        Button::new("rename-tab-commit", "Rename")
                            .contained()
                            .disabled(blank)
                            .on_click(move |_, _, cx| {
                                let _ = rename.update(cx, |this, cx| this.commit(cx));
                            }),
                    ),
            );
        let on_close: CloseHandler = Rc::new(move |_, cx| {
            let _ = close.update(cx, |_, cx| cx.emit(RenameTabEvent::Close));
        });
        div()
            .id("rename-tab-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "rename-tab-backdrop",
                paper,
                Some(on_close),
            ))
    }
}

/// How long a tab slides to its new place when the order changes.
pub const SLIDE_DURATION: Duration = Duration::from_millis(160);
/// Hovered midpoints are unreliable while tabs are sliding: live resorting
/// during a drag pauses this long after a slide starts.
pub const SLIDE_SETTLE: Duration = Duration::from_millis(170);

/// `cubic-bezier(0.2, 0, 0, 1)`, the slide's easing, at progress `x` (0..1).
pub fn slide_easing(x: f32) -> f32 {
    cubic_bezier(0.2, 0., 0., 1., x)
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` at `x`: the curve's parameter is
/// found by bisection on its x, then its y is the eased value.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    let curve = |a: f32, b: f32, t: f32| {
        let u = 1. - t;
        3. * u * u * t * a + 3. * u * t * t * b + t * t * t
    };
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..40 {
        let mid = (low + high) / 2.;
        if curve(x1, x2, mid) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    curve(y1, y2, (low + high) / 2.)
}

/// Where each tab starts in its row: tabs sit edge to edge, each `width` wide.
pub fn tab_slots(order: &[(String, f32)]) -> HashMap<&str, f32> {
    let mut left = 0.;
    order
        .iter()
        .map(|(id, width)| {
            let slot = (id.as_str(), left);
            left += width;
            slot
        })
        .collect()
}

/// One tab sliding from where it was (`from`, CSS px off its new slot) to
/// its new slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slide {
    pub from: f32,
    pub started: Instant,
}

impl Slide {
    /// The tab's offset from its slot at `now`, or `None` once it has arrived.
    pub fn offset(&self, now: Instant) -> Option<f32> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= SLIDE_DURATION {
            return None;
        }
        let progress = elapsed.as_secs_f32() / SLIDE_DURATION.as_secs_f32();
        Some(self.from * (1. - slide_easing(progress)))
    }
}

/// FLIP for one pane's strip: when its order changes (live drag resorting,
/// reorder chords, drops), each tab slides from its previous slot instead of
/// teleporting. Slots are layout positions, unaffected by the scroll and by
/// a slide still running, so back-to-back reorders start from the truth.
#[derive(Clone, Debug, Default)]
pub struct TabSlides {
    order: Vec<(String, f32)>,
    slides: HashMap<String, Slide>,
}

impl TabSlides {
    /// Take this frame's order (ids with their widths). True when tabs
    /// started sliding.
    pub fn update(&mut self, order: Vec<(String, f32)>, now: Instant) -> bool {
        let ids_changed = order.len() != self.order.len()
            || order
                .iter()
                .zip(&self.order)
                .any(|((new, _), (old, _))| new != old);
        let mut started = false;
        if ids_changed && !self.order.is_empty() {
            let before = tab_slots(&self.order);
            let after = tab_slots(&order);
            for (id, left) in &after {
                if let Some(from) = before.get(id)
                    && (from - left).abs() > 0.5
                {
                    self.slides.insert(
                        id.to_string(),
                        Slide {
                            from: from - left,
                            started: now,
                        },
                    );
                    started = true;
                }
            }
        }
        self.slides.retain(|id, slide| {
            order.iter().any(|(tab, _)| tab == id) && slide.offset(now).is_some()
        });
        self.order = order;
        started
    }

    /// A tab's offset from its slot now (0 when it is not sliding).
    pub fn offset(&self, id: &str, now: Instant) -> f32 {
        self.slides
            .get(id)
            .and_then(|slide| slide.offset(now))
            .unwrap_or(0.)
    }

    /// Some tab is still on its way.
    pub fn running(&self, now: Instant) -> bool {
        self.slides
            .values()
            .any(|slide| slide.offset(now).is_some())
    }

    /// Where a tab's slot starts and how wide it is, in the current order.
    pub fn slot(&self, id: &str) -> Option<(f32, f32)> {
        let mut left = 0.;
        for (tab, width) in &self.order {
            if tab == id {
                return Some((left, *width));
            }
            left += width;
        }
        None
    }
}

/// A rectangle on the screen (or in a window), in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ScreenRect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// `pointInsideAnyWindow`: whether the pointer is over one of the windows.
pub fn point_inside_any_window(x: f32, y: f32, windows: &[ScreenRect]) -> bool {
    windows.iter().any(|window| window.contains(x, y))
}

/// `shouldDetachTabDrag`: a drag no strip took becomes a window of its own
/// only when the pointer actually left the window it started in.
pub fn should_detach_tab_drag(dropped: bool, x: f32, y: f32, window: ScreenRect) -> bool {
    !dropped && !window.contains(x, y)
}

/// Open the rename dialog for a tab; the owner subscribes to its
/// [`RenameTabEvent`]s, applies a rename to its tabs store, drops the entity
/// and gives the terminal its focus back.
pub fn open_rename_tab_dialog(
    tab_id: &str,
    title: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<RenameTabDialog> {
    cx.new(|cx| RenameTabDialog::new(tab_id, title, window, cx))
}

/// Where a context menu opened by keyboard goes when it has no pointer
/// position: the window's centre (used by the overflow menus as well).
pub fn window_center(window: &Window) -> Point<Pixels> {
    let size = window.viewport_size();
    point(size.width / 2., size.height / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_arrows_only_past_the_tolerance() {
        assert_eq!(
            TabScrollState::measure(500., 500., 0.),
            TabScrollState::default()
        );
        // Seven pixels of overflow are not worth an arrow.
        assert_eq!(
            TabScrollState::measure(507., 500., 0.),
            TabScrollState::default()
        );
        assert_eq!(
            TabScrollState::measure(900., 500., 0.),
            TabScrollState {
                overflow: true,
                can_scroll_left: false,
                can_scroll_right: true
            }
        );
        assert_eq!(
            TabScrollState::measure(900., 500., 200.),
            TabScrollState {
                overflow: true,
                can_scroll_left: true,
                can_scroll_right: true
            }
        );
        assert_eq!(
            TabScrollState::measure(900., 500., 395.),
            TabScrollState {
                overflow: true,
                can_scroll_left: true,
                can_scroll_right: false
            }
        );
    }

    #[test]
    fn scrolls_by_most_of_the_row_and_sideways_with_the_wheel() {
        assert_eq!(scroll_step(1000.), 720.);
        assert_eq!(scroll_step(100.), 160.);
        assert_eq!(wheel_delta(0., 40.), 40.);
        assert_eq!(wheel_delta(-30., 10.), -30.);
    }

    #[test]
    fn reveals_a_tab_with_room_to_spare() {
        // Visible: 100+36 .. 100+600-80-36 = 136 .. 584.
        assert_eq!(reveal_offset(100., 600., 80., 200., 180.), None);
        assert_eq!(reveal_offset(100., 600., 80., 120., 180.), Some(84.));
        assert_eq!(reveal_offset(100., 600., 80., 20., 180.), Some(0.));
        assert_eq!(
            reveal_offset(100., 600., 80., 500., 180.),
            Some(500. + 180. - 600. + 80. + 36.)
        );
    }

    #[test]
    fn detaches_rejected_drops_only_when_they_end_outside_the_source_window() {
        let window = ScreenRect {
            x: 100.,
            y: 50.,
            width: 800.,
            height: 600.,
        };
        assert!(should_detach_tab_drag(false, 99., 300., window));
        assert!(should_detach_tab_drag(false, 900., 300., window));
        assert!(should_detach_tab_drag(false, 400., 650., window));
        assert!(!should_detach_tab_drag(false, 400., 300., window));
        assert!(!should_detach_tab_drag(true, 950., 300., window));
    }

    #[test]
    fn recognizes_points_inside_either_muxus_window() {
        let windows = [
            ScreenRect {
                x: 100.,
                y: 50.,
                width: 800.,
                height: 600.,
            },
            ScreenRect {
                x: 1000.,
                y: 100.,
                width: 500.,
                height: 400.,
            },
        ];
        assert!(point_inside_any_window(100., 50., &windows));
        assert!(point_inside_any_window(899., 649., &windows));
        assert!(point_inside_any_window(1250., 300., &windows));
        // Outer edges and desktop points stay available for detaching.
        assert!(!point_inside_any_window(900., 300., &windows));
        assert!(!point_inside_any_window(1500., 300., &windows));
        assert!(!point_inside_any_window(50., 20., &windows));
    }

    #[test]
    fn eases_out_like_the_css_curve() {
        assert!(slide_easing(0.).abs() < 1e-4);
        assert!((slide_easing(1.) - 1.).abs() < 1e-4);
        // cubic-bezier(0.2, 0, 0, 1) is most of the way there by half time.
        let half = slide_easing(0.5);
        assert!(half > 0.8 && half < 0.95, "{half}");
        assert!(slide_easing(0.25) < half);
    }

    fn order(ids: &[(&str, f32)]) -> Vec<(String, f32)> {
        ids.iter()
            .map(|(id, width)| (id.to_string(), *width))
            .collect()
    }

    #[test]
    fn tabs_slide_from_their_old_slots_when_the_order_changes() {
        let mut slides = TabSlides::default();
        let start = Instant::now();
        assert!(!slides.update(order(&[("a", 210.), ("b", 190.), ("c", 190.)]), start));
        // Same order next frame: nothing moves.
        assert!(!slides.update(order(&[("a", 210.), ("b", 190.), ("c", 190.)]), start));
        // "c" moves first: it comes from 400px to the right, the others shift right.
        assert!(slides.update(order(&[("c", 190.), ("a", 210.), ("b", 190.)]), start));
        assert_eq!(slides.offset("c", start), 400.);
        assert_eq!(slides.offset("a", start), -190.);
        assert_eq!(slides.offset("b", start), -190.);
        assert!(slides.running(start));
        let halfway = start + SLIDE_DURATION / 2;
        assert!(slides.offset("c", halfway) < 100.);
        let done = start + SLIDE_DURATION;
        assert_eq!(slides.offset("c", done), 0.);
        assert!(!slides.running(done));
        assert_eq!(slides.slot("a"), Some((190., 210.)));
        assert_eq!(slides.slot("zz"), None);
    }

    #[test]
    fn a_new_or_closed_tab_slides_only_the_tabs_that_moved() {
        let mut slides = TabSlides::default();
        let start = Instant::now();
        slides.update(order(&[("a", 210.), ("b", 190.), ("c", 190.)]), start);
        assert!(slides.update(order(&[("a", 210.), ("c", 190.)]), start));
        assert_eq!(slides.offset("a", start), 0.);
        assert_eq!(slides.offset("c", start), 190.);
        assert!(!slides.update(order(&[("a", 210.), ("c", 190.), ("d", 190.)]), start));
        assert_eq!(slides.offset("d", start), 0.);
    }

    #[test]
    fn renames_to_the_trimmed_title_only() {
        assert_eq!(renamed_title("  prod db "), Some("prod db".into()));
        assert_eq!(renamed_title("   "), None);
        assert_eq!(renamed_title(""), None);
    }
}
