//! Notices around the terminal on a sandboxed backend: the release update
//! snackbar and the tab strip's "Rename tab" dialog. Nothing touches the real
//! home directory.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=rename out.png -- \
//!     target-ui-b/debug/examples/notices
//! ```
//!
//! `MUXUS_SCENE`: `update` (default, Muxus 0.12.0 is out), `rename` (the
//! dialog for a tab titled `MUXUS_TITLE`, "Local" by default).
//! `MUXUS_THEME=dark` for the dark variant.

#[path = "support/scene.rs"]
mod scene;

use muxus_app::features::notices::tab_strip::RenameTabDialog;
use muxus_app::features::notices::update_view::UpdateNotification;
use muxus_ui::gpui_kit::AppContext;

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let (backend, runtime) = scene::demo_backend(scratch.path());
    let which = scene::scene("update");
    let title = std::env::var("MUXUS_TITLE").unwrap_or_else(|_| "Local".into());
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(scene::mode(), cx);
            let backend = backend.clone();
            let which = which.clone();
            let title = title.clone();
            scene::open(cx, move |window, cx| match which.as_str() {
                "rename" => cx
                    .new(|cx| RenameTabDialog::new("tab-local", title, window, cx))
                    .into(),
                _ => cx
                    .new(|cx| {
                        let mut notice = UpdateNotification::new(backend, true);
                        notice.show_release(
                            "0.11.1",
                            "0.12.0",
                            "https://github.com/FloSch62/muxus/releases/tag/v0.12.0",
                            cx,
                        );
                        notice
                    })
                    .into(),
            });
        });
    drop(runtime);
}
