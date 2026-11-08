//! The remote editor: a multi-file workspace on gpui-component's code editor
//! (`RemoteEditorWorkspace.tsx`, `MonacoTextEditor.tsx`, `editor/*`).

pub mod beautify;
pub mod css_format;
pub mod features;
pub mod formatting;
pub mod general_text;
pub mod goto_line;
pub mod json;
pub mod language;
pub mod links;
pub mod markers;
pub mod minimap;
pub mod occurrences;
pub mod overlays;
pub mod problem_widget;
pub mod registry;
pub mod syntax_problems;
pub mod workspace;
