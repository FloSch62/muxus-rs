//! Material icons as GPUI elements, drawn the way MUI's `SvgIcon` draws them:
//! the 24-unit artwork scaled to a square of `size` px, filled with the
//! current text color.

use crate::zoom::px;
use gpui_kit::{Hsla, Styled, Svg, svg};

/// `<Icon sx={{ fontSize: size, color }} />`. GPUI's `svg()` paints with its
/// own text color only (nothing is inherited), so the color is required.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// MUI `fontSize="small"`: pxToRem(20) under the 13px theme.
pub const SMALL: f32 = 20. * 13. / 14.;
/// MUI default icon size: pxToRem(24).
pub const MEDIUM: f32 = 24. * 13. / 14.;

/// The full-color Muxus logo at `size` px, rasterized once per size and
/// window scale. Rendering it from the 1254-unit artwork at the exact device
/// size keeps it as crisp as the browser draws it, and avoids the image
/// loader's asynchronous first frame.
pub fn logo(size: f32, window: &gpui_kit::Window, cx: &mut gpui_kit::App) -> gpui_kit::Img {
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default)]
    struct LogoCache(HashMap<(u32, u32), Arc<gpui_kit::RenderImage>>);
    impl gpui_kit::Global for LogoCache {}

    let scale = window.scale_factor();
    let key = ((size * 100.) as u32, (scale * 100.) as u32);
    if !cx.has_global::<LogoCache>() {
        cx.set_global(LogoCache::default());
    }
    let cached = cx.global::<LogoCache>().0.get(&key).cloned();
    let image = match cached {
        Some(image) => Some(image),
        None => {
            let source = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/icons/muxus.svg"
            ));
            let sized = source.replacen(
                r#"width="1254" height="1254""#,
                &format!(r#"width="{size}" height="{size}""#),
                1,
            );
            let rendered = cx
                .svg_renderer()
                .render_single_frame(sized.as_bytes(), scale)
                .ok();
            if let Some(image) = &rendered {
                cx.global_mut::<LogoCache>().0.insert(key, image.clone());
            }
            rendered
        }
    };
    match image {
        Some(image) => gpui_kit::img(gpui_kit::ImageSource::Render(image))
            .size(px(size))
            .flex_none(),
        None => gpui_kit::img("icons/muxus.svg").size(px(size)).flex_none(),
    }
}
