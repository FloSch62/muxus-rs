//! The remote editor: a multi-file workspace on gpui-component's code editor
//! (`RemoteEditorWorkspace.tsx`, `MonacoTextEditor.tsx`, `editor/*`).

pub mod features;
pub mod general_text;
pub mod goto_line;
pub mod json;
pub mod language;
pub mod links;
pub mod occurrences;
pub mod overlays;
pub mod registry;
pub mod workspace;
