//! The host picker of an empty pane (`client/src/components/HostPickerPopover.tsx`):
//! the saved hosts in their folders, filtered as you type; Enter connects
//! the first match. The window connects the host (into the blank tab the
//! picker was opened from) on [`HostPickerEvent::Connect`].

use muxus_core::api::{SavedHostProfile, SshConfigResponse};
use muxus_session::Backend;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, point, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, TextVariant, Theme, sizes, weight};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::list::{
    ListItemButton, ListSubheader, list_item_icon, list_item_text, pitch_correction,
};
use muxus_ui::widgets::menu::{self, popover_paper};
use muxus_ui::widgets::{Button, ButtonColor, TextField};

use super::kit::{snap, text};
use crate::hosts::managed::{
    ManagedHost, group_managed_hosts, managed_host_address, managed_host_display_name,
};
use crate::sidebar::kind_icon::host_kind_icon_name;

/// What the picker asks the window to do.
#[derive(Clone, Debug, PartialEq)]
pub enum HostPickerEvent {
    Close,
    /// Connect this host (into `replace_tab_id`, the blank tab, if any).
    Connect {
        host: ManagedHost,
        replace_tab_id: Option<String>,
    },
    /// "Add host": open the host editor for a new host.
    AddHost,
}

pub struct HostPicker {
    search: Entity<InputState>,
    ssh: Option<SshConfigResponse>,
    saved: Vec<SavedHostProfile>,
    position: Point<Pixels>,
    replace_tab_id: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HostPickerEvent> for HostPicker {}

impl Focusable for HostPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the picker with its top-left corner at `position` (the bottom-left
/// of the empty pane's "Saved host" card).
pub fn open_host_picker(
    backend: Backend,
    position: Point<Pixels>,
    replace_tab_id: Option<String>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<HostPicker> {
    cx.new(|cx| HostPicker::new(backend, position, replace_tab_id, window, cx))
}

impl HostPicker {
    pub fn new(
        backend: Backend,
        position: Point<Pixels>,
        replace_tab_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts"));
        let subscriptions =
            vec![
                cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => this.connect_first(cx),
                    _ => cx.notify(),
                }),
            ];
        search.update(cx, |state, cx| state.focus(window, cx));
        cx.spawn(async move |this, cx| {
            let ssh = backend.ssh_config().await.ok();
            let saved = backend
                .saved_hosts()
                .await
                .map(|saved| saved.profiles)
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.ssh = ssh;
                this.saved = saved;
                cx.notify();
            });
        })
        .detach();
        Self {
            search,
            ssh: None,
            saved: Vec::new(),
            position,
            replace_tab_id,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn groups(&self, cx: &App) -> Vec<crate::hosts::managed::ManagedHostGroup> {
        let filter = self.search.read(cx).value().to_string();
        match &self.ssh {
            Some(ssh) => group_managed_hosts(
                &ssh.hosts,
                &self.saved,
                &ssh.files,
                Some(&ssh.path),
                &filter,
            ),
            None => group_managed_hosts(&[], &self.saved, &[], None, &filter),
        }
    }

    fn connect(&mut self, host: ManagedHost, cx: &mut Context<Self>) {
        cx.emit(HostPickerEvent::Connect {
            host,
            replace_tab_id: self.replace_tab_id.clone(),
        });
        cx.emit(HostPickerEvent::Close);
    }

    fn connect_first(&mut self, cx: &mut Context<Self>) {
        if let Some(host) = self
            .groups(cx)
            .into_iter()
            .flat_map(|group| group.hosts)
            .next()
        {
            self.connect(host, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(HostPickerEvent::Close);
        }
    }

    /// The address line: `secondary` at `fontSize: 11` on body2's line.
    fn address_variant(theme: &Theme) -> TextVariant {
        let body2 = theme.typography.body2;
        TextVariant {
            size: sizes::TEXT_DENSE,
            line_height: sizes::TEXT_DENSE * body2.line_height / body2.size,
            ..body2
        }
    }

    /// Where Chrome and GPUI stack one row: the 1px collapsed margin, the
    /// dense 4px padding, the two-line text's 6px margins and both lines.
    fn row_pitches(theme: &Theme) -> (f32, f32) {
        let fixed = 1. + 2. * Theme::spacing_css(0.5) + 2. * 6.;
        let (title, address) = (
            theme.typography.body2.line_height,
            Self::address_variant(theme).line_height,
        );
        (fixed + title + address, fixed + snap(title) + snap(address))
    }

    fn row(
        &self,
        index: usize,
        host: &ManagedHost,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let ink = host
            .metadata()
            .and_then(|metadata| metadata.color.as_deref())
            .and_then(try_hex)
            .unwrap_or(p.text.secondary);
        let weak = cx.entity().downgrade();
        let target = host.clone();
        let (chrome_pitch, gpui_pitch) = Self::row_pitches(theme);
        ListItemButton::new(("host-picker-row", index))
            .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
            .extra_bottom(pitch_correction(index, chrome_pitch, gpui_pitch))
            .on_click(move |_, _, cx| {
                let host = target.clone();
                let _ = weak.update(cx, |this, cx| this.connect(host, cx));
            })
            // `<Icon sx={{ fontSize: 19 }} />` in a 34px column.
            .child(list_item_icon(
                34.,
                icon(host_kind_icon_name(host.kind()), 19., ink),
            ))
            .child(list_item_text(
                text(
                    managed_host_display_name(host).to_owned(),
                    t.body2,
                    p.text.primary,
                )
                .weight(weight::EMPHASIS),
                Some(
                    text(
                        managed_host_address(host),
                        Self::address_variant(theme),
                        p.text.secondary,
                    )
                    .into_any_element(),
                ),
            ))
            .into_any_element()
    }
}

impl Render for HostPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let groups = self.groups(cx);
        let total = self.ssh.as_ref().map_or(0, |ssh| ssh.hosts.len()) + self.saved.len();
        let mut list = div()
            .id("host-picker-list")
            .flex()
            .flex_col()
            .max_h(px(380.))
            .overflow_y_scroll()
            .py(Theme::spacing(0.5));
        let mut index = 0;
        for group in &groups {
            // A compact popover rather than a tree, but a nested group still
            // reads as a path.
            list = list.child(
                ListSubheader::new(group.label.split('/').collect::<Vec<_>>().join(" / ")).caps(),
            );
            for host in &group.hosts {
                list = list.child(self.row(index, host, &theme, cx));
                index += 1;
            }
            // The group's last row keeps its bottom margin.
            list = list.child(div().h(px(1.)).flex_none());
        }
        if index == 0 {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(Theme::spacing(0.75))
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(3.))
                    .child(icon("DnsOutlined", 30., p.text.disabled))
                    .child(text(
                        if total == 0 {
                            "No saved hosts yet."
                        } else {
                            "No hosts match your search."
                        },
                        t.body2,
                        p.text.secondary,
                    )),
            );
        }
        let add = cx.entity().downgrade();
        // The paper's `mt: 0.75` is not part of the box MUI keeps inside the
        // window, so near the bottom edge it hangs that much lower.
        let paper = popover_paper(&theme)
            .mb(-Theme::spacing(0.75))
            .w(px(360.))
            .overflow_hidden()
            .child(
                div()
                    .p(Theme::spacing(1.25))
                    .pb(Theme::spacing(1.))
                    .child(
                        div()
                            .px(Theme::spacing(0.25))
                            .mb(Theme::spacing(1.))
                            .child(text("Connect to a host", t.subtitle2, p.text.primary)),
                    )
                    .child(
                        TextField::new("host-picker-search", &self.search)
                            .start(icon("Search", SMALL, p.action.active))
                            .full_width(),
                    ),
            )
            .child(divider(&theme))
            .child(list)
            .child(divider(&theme))
            .child(
                div().p(Theme::spacing(0.75)).child(
                    Button::new("host-picker-add", "Add host")
                        .full_width()
                        .color(ButtonColor::Inherit)
                        .start_icon("Add")
                        .on_click(move |_, _, cx| {
                            let _ = add.update(cx, |_, cx| {
                                // The request first: an owner that drops its
                                // subscription on `Close` would miss it.
                                cx.emit(HostPickerEvent::AddHost);
                                cx.emit(HostPickerEvent::Close);
                            });
                        }),
                ),
            );
        let close = cx.entity().downgrade();
        div()
            .id("host-picker")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(menu::popup(
                self.position + point(px(0.), Theme::spacing(0.75)),
                move |_, cx| {
                    let _ = close.update(cx, |_, cx| cx.emit(HostPickerEvent::Close));
                },
                paper,
            ))
    }
}
