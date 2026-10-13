//! Embedded assets: the Material icons and the Muxus logo under
//! `assets/icons` (see `hack/extract_icons.mjs`), served to GPUI's `svg()` and
//! `img()` elements as `icons/<Name>.svg`. Paths that are not ours fall through
//! to gpui-component's own icon set.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../assets"]
#[include = "icons/*.svg"]
struct Embedded;

/// The asset source the application registers with `Application::with_assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = Embedded::get(path) {
            return Ok(Some(file.data));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = Embedded::iter()
            .filter(|name| name.starts_with(path))
            .map(|name| SharedString::from(name.to_string()))
            .collect();
        names.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(names)
    }
}
