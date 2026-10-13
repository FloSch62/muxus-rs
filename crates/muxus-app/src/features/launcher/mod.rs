//! The quick launcher (`client/src/components/QuickLauncherDialog.tsx`,
//! `quick-launcher.ts`) and the host picker of empty panes
//! (`HostPickerPopover.tsx`).

pub mod catalog;
pub mod demo;
pub mod host_picker;
pub mod kit;
pub mod search;
pub mod sessions;
pub mod time_format;
pub mod view;

pub use host_picker::{HostPicker, HostPickerEvent, open_host_picker};
pub use view::{LauncherContext, LauncherEvent, QuickLauncher, open_quick_launcher};
