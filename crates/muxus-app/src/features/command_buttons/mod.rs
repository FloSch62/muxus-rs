//! Command buttons and multi-execution: the action bar (`ActionBar.tsx`),
//! the keyboard menu (`CommandButtonMenu.tsx`), the editor dialog
//! (`CommandButtonsDialog.tsx`), the groups (`command-button-groups.ts`) and
//! mirrored input (`MultiExecControl.tsx`, `state/multi-exec.ts`).

pub mod action_bar;
pub mod dialog;
pub mod groups;
pub mod menu;
pub mod model;
pub mod multi_exec;
pub mod multi_exec_control;
pub mod palette;
pub mod run;

pub use action_bar::{ActionBar, ActionBarEvent, action_bar};
pub use dialog::{CommandButtonsDialog, CommandButtonsEvent, open_command_buttons};
pub use menu::{CommandButtonMenu, CommandMenuEvent, open_command_menu};
pub use model::{CommandButtons, MultiExec};
pub use multi_exec_control::{MultiExecControl, multi_exec_control};
pub use run::{CommandHooks, set_command_hooks};
