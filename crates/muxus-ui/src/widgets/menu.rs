//! MUI `Menu`, `MenuItem`, `ListItemIcon` and `Divider` as the theme styles
//! them (`MuiMenu`: 10px corners, a divider-colored border and a soft
//! 0 8px 28px shadow; no elevation overlay).
//!
//! A menu is plain content: the view that owns it keeps the open state and
//! mounts [`popup`] where the menu should appear.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    Anchor, AnyElement, App, BoxShadow, ClickEvent, Decorations, Div, Edges, ElementId, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point,
};

use crate::color::alpha;
use crate::icon::{SMALL, icon};
use crate::label::label;
use crate::theme::{ActiveTheme, Theme, fonts};

/// The menu surface: paper background, border, corners and shadow, with the
/// 8px vertical padding of `MenuList`.
pub fn paper(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .py(px(8.))
        .min_w(px(112.))
        .bg(theme.palette.background_paper)
        .border_1()
        .border_color(theme.palette.divider)
        .rounded(px(10.))
        .shadow(vec![BoxShadow {
            color: theme.components.menu_shadow,
            offset: point(px(0.), px(8.)),
            blur_radius: px(28.),
            spread_radius: px(0.),
            inset: false,
        }])
        .font_family(fonts::UI)
        .text_color(theme.palette.text.primary)
}

/// A `Popover`'s paper (the theme leaves `MuiPopover` alone): the shape's
/// 8px corners and elevation 8, no border.
pub fn popover_paper(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .bg(theme.palette.background_paper)
        .rounded(px(theme.radius))
        .shadow(crate::widgets::button::elevation(8, theme))
        .font_family(fonts::UI)
        .text_color(theme.palette.text.primary)
}

/// Draw order of menus and listboxes among deferred elements: above dialogs
/// ([`super::layer::DIALOG_LAYER`]). GPUI paints every deferred element in
/// priority order but hit-tests a nested one (a select's menu inside a
/// dialog) above its parent, so a menu below its dialog's priority was
/// invisible and still caught the next click.
pub const MENU_LAYER: usize = 4;

/// Mount `content` as a floating layer at a window position (a context menu
/// at the pointer). It stays 16px inside the window, as MUI's
/// `marginThreshold` keeps it.
///
/// Like MUI's `Menu` it is modal: an invisible backdrop covers the window,
/// so the control under the pointer loses its hover (and its tooltip), and a
/// press anywhere outside the menu dismisses it without reaching what is
/// beneath. [`Popup::non_modal`] is MUI's `Popper` instead (an
/// autocomplete's listbox): no backdrop, and a press outside dismisses it
/// and still reaches what it hits.
pub fn popup(
    position: Point<Pixels>,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    content: impl IntoElement,
) -> Popup {
    popup_at(position, Anchor::TopLeft, on_dismiss, content)
}

/// Like [`popup`], with the menu corner that sits on `position` chosen
/// (`BottomLeft` opens upward, as the status bar menu does).
pub fn popup_at(
    position: Point<Pixels>,
    corner: Anchor,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
    content: impl IntoElement,
) -> Popup {
    Popup {
        position,
        corner,
        on_dismiss: Rc::new(on_dismiss),
        content: content.into_any_element(),
        modal: true,
    }
}

/// See [`popup`].
#[derive(IntoElement)]
pub struct Popup {
    position: Point<Pixels>,
    corner: Anchor,
    on_dismiss: crate::widgets::dialog::CloseHandler,
    content: AnyElement,
    modal: bool,
}

impl Popup {
    /// No backdrop: presses outside dismiss the popup and reach their target.
    pub fn non_modal(mut self) -> Self {
        self.modal = false;
        self
    }
}

