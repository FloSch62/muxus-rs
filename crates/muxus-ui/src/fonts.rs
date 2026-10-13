//! Bundled fonts (see `hack/build_fonts.py` and `assets/fonts`).
//!
//! GPUI's text system rasterizes a variable font at its default instance, so
//! the 450 and 550 weights the chrome uses would render as 400 from Inter's
//! variable file. The fonts here are static faces cut from the same variable
//! font the client bundles, one per weight, each carrying its exact
//! `usWeightClass`; requesting `FontWeight(450.)` matches the 450 face.

use std::borrow::Cow;

use gpui_kit::App;

/// Weights of Inter shipped as static faces.
pub const INTER_WEIGHTS: [f32; 8] = [400., 450., 500., 550., 600., 650., 700., 800.];

macro_rules! font {
    ($file:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/",
            $file
        ))
        .as_slice()
    };
}

/// Every bundled font file.
pub fn bundled() -> Vec<Cow<'static, [u8]>> {
    [
        font!("Inter-400.ttf"),
        font!("Inter-450.ttf"),
        font!("Inter-500.ttf"),
        font!("Inter-550.ttf"),
        font!("Inter-600.ttf"),
        font!("Inter-650.ttf"),
        font!("Inter-700.ttf"),
        font!("Inter-800.ttf"),
        font!("JetBrainsMono-400.ttf"),
        font!("JetBrainsMono-500.ttf"),
        font!("JetBrainsMono-700.ttf"),
        font!("JetBrainsMono-400-Italic.ttf"),
        font!("JetBrainsMono-700-Italic.ttf"),
        font!("PureNerdFont.ttf"),
    ]
    .into_iter()
    .map(Cow::Borrowed)
    .collect()
}

/// Register the bundled fonts with the application's text system.
pub fn register(cx: &mut App) {
    if let Err(error) = cx.text_system().add_fonts(bundled()) {
        eprintln!("muxus: could not load the bundled fonts: {error:#}");
    }
}
