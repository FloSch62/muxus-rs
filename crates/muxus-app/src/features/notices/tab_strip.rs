//! The parts of `components/TabStrip.tsx` the window's tab strip still
//! lacks: the "Rename tab" dialog and the scroll arrows that appear when a
//! pane holds more tabs than fit ("Earlier tabs" / "Later tabs"), with the
//! rules that decide when they show, how far they scroll and how the active
//! tab is kept in view.
//!
//! The strip itself belongs to the window (`ui/panes.rs`). It hooks these in
//! by keeping a [`ScrollHandle`] on its tab row, asking
//! [`TabScrollState::of`] each frame, mounting [`earlier_tabs_button`] /
//! [`later_tabs_button`] when the state says so, calling
//! [`reveal_offset`] after the active tab changes, and opening a
//! [`RenameTabDialog`] on a tab's double-click or its "Rename tab" item.

use std::rc::Rc;

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
    fn renames_to_the_trimmed_title_only() {
        assert_eq!(renamed_title("  prod db "), Some("prod db".into()));
        assert_eq!(renamed_title("   "), None);
        assert_eq!(renamed_title(""), None);
    }
}
