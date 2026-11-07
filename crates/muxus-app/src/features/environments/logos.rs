//! The tools' own logos (`components/environments/environment-icons.tsx`), in
//! their own colors. They are content, not Material icons: drawn as images
//! from `assets/icons/env-*.svg`, rasterized once per size and window scale so
//! they are as crisp as the browser draws them.

use std::collections::HashMap;
use std::sync::Arc;

use muxus_ui::gpui_kit::{App, Global, ImageSource, Img, RenderImage, Styled, Window, img};
use muxus_ui::theme::{ActiveTheme, Mode};

use super::model::EnvironmentKind;

macro_rules! logo {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icons/env-",
            $name,
            ".svg"
        ))
    };
}

/// The project's own mark, and its variant for the dark theme where it has one.
fn source(kind: EnvironmentKind, dark: bool) -> &'static str {
    match kind {
        EnvironmentKind::Docker => logo!("docker"),
        EnvironmentKind::Podman => logo!("podman"),
        EnvironmentKind::Containerlab if dark => logo!("containerlab-dark"),
        EnvironmentKind::Containerlab => logo!("containerlab"),
        EnvironmentKind::Kubernetes => logo!("kubernetes"),
        EnvironmentKind::Proxmox => logo!("proxmox"),
        EnvironmentKind::Libvirt => logo!("libvirt"),
    }
}

fn asset_path(kind: EnvironmentKind, dark: bool) -> &'static str {
    match kind {
        EnvironmentKind::Docker => "icons/env-docker.svg",
        EnvironmentKind::Podman => "icons/env-podman.svg",
        EnvironmentKind::Containerlab if dark => "icons/env-containerlab-dark.svg",
        EnvironmentKind::Containerlab => "icons/env-containerlab.svg",
        EnvironmentKind::Kubernetes => "icons/env-kubernetes.svg",
        EnvironmentKind::Proxmox => "icons/env-proxmox.svg",
        EnvironmentKind::Libvirt => "icons/env-libvirt.svg",
    }
}

/// The SVG with its root sized to `size` px; the view box scales into it.
pub fn sized_svg(svg: &str, size: f32) -> String {
    let Some(start) = svg.find("<svg") else {
        return svg.to_string();
    };
    let Some(end) = svg[start..].find('>').map(|end| start + end) else {
        return svg.to_string();
    };
    let mut root = svg[start..end].to_string();
    for attribute in ["width", "height"] {
        let needle = format!(" {attribute}=\"");
        if let Some(at) = root.find(&needle) {
            let value_start = at + needle.len();
            if let Some(close) = root[value_start..].find('"') {
                root.replace_range(at..value_start + close + 1, "");
            }
        }
    }
    let root = root.trim_end_matches('/');
    let closing = if svg[..end].ends_with('/') { "/" } else { "" };
    format!(
        "{}{root} width=\"{size}\" height=\"{size}\"{closing}{}",
        &svg[..start],
        &svg[end..]
    )
}

#[derive(Default)]
struct LogoCache(HashMap<(EnvironmentKind, bool, u32, u32), Arc<RenderImage>>);

impl Global for LogoCache {}

/// A tool's logo, decorative: the line or tooltip beside it names the tool (`EnvironmentLogo`).
/// Muxus's theme picks the variant, not the system's: a mark's own media query would follow
/// the OS.
pub fn environment_logo(kind: EnvironmentKind, size: f32, window: &Window, cx: &mut App) -> Img {
    let dark = cx.theme().mode == Mode::Dark;
    let scale = window.scale_factor();
    let key = (kind, dark, (size * 100.) as u32, (scale * 100.) as u32);
    if !cx.has_global::<LogoCache>() {
        cx.set_global(LogoCache::default());
    }
    let cached = cx.global::<LogoCache>().0.get(&key).cloned();
    let image = cached.or_else(|| {
        let sized = sized_svg(source(kind, dark), size);
        let rendered = cx
            .svg_renderer()
            .render_single_frame(sized.as_bytes(), scale)
            .ok()?;
        cx.global_mut::<LogoCache>().0.insert(key, rendered.clone());
        Some(rendered)
    });
    let element = match image {
        Some(image) => img(ImageSource::Render(image)),
        None => img(asset_path(kind, dark)),
    };
    element.size(muxus_ui::zoom::px(size)).flex_none()
}