impl RenderOnce for Popup {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let size = window.viewport_size();
        // A window that draws its own frame keeps the frame's shadow margin as
        // its client inset even maximized or fullscreen, where no shadow is
        // drawn, and GPUI adds that inset to the snap margin: without taking
        // it back the menu would stop 20px short of the window edge.
        let inset = window.client_inset().unwrap_or_default();
        let shadowless = match window.window_decorations() {
            Decorations::Client { tiling } => {
                window.is_fullscreen()
                    || (tiling.top && tiling.bottom && tiling.left && tiling.right)
            }
            Decorations::Server => true,
        };
        // MUI's `marginThreshold` also holds the menu 16px off the window
        // edges it opens from (a control at the very left of the window).
        let mut position = self.position;
        if matches!(self.corner, Anchor::TopLeft | Anchor::BottomLeft) {
            position.x = position.x.max(px(16.));
        }
        if matches!(
            self.corner,
            Anchor::TopLeft | Anchor::TopRight | Anchor::TopCenter
        ) {
            position.y = position.y.max(px(16.));
        }
        let placed = anchored().anchor(self.corner).position(position);
        let placed = if shadowless && inset > px(0.) {
            placed.snap_to_window_with_margin(Edges::all(-inset))
        } else {
            placed.snap_to_window()
        };
        // MUI keeps a menu 16px inside the window (`marginThreshold`). GPUI
        // only snaps an element back once it leaves the window, so the menu
        // carries 16px of transparent room on the sides that face away from
        // its anchor point: the room is what gets snapped to the edge.
        let mut surface = div().id("menu-surface").pr(px(16.));
        surface = match self.corner {
            Anchor::BottomLeft | Anchor::BottomRight | Anchor::BottomCenter => surface.pt(px(16.)),
            _ => surface.pb(px(16.)),
        };
        if !self.modal {
            let outside = self.on_dismiss;
            return deferred(
                placed.child(
                    surface.child(
                        div()
                            .id("menu-content")
                            .occlude()
                            .on_mouse_down_out(move |_, window, cx| outside(window, cx))
                            .child(self.content),
                    ),
                ),
            )
            .with_priority(MENU_LAYER)
            .into_any_element();
        }
        let (left, right) = (self.on_dismiss.clone(), self.on_dismiss);
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("menu-backdrop")
                    .w(size.width)
                    .h(size.height)
                    .occlude()
                    .child(crate::widgets::hover::hover_refresh("menu-hover"))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| left(window, cx))
                    .on_mouse_down(MouseButton::Right, move |_, window, cx| right(window, cx))
                    .child(
                        placed.child(
                            surface.child(
                                div()
                                    .id("menu-content")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(self.content),
                            ),
                        ),
                    ),
            ),
        )
        .with_priority(MENU_LAYER)
        .into_any_element()
    }
}

/// Where a menu opened from a control goes: MUI's default anchor origin, the
/// control's bottom-left corner.
pub fn below(anchor: &crate::widgets::tooltip::AnchorBounds) -> Option<Point<Pixels>> {
    anchor.get().map(|bounds| bounds.bottom_left())
}

/// `<Divider />` inside a menu: 8px above and below.
pub fn menu_divider(theme: &Theme) -> Div {
    div()
        .my(px(8.))
        .h(px(1.))
        .bg(theme.palette.divider)
        .flex_none()
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// One `MenuItem` with an optional `ListItemIcon`, label and trailing chord hint.
#[derive(IntoElement)]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    icon: Option<SharedString>,
    icon_color: Option<gpui_kit::Hsla>,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    chord: Option<SharedString>,
    selected: bool,
    disabled: bool,
    dense: bool,
    destructive: bool,
    on_click: Option<Handler>,
    detail: Option<SharedString>,
    highlighted: bool,
    prefix: Option<AnyElement>,
    prefix_gap: f32,
    focus_ring: bool,
    body: Option<AnyElement>,
}

