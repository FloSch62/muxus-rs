//! The remote file browser and the remote editor: `components/SftpPanel.tsx`,
//! `layout/SftpWindow.tsx`, the browser docked in the sidebar
//! (`layout/Sidebar.tsx`), `components/RemoteEditorWorkspace.tsx` and the
//! modules they use. Every file operation goes through
//! `muxus_session::Backend`.

pub mod dialogs;
pub mod docked;
pub mod editor;
pub mod format;
pub mod icons;
pub mod linux_apps;
pub mod local_copies;
pub mod local_open;
pub mod open_with;
pub mod palette;
pub mod panel;
pub mod placement;
pub mod services;
pub mod terminal_drop;
pub mod terminal_upload;
pub mod transfer;
pub mod window;
