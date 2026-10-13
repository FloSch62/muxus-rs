//! Muxus application logic: the host catalog model and where it comes from,
//! the sidebar tree, tabs and pane layout, terminal session links and their
//! recovery rules, and the keymap. Everything in this library is free of UI
//! types, so it can be tested headlessly; the GPUI views live in the `muxus`
//! binary.

pub mod data_source;
pub mod export;
pub mod features;
pub mod hosts;
pub mod keymap;
pub mod model;
pub mod session;
pub mod sidebar;
pub mod sidebar_width;
pub mod state;
pub mod util;
