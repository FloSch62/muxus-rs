//! The NETCONF and gNMI workbench (port of `client/src/components/ManagementViewImpl.tsx`
//! and `client/src/components/management/*`).
//!
//! [`open_management_view`] opens the view for a gNMI or NETCONF profile; it
//! connects through [`muxus_session::Backend::open_management_session`] and
//! reports what the tab and the window need through [`ManagementViewEvent`].

mod actions;
mod bars;
mod editor;
mod fields;
mod live;
pub mod logic;
mod navigator;
mod overlays;
mod palette;
mod path_text;
mod results;
mod scene;
mod tools;
mod tree_view;
mod ui;
mod view;
mod widgets;
mod workbench;

pub use view::{
    ManagementPrompt, ManagementView, ManagementViewEvent, Redial, SessionPhase, TabStatus,
    open_management_view,
};
