//! Scripts on many hosts (`components/ScriptsDialog.tsx`, `client/src/scripts`): the Scripts
//! dialog, the window's job and what opens Scripts for a host, a selection or a folder. The job
//! itself runs in the backend (`muxus_session::scripts`).

pub mod dialog;
pub mod jobs;
pub mod model;

pub use dialog::{ScriptsDialog, ScriptsDialogEvent, open_scripts_dialog};
pub use jobs::{ScriptJobs, ScriptJobsEvent};
pub use model::{OpenScripts, is_script_host, open_scripts_for, script_target};