impl MenuItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            icon_color: None,
            leading: None,
            trailing: None,
            chord: None,
            selected: false,
            disabled: false,
            dense: false,
            destructive: false,
            on_click: None,
            detail: None,
            highlighted: false,
            prefix: None,
            body: None,
            prefix_gap: 0.,
            focus_ring: false,
        }
    }

    /// Content before the label without the 36px icon column, `gap` px
    /// apart (`sx={{ gap }}` items: a color dot or its empty slot).
    pub fn prefix(mut self, element: impl IntoElement, gap: f32) -> Self {
        self.prefix = Some(element.into_any_element());
        self.prefix_gap = gap;
        self
    }

    /// Keyboard focus (`Mui-focusVisible`): the focus tint, as a menu list
    /// moved by the arrow keys shows it.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focus_ring = focused;
        self
    }

    /// A dimmer second line under the label (an autocomplete option's
    /// details): the label drops to body2 and the detail is a caption.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The keyboard-highlighted option of an open select or autocomplete
    /// (`Mui-focused`): the hover tint without the pointer.
    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn icon_color(mut self, color: gpui_kit::Hsla) -> Self {
        self.icon_color = Some(color);
        self
    }

    /// Custom content in the icon column (e.g. a zoom percentage).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// Custom content after the label.
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    /// `<ChordHint chord=... />`.
    pub fn chord(mut self, chord: Option<impl Into<SharedString>>) -> Self {
        self.chord = chord.map(Into::into);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// `dense`: 4px vertical padding and body2 type.
    pub fn dense(mut self) -> Self {
        self.dense = true;
        self
    }

    /// Red label and icon for destructive actions.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Content of its own in place of the label (and detail): the item aligns its
    /// children to the top and sizes to the content, with MUI's 6px vertical padding
    /// (`alignItems: flex-start; py: 0.75`).
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let typo = if self.dense {
            theme.typography.body2
        } else {
            theme.typography.body1
        };
        let ink = if self.destructive {
            p.error.main
        } else {
            p.text.primary
        };
        let icon_ink = self.icon_color.unwrap_or(if self.destructive {
            p.error.main
        } else {
            p.action.active
        });
        let selected_bg = alpha(p.primary.main, p.action.selected_opacity);
        let hover = if self.selected {
            alpha(
                p.primary.main,
                p.action.selected_opacity + p.action.hover_opacity,
            )
        } else {
            p.action.hover
        };
        let pad_y = if self.dense { 4. } else { 6. };
        let mut item = div()
            .id(self.id)
            .flex()
            .items_center()
            .px(px(16.))
            .flex_none()
            .text_size(px(typo.size))
            .line_height(px(typo.line_height))
            .font_weight(FontWeight(typo.weight))
            .text_color(ink)
            .whitespace_nowrap();
        item = if self.body.is_some() {
            item.items_start().py(px(6.))
        } else if self.detail.is_some() {
            // Two lines: padding around their natural height.
            item.py(px(pad_y)).min_h(px(2. * pad_y + typo.line_height))
        } else {
            // MUI's 6px (dense 4px) vertical padding around one line, as an
            // exact fractional height with the content centred: GPUI rounds a
            // text line's box up to whole device pixels, and padding around it
            // would let every item grow by that rounding.
            // A dense item is at least 32px tall (MUI's dense `minHeight`).
            let natural = 2. * pad_y + typo.line_height;
            item.h(px(if self.dense {
                natural.max(32.)
            } else {
                natural
            }))
        };
        if self.selected {
            item = item.bg(selected_bg);
        } else if self.focus_ring {
            item = item.bg(p.action.focus);
        } else if self.highlighted {
            item = item.bg(p.action.hover);
        }
        if self.disabled {
            item = item.opacity(0.38);
        } else {
            item = item.cursor_pointer().hover(move |s| s.bg(hover));
            if let Some(handler) = self.on_click {
                item = item
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        if let Some(prefix) = self.prefix {
            item = item
                .gap(px(self.prefix_gap))
                .child(div().flex().flex_none().child(prefix));
        }
        if let Some(leading) = self.leading {
            item = item.child(div().flex().flex_none().min_w(px(36.)).child(leading));
        } else if let Some(name) = &self.icon {
            item = item.child(
                div()
                    .flex()
                    .flex_none()
                    .min_w(px(36.))
                    .child(icon(name, SMALL, icon_ink)),
            );
        }
        item = match (self.body, self.detail) {
            (Some(body), _) => item.child(div().flex_1().min_w_0().child(body)),
            (None, Some(detail)) => {
                let (body2, caption) = (theme.typography.body2, theme.typography.caption);
                item.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(label(self.label).variant(body2).color(ink))
                        .child(label(detail).variant(caption).color(p.text.secondary)),
                )
            }
            (None, None) => item.child(
                label(self.label)
                    .size(typo.size)
                    .weight(typo.weight)
                    .line_height(typo.line_height)
                    .color(ink)
                    .grow(),
            ),
        };
        if let Some(trailing) = self.trailing {
            item = item.child(trailing);
        }
        if let Some(chord) = self.chord {
            item = item.child(chord_hint(chord, theme));
        }
        item
    }
}

/// `ChordHint`: the trailing shortcut in a menu item, caption tier in the mono font.
pub fn chord_hint(chord: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .ml(px(24.))
        .font_family(fonts::MONO)
        .text_size(px(11.))
        .line_height(px(theme.typography.caption.line_height))
        .text_color(theme.palette.text.secondary)
        .whitespace_nowrap()
        .child(chord.into())
}
