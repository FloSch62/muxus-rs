//! Shared types, protocol and pure helpers (port of shared/src).
//!
//! Every type here serializes to exactly the JSON the TypeScript build reads
//! and writes (camelCase fields, absent optionals left out, the same enum
//! tags), because that JSON is stored in SQLite and preference files both
//! builds share.

// The enums here mirror JSON unions variant for variant; boxing the large
// variants would only add indirection to short-lived messages.
#![allow(clippy::large_enum_variant)]

pub mod api;
pub mod collate;
pub mod connection_links;
pub mod folder_paths;
pub mod gnmi_path;
pub mod js;
pub mod management;
pub mod paste_pacing;
pub mod platform;
pub mod protocol;
pub mod session_log_file;
pub mod session_transcript;
pub mod shell_integration;
pub mod stream;
pub mod update_version;
pub mod validate;
