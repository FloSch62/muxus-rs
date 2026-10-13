//! Text placement probe: the same boxes as a reference HTML page, so the
//! baseline GPUI picks for each size and line height can be measured
//! against Chrome's. Configs come from MUXUS_BASELINE_CONFIGS (JSON list of
//! [size, line_height, weight]).

use gpui_kit::*;
use muxus_ui::theme::fonts;

struct Probe(Vec<(f32, f32, f32)>);

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .relative()
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x000000));
        for (i, (size, lh, weight)) in self.0.iter().enumerate() {
            root = root.child(
                div()
                    .absolute()
                    .left(px(20.))
                    .top(px(20. + i as f32 * 40.))
                    .h(px(*lh))
                    .font_family(fonts::UI)
                    .text_size(px(*size))
                    .line_height(px(*lh))
                    .font_weight(FontWeight(*weight))
                    .whitespace_nowrap()
                    .child(format!("HHHH xg {size}/{lh}/{weight}")),
            );
        }
        root
    }
}

struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    if std::env::var_os("MUXUS_LOG").is_some() {
        let _ = log::set_boxed_logger(Box::new(StderrLog))
            .map(|()| log::set_max_level(log::LevelFilter::Info));
    }
    let raw = std::env::var("MUXUS_BASELINE_CONFIGS").unwrap_or_else(|_| "[[13,19.5,450]]".into());
    let configs: Vec<(f32, f32, f32)> = raw
        .trim_matches(|c| c == '[' || c == ']')
        .split("],[")
        .filter_map(|item| {
            let v: Vec<f32> = item
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            (v.len() == 3).then(|| (v[0], v[1], v[2]))
        })
        .collect();
    gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(muxus_ui::theme::Mode::Light, cx);
            let configs = configs.clone();
            gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                window.toggle_fullscreen();
                cx.new(|_| Probe(configs))
            })
            .expect("open window");
        });
}
