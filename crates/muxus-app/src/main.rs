//! The Muxus desktop application.

// A release build opens no console window next to its own on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ui;

fn main() {
    ui::run();
}
