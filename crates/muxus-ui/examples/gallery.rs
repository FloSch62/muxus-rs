//! Every shared widget in both themes, for screenshot review:
//! `hack/shot.sh --env MUXUS_THEME=dark out.png -- target/debug/examples/gallery`.

use gpui_kit::base::input::InputState;
use gpui_kit::*;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Mode, Status};
use muxus_ui::widgets::display::{badge, divider, linear_progress};
use muxus_ui::widgets::menu::{self, MenuItem, chord_hint, menu_divider};
use muxus_ui::widgets::tabs::tabs_row;
use muxus_ui::widgets::{
    Alert, Button, ButtonColor, Checkbox, Chip, IconButton, Select, Switch, Tab, TextField,
};

struct Gallery {
    search: Entity<InputState>,
    name: Entity<InputState>,
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let section = |title: &'static str| {
            div().mt(px(16.)).mb(px(8.)).child(
                label(title)
                    .size(theme.typography.overline.size)
                    .weight(600.)
                    .line_height(20.)
                    .color(p.text.secondary),
            )
        };
        let row = || div().flex().items_center().gap(px(12.));
        div()
            .size_full()
            .flex()
            .gap(px(32.))
            .p(px(24.))
            .bg(p.background_default)
            .text_color(p.text.primary)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(520.))
                    .child(section("BUTTONS"))
                    .child(
                        row()
                            .child(Button::new("text", "Text"))
                            .child(Button::new("outlined", "Outlined").outlined())
                            .child(Button::new("contained", "Contained").contained())
                            .child(Button::new("icon", "Add host").outlined().start_icon("Add"))
                            .child(Button::new("disabled", "Disabled").contained().disabled(true)),
                    )
                    .child(
                        row()
                            .mt(px(8.))
                            .child(Button::new("medium", "Medium").contained().medium())
                            .child(Button::new("error", "Delete").color(ButtonColor::Error))
                            .child(Button::new("inherit", "Workspace 1").color(ButtonColor::Inherit).start_icon("WorkspacesOutlined")),
                    )
                    .child(section("ICON BUTTONS"))
                    .child(
                        row()
                            .child(IconButton::new("i1", "Menu"))
                            .child(IconButton::new("i2", "SettingsOutlined").tooltip("Settings"))
                            .child(IconButton::new("i3", "SwapHorizOutlined").badge(badge(3, p.success.main, p.success.contrast_text, 5., muxus_ui::icon::SMALL)))
                            .child(IconButton::new("i4", "CenterFocusStrongOutlined").color(ButtonColor::Primary))
                            .child(IconButton::new("i5", "Add").medium())
                            .child(IconButton::new("i6", "Close").disabled(true)),
                    )
                    .child(section("TEXT FIELDS"))
                    .child(TextField::new("search", &self.search).start_icon("Search").full_width())
                    .child(div().mt(px(12.)).child(TextField::new("name", &self.name).full_width()))
                    .child(div().mt(px(12.)).child(Select::new("scale", "100% (default)").full_width()))
                    .child(section("SELECTION"))
                    .child(
                        row()
                            .child(Checkbox::new("c1", true))
                            .child(Checkbox::new("c2", false))
                            .child(Checkbox::new("c3", false).indeterminate(true))
                            .child(Checkbox::new("c4", true).small())
                            .child(Checkbox::radio("r1", true))
                            .child(Checkbox::radio("r2", false))
                            .child(Switch::new("s1", true))
                            .child(Switch::new("s2", false))
                            .child(Switch::new("s3", true).small())
                            .child(Switch::new("s4", false).small()),
                    )
                    .child(section("CHIPS AND PROGRESS"))
                    .child(
                        row()
                            .child(Chip::new("Connected"))
                            .child(Chip::new("Outlined").outlined())
                            .child(Chip::new("Primary").color(p.primary.main))
                            .child(Chip::new("Error").outlined().color(p.error.main)),
                    )
                    .child(div().mt(px(12.)).w(px(300.)).child(linear_progress(&theme, 0.62))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(520.))
                    .child(section("TABS"))
                    .child(
                        tabs_row(&theme)
                            .child(Tab::new("t1", "General", true))
                            .child(Tab::new("t2", "Connection route", false))
                            .child(Tab::new("t3", "Forwarding", false)),
                    )
                    .child(divider(&theme))
                    .child(section("MENU"))
                    .child(
                        menu::paper(&theme)
                            .w(px(280.))
                            .child(MenuItem::new("m1", "Light").icon("LightModeOutlined").selected(true))
                            .child(MenuItem::new("m2", "Find").icon("SearchOutlined").trailing(chord_hint("Ctrl+Shift+F", &theme)))
                            .child(MenuItem::new("m3", "Zoom pane").icon("OpenInFullOutlined").disabled(true))
                            .child(menu_divider(&theme))
                            .child(MenuItem::new("m4", "Delete host").icon("DeleteOutlined").destructive()),
                    )
                    .child(section("ALERTS"))
                    .child(Alert::new(Status::Info, "The host editor is not available yet."))
                    .child(div().mt(px(8.)).child(Alert::new(Status::Success, "Copied \"ssh web-01\"").filled()))
                    .child(div().mt(px(8.)).child(Alert::new(Status::Error, "Connection refused").filled()))
                    .child(section("TYPE"))
                    .child(
                        paragraph("Open a shell, connect over SSH, Telnet or serial, open a remote desktop, or manage a device over gNMI or NETCONF.")
                            .size(theme.typography.body2.size)
                            .line_height(theme.typography.body2.line_height)
                            .color(p.text.secondary),
                    ),
            )
    }
}

fn main() {
    let mode = if std::env::var("MUXUS_THEME").as_deref() == Ok("dark") {
        Mode::Dark
    } else {
        Mode::Light
    };
    gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(mode, cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.toggle_fullscreen();
                let search =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Search or user@host ⏎"));
                let name =
                    cx.new(|cx| InputState::new(window, cx).default_value("web-01.prod.internal"));
                cx.new(|_| Gallery { search, name })
            })
            .expect("open window");
        });
}
