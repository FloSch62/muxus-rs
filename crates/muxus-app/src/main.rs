//! The Muxus desktop application.

// A release build opens no console window next to its own on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ui;

fn main() {
    // `muxus --web-window`: a saved web page's window, which the app starts as a process of
    // its own (see muxus_webview).
    if let Some(code) = muxus_webview::run_if_requested() {
        std::process::exit(code);
    }
    ui::run();
}
