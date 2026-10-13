//! MUI `Tabs`/`Tab` as the theme styles them: 40px tall, 550-weight labels
//! in text.secondary (text.primary when selected) and a 2px indicator in
//! text.primary with rounded top corners.
//!
//! A tab can carry a start icon (`iconPosition="start"`), fill its share of
//! a `variant="fullWidth"` row, or stand in a vertical `Tabs` column with
//! the indicator on its end edge; `sx` sizes override the defaults.

use std::rc::Rc;

use crate::zoom::px;
use gpui_kit::{
    App, ClickEvent, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::color::alpha;
use crate::icon::{SMALL, icon};
use crate::theme::{ActiveTheme, Theme, fonts};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A tab label's line height per px of font size (MUI `Tab`'s `lineHeight: 1.25`).
pub const LINE_HEIGHT: f32 = 1.25;

/// The `Tabs` row container.
pub fn tabs_row(theme: &Theme) -> Div {
    let _ = theme;
    div().flex().items_stretch().min_h(px(40.)).h(px(40.))
}

#[derive(IntoElement)]
pub struct Tab {
    id: ElementId,
    label: SharedString,
    selected: bool,
    on_click: Option<Handler>,
    icon: Option<SharedString>,
    gap: f32,
    vertical: bool,
    fill: bool,
    min_width: f32,
    min_height: Option<f32>,
    padding: (f32, f32, f32),
    indicator_offset: f32,
    content: Option<gpui_kit::AnyElement>,
    font_size: Option<f32>,
}

impl Tab {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected,
            on_click: None,
            icon: None,
            gap: 8.,
            vertical: false,
            fill: false,
            min_width: 90.,
            min_height: None,
            padding: (16., 16., 12.),
            indicator_offset: 0.,
            content: None,
            font_size: None,
        }
    }

    /// A small icon before the label (`iconPosition="start"`), `gap` px
    /// apart (8 by default, MUI's icon margin).
    pub fn icon(mut self, icon: impl Into<SharedString>, gap: f32) -> Self {
        self.icon = Some(icon.into());
        self.gap = gap;
        self
    }

    /// A tab in a vertical `Tabs` column: content at the start, the
    /// indicator a 2px bar on the end edge.
    pub fn vertical(mut self) -> Self {
        self.vertical = true;
        self
    }

    /// Any content as the label (a name with a count beside it); it takes
    /// the tab's width.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// `sx={{ fontSize }}` in px.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = Some(size);
        self
    }

    /// `variant="fullWidth"`: an equal share of the row.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self.min_width = 0.;
        self
    }

    /// `sx={{ minWidth }}`.
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    /// `sx={{ minHeight }}`.
    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = Some(height);
        self
    }

    /// `sx` padding: start, end and block, in px.
    pub fn padding(mut self, start: f32, end: f32, block: f32) -> Self {
        self.padding = (start, end, block);
        self
    }

    /// Move the indicator outwards by `offset` px (-1 lays it over the
    /// `Tabs`' 1px divider line instead of beside it).
    pub fn indicator_offset(mut self, offset: f32) -> Self {
        self.indicator_offset = offset;
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

impl RenderOnce for Tab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = &theme.palette;
        let button = theme.typography.button;
        let ink = if self.selected {
            p.text.primary
        } else {
            p.text.secondary
        };
        let (start, end, block) = self.padding;
        let mut tab = div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .min_w(px(self.min_width))
            .pl(px(start))
            .pr(px(end))
            .py(px(block))
            .font_family(fonts::UI)
            .text_size(px(self.font_size.unwrap_or(button.size)))
            .font_weight(FontWeight(550.))
            .line_height(px(self.font_size.unwrap_or(button.size) * LINE_HEIGHT))
            .text_color(ink)
            .cursor_pointer();
        if !self.vertical {
            tab = tab.justify_center();
        }
        if self.fill {
            tab = tab.flex_1().flex_basis(px(0.));
        }
        if let Some(height) = self.min_height {
            tab = tab.min_h(px(height));
        }
        if let Some(name) = &self.icon {
            tab = tab.gap(px(self.gap)).child(icon(name, SMALL, ink));
        }
        tab = match self.content {
            Some(content) => tab.child(div().flex_1().min_w_0().child(content)),
            None => tab.child(self.label),
        };
        if self.selected {
            let indicator = div().absolute().rounded_t(px(2.)).bg(p.text.primary);
            tab = tab.child(if self.vertical {
                indicator
                    .top(px(0.))
                    .bottom(px(0.))
                    .right(px(self.indicator_offset))
                    .w(px(2.))
            } else {
                indicator
                    .left(px(0.))
                    .right(px(0.))
                    .bottom(px(self.indicator_offset))
                    .h(px(2.))
            });
        } else {
            let hover = alpha(p.text.primary, 1.);
            tab = tab.hover(move |s| s.text_color(hover));
        }
        if let Some(handler) = self.on_click {
            tab = tab.on_click(move |event, window, cx| handler(event, window, cx));
        }
        tab
    }
}
