//! Saved workspaces: the dialog (`client/src/components/WorkspaceDialog.tsx`),
//! the catalog list (`workspace-list.ts`) and the cross-window sync
//! (`workspace-sync.ts`).

pub mod layout;
pub mod list;
pub mod model;
pub mod sync;
pub mod view;

pub use model::{WorkspaceInitialSelection, WorkspaceLayoutHost, Workspaces, WorkspacesState};
pub use sync::WorkspaceBus;
pub use view::{WorkspaceDialog, WorkspaceDialogContext, WorkspaceDialogEvent, open_workspaces};
