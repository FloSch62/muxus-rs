//! Host editing: the host editor dialog for every connection kind, the
//! tunnel editor, bulk edit, organize, the folder dialog and the launch-group
//! dialog (`client/src/components/HostEditorDialog.tsx`, `host-editor/*`,
//! `NativeHostEditorContent.tsx`, `RemoteDesktopHostEditorContent.tsx`,
//! `ManagementHostEditorContent.tsx`, `TunnelEditorDialog.tsx`,
//! `HostBulkEditDialog.tsx`, `HostOrganizationDialog.tsx`,
//! `sidebar/FolderDialog.tsx`, `sidebar/LaunchGroupDialog.tsx`).
//!
//! The form logic (drafts, validation, the writes a save performs) lives in
//! [`logic`] as plain Rust with unit tests; the GPUI views build on it.

pub mod demo;
pub mod dialogs;
pub mod logic;
pub mod palette;
pub mod services;
pub mod view;
pub mod widgets;
