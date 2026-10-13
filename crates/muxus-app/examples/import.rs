//! The session import dialog on a sandboxed backend with the documentation's
//! demo data. Nothing touches the real home directory.
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=review out.png -- \
//!     target-ui-b/debug/examples/import
//! ```
//!
//! `MUXUS_SCENE`: `mobaxterm` (default, choosing a source), `securecrt`,
//! `review` (a MobaXterm file with SSH and RDP sessions in folders and one
//! unsupported Telnet bookmark). `MUXUS_THEME=dark` for the dark variant.

#[path = "support/scene.rs"]
mod scene;

use muxus_app::features::import::dialog::SessionImportDialog;
use muxus_import::ImportVendor;
use muxus_ui::gpui_kit::AppContext;

/// The file the reference capture hands the dialog.
const MOBAXTERM: &str = "[Bookmarks]
SubRep=Production\\Europe
Edge router=#109#0%edge-01.example.net%22%admin%%rest
Web front=#109#0%web-front.example.net%2222%deploy%%rest

[Bookmarks_1]
SubRep=Lab
Build box=#109#0%build.lab.example%22%ci%3%rest
Windows desk=#91#4%desk.lab.example%3389%admin%0
Old telnet=#98#1%switch.lab.example%23%%%2%%22%%%0
";

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let (backend, runtime) = scene::demo_backend(scratch.path());
    let which = scene::scene("mobaxterm");
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(scene::mode(), cx);
            let backend = backend.clone();
            let which = which.clone();
            scene::open(cx, move |window, cx| {
                let vendor = if which == "securecrt" {
                    ImportVendor::SecureCrt
                } else {
                    ImportVendor::MobaXterm
                };
                let dialog = cx.new(|cx| {
                    let mut dialog = SessionImportDialog::new(backend, vendor, window, cx);
                    if which == "review" {
                        dialog.review_content(MOBAXTERM, "MobaXterm.ini", window, cx);
                    }
                    dialog
                });
                dialog.into()
            });
        });
    drop(runtime);
}
