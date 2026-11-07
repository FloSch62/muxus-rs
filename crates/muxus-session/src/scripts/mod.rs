//! Scripts on many SSH hosts: the library of saved scripts, a script's run on one live SSH
//! connection, and a job that runs one script over many hosts.
//!
//! | TypeScript | Here |
//! |---|---|
//! | `server/src/routes/scripts.ts` | the [`Backend`](crate::Backend) methods in [`routes`] |
//! | `server/src/scripts/script-runs.ts` | [`runs`] |
//! | `client/src/scripts/run-plan.ts` | [`plan`] |
//! | `client/src/scripts/script-jobs.ts` (the running part) | [`job`] |
//!
//! In TypeScript the window ran the job: it dialed each host over its own terminal socket and
//! polled the server for each run's output. Here the backend runs the whole job and hands the
//! window a stream of [`ScriptJobEvent`]s: hosts changing state, output as it arrives, and the
//! sign-in questions of the hosts' dials, which the window answers one at a time.

pub mod job;
pub mod plan;
pub mod routes;
pub mod runs;
mod wrapper;

pub use job::{
    ScriptJobEvent, ScriptJobHandle, ScriptJobRequest, ScriptPrompt, ScriptPromptAnswer,
    ScriptPromptRequest, ScriptTarget,
};
pub use plan::{HostOutcome, HostRunState, RunMode, run_job, run_waves};
pub use wrapper::{SCRIPT_WRAPPER, kill_job_command, script_command};
