//! Saving and opening files from the settings (exports, imports): the
//! desktop save-as and open dialogs.

use std::path::PathBuf;

use muxus_ui::gpui_kit::{Context, PathPromptOptions};
use muxus_ui::theme::Status;

use super::window::SettingsWindow;

fn default_directory() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

impl SettingsWindow {
    /// `saveTextFile`: ask where, then write `text` there.
    pub fn save_text_file(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        self.save_text_file_then(name, text, cx, |_, _| {});
    }

    /// Like [`SettingsWindow::save_text_file`], running `saved` once written.
    pub fn save_text_file_then(
        &mut self,
        name: String,
        text: String,
        cx: &mut Context<Self>,
        saved: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let answer = crate::util::file_dialog::ask_save_path(&default_directory(), &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = answer.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            let written = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| match written {
                Ok(()) => saved(this, cx),
                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
            });
        })
        .detach();
    }

    /// Ask for one file to open, then hand its path to `chosen`.
    pub fn pick_file(
        &mut self,
        cx: &mut Context<Self>,
        chosen: impl FnOnce(&mut Self, PathBuf, &mut Context<Self>) + 'static,
    ) {
        let answer = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(paths) = answer.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                chosen(this, path, cx);
                cx.notify();
            });
        })
        .detach();
    }
}
