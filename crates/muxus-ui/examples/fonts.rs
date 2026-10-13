//! Renders Inter at every bundled weight next to a few icons, for checking the
//! font and icon pipeline in a screenshot.

use gpui_kit::*;
use muxus_ui::icon::icon;
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Mode};

struct Specimen;

impl Render for Specimen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .size_full()
            .bg(theme.palette.background_default)
            .text_color(theme.palette.text.primary);
        for weight in [400., 450., 500., 550., 600., 700.] {
            col = col.child(
                div()
                    .flex()
                    .gap_4()
                    .child(
                        div()
                            .ui_font(13., weight)
                            .child(format!("{weight} core-switch (console) Production")),
                    )
                    .child(div().ui_font(18.5714, weight).child("Muxus Workspace")),
            );
        }
        col.child(
            div()
                .flex()
                .gap_2()
                .text_color(theme.palette.text.secondary)
                .child(icon("DnsOutlined", 16., theme.palette.text.secondary))
                .child(icon("ScienceOutlined", 16., theme.palette.success.main))
                .child(icon("CloudOutlined", 16., theme.palette.error.main))
                .child(icon(
                    "SettingsOutlined",
                    18.5714,
                    theme.palette.action.active,
                ))
                .child(img("icons/muxus.svg").size(px(28.))),
        )
        .child(
            div()
                .font_family("JetBrains Mono")
                .text_size(px(14.))
                .child("deploy@web-01:~$ ll conf/ 0O1l"),
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
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Specimen))
                .expect("open window");
        });
}
