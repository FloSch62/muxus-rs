//! The workbench's logic, without any UI (port of `client/src/management`):
//! request drafts, the data tree the explorer and result views share, live
//! telemetry tables, NETCONF XML helpers, Set review, "copy as" and the
//! gNOI/gNSI tool formats. A GPUI view renders this state.

pub mod connection;
pub mod controller;
pub mod copy_as;
pub mod data_tree;
pub mod files;
pub mod live_table;
pub mod requests;
pub mod set_review;
pub mod store;
pub mod tool_format;
pub mod xml;
