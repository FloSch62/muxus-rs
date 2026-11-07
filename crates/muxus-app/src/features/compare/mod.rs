//! Compare two texts (`CompareDialog.tsx`, `compare-sources.ts`,
//! `text-compare.ts`): "Compare with…" in session history, the terminal's
//! context menu and the file browser opens a dialog on a diff of the text it
//! was chosen on against a second one: a terminal's output or selection, a
//! history record, or a local or SFTP file.
//!
//! Hook points for the window:
//! - [`install`] once with the backend and how a window opens the dialog
//!   (it mounts [`view::CompareDialog`] over the active main window).
//! - [`open_compare`] from anywhere with the first side; a side still loading
//!   when another is opened is dropped.

pub mod rows;
pub mod sources;
pub mod text;
pub mod view;

use std::future::Future;
use std::rc::Rc;

use muxus_session::Backend;
use muxus_ui::gpui_kit::{App, Global};
use muxus_ui::theme::Status;

use crate::features::launcher::kit::show_toast;
use sources::{CompareText, comparable};

/// Opens the dialog in the active window with its first side.
pub type OpenHook = Rc<dyn Fn(CompareText, &mut App)>;

/// What the compare feature shares across windows.
#[derive(Clone)]
struct CompareServices {
    backend: Backend,
    open: OpenHook,
    /// `latestOpen`: the newest request; older ones are dropped when they land.
    latest: u64,
}

impl Global for CompareServices {}

/// Install the feature: the backend its sources read from and how a window
/// opens the dialog.
pub fn install(backend: Backend, open: impl Fn(CompareText, &mut App) + 'static, cx: &mut App) {
    cx.set_global(CompareServices {
        backend,
        open: Rc::new(open),
        latest: 0,
    });
}

/// The backend the sources read from, once installed.
pub fn backend(cx: &App) -> Option<Backend> {
    cx.try_global::<CompareServices>()
        .map(|services| services.backend.clone())
}

/// `openCompare`: open the dialog with the side it was invoked from. A side
/// still loading when another is opened is dropped, so a slow fetch cannot
/// replace the text the dialog is already showing.
pub fn open_compare<F>(load: impl FnOnce(Backend) -> F, cx: &mut App)
where
    F: Future<Output = Result<CompareText, String>> + 'static,
{
    let Some(services) = cx.try_global::<CompareServices>().cloned() else {
        return;
    };
    let request = services.latest + 1;
    cx.global_mut::<CompareServices>().latest = request;
    let future = load(services.backend.clone());
    cx.spawn(async move |cx| {
        let side = future.await.and_then(comparable);
        cx.update(|cx| {
            let latest = cx
                .try_global::<CompareServices>()
                .is_some_and(|services| services.latest == request);
            if !latest {
                return;
            }
            match side {
                Ok(side) => (services.open)(side, cx),
                Err(message) => show_toast(Status::Error, message, cx),
            }
        });
    })
    .detach();
}

/// [`open_compare`] with a side already at hand (a terminal's text).
pub fn open_compare_text(side: CompareText, cx: &mut App) {
    open_compare(move |_| async move { Ok(side) }, cx);
}
