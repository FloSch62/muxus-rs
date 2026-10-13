//! The native file dialogs, with what happens when the desktop has none.
//!
//! GPUI asks the desktop portal (`org.freedesktop.portal.FileChooser`) for its
//! dialogs. Without a working portal (a bare window manager, a broken portal
//! backend) the request fails at once, and the action that asked silently did
//! nothing: a download never landed anywhere. Chromium falls back to its own
//! GTK dialog there; Muxus saves into the suggested folder and says where, and
//! reports a picker it could not open.

use std::path::{Path, PathBuf};

use muxus_ui::gpui_kit::{App, PathPromptOptions};
use muxus_ui::theme::Status;

use crate::features::desktop::main_log::{MainLogLevel, main_log};
use crate::features::launcher::kit::{show_error_toast, show_toast};

/// Where a save goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// The user picked this path.
    Chosen(PathBuf),
    /// The user closed the dialog.
    Cancelled,
    /// No dialog could be shown: a free name in the suggested folder.
    Fallback(PathBuf),
}

impl SaveTarget {
    pub fn path(&self) -> Option<&Path> {
        match self {
            SaveTarget::Chosen(path) | SaveTarget::Fallback(path) => Some(path),
            SaveTarget::Cancelled => None,
        }
    }
}

/// Ask where to save `name`, starting in `directory`. Without a dialog the file goes to a
/// free name in `directory`, and a notification says where.
pub fn ask_save_path(
    directory: &Path,
    name: &str,
    cx: &mut App,
) -> impl std::future::Future<Output = SaveTarget> + use<> {
    let answer = cx.prompt_for_new_path(directory, Some(name));
    let directory = directory.to_path_buf();
    let name = name.to_string();
    let (tx, rx) = async_channel::bounded(1);
    cx.spawn(async move |cx| {
        let target = match answer.await {
            Ok(Ok(Some(path))) => SaveTarget::Chosen(path),
            Ok(Ok(None)) | Err(_) => SaveTarget::Cancelled,
            Ok(Err(error)) => {
                main_log(
                    MainLogLevel::Warn,
                    "the save dialog is unavailable; saving to the default folder",
                    Some(&error),
                );
                let path = free_path(&directory, &name);
                let message = format!("No save dialog is available; saving to {}", path.display());
                cx.update(|cx| show_toast(Status::Info, message, cx));
                SaveTarget::Fallback(path)
            }
        };
        let _ = tx.send(target).await;
    })
    .detach();
    async move { rx.recv().await.unwrap_or(SaveTarget::Cancelled) }
}

/// Ask for files or folders; `None` when the user cancels. A chooser that cannot open says
/// so in a notification.
pub fn pick_paths(
    options: PathPromptOptions,
    cx: &mut App,
) -> impl std::future::Future<Output = Option<Vec<PathBuf>>> + use<> {
    let answer = cx.prompt_for_paths(options);
    let (tx, rx) = async_channel::bounded(1);
    cx.spawn(async move |cx| {
        let paths = match answer.await {
            Ok(Ok(paths)) => paths,
            Err(_) => None,
            Ok(Err(error)) => {
                main_log(
                    MainLogLevel::Warn,
                    "the file chooser is unavailable",
                    Some(&error),
                );
                let message = format!("Could not open the file chooser: {error}");
                cx.update(|cx| show_error_toast(message, cx));
                None
            }
        };
        let _ = tx.send(paths).await;
    })
    .detach();
    async move { rx.recv().await.ok().flatten() }
}

/// `name` in `directory`, or `name (1)`, `name (2)`, … before the extension when taken.
pub fn free_path(directory: &Path, name: &str) -> PathBuf {
    let first = directory.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| directory.join(format!("{stem} ({n}){ext}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_free_name_beside_taken_ones() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(free_path(dir.path(), "log.txt"), dir.path().join("log.txt"));
        std::fs::write(dir.path().join("log.txt"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "log.txt"),
            dir.path().join("log (1).txt")
        );
        std::fs::write(dir.path().join("log (1).txt"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "log.txt"),
            dir.path().join("log (2).txt")
        );
        std::fs::write(dir.path().join("README"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), "README"),
            dir.path().join("README (1)")
        );
        std::fs::write(dir.path().join(".bashrc"), "").unwrap();
        assert_eq!(
            free_path(dir.path(), ".bashrc"),
            dir.path().join(".bashrc (1)")
        );
    }
}
