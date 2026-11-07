//! The window's script job (`useScriptJobStore`, `startScriptJob`, `cancelScriptJob` and
//! `rerunFailedHosts` in `client/src/scripts/script-jobs.ts`). The job runs in the backend; this
//! keeps what it reported, so closing the Scripts dialog does not stop a run and opening it
//! again shows where it is. The dial questions of its hosts go to the window as
//! [`ScriptJobsEvent::Prompt`], to be answered one at a time with every other question.

use std::rc::Rc;
use std::time::Instant;

use muxus_session::Backend;
use muxus_session::scripts::{
    ScriptJobEvent, ScriptJobHandle, ScriptJobRequest, ScriptPrompt, ScriptPromptAnswer,
    ScriptTarget,
};
use muxus_ui::gpui_kit::{Context, EventEmitter, Task};

use super::model::{JobInput, ScriptJob};
use crate::features::forwarding::start::{DialAnswer, DialPrompt};

/// What the job asks of the window.
pub enum ScriptJobsEvent {
    /// A host's dial asks a sign-in question; answer on `reply` (a dropped reply cancels).
    Prompt {
        /// Names the question in the window's queue.
        key: String,
        prompt: DialPrompt,
        reply: async_channel::Sender<DialAnswer>,
    },
    /// The question under `key` no longer needs an answer.
    Withdraw { key: String },
}

/// The window's current (or last) job.
pub struct ScriptJobs {
    backend: Backend,
    pub job: Option<ScriptJob>,
    handle: Option<Rc<ScriptJobHandle>>,
    next_id: u64,
    _pump: Option<Task<()>>,
}

impl EventEmitter<ScriptJobsEvent> for ScriptJobs {}

fn prompt_key(job: u64, prompt: u64) -> String {
    format!("script-prompt:{job}:{prompt}")
}

impl ScriptJobs {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            job: None,
            handle: None,
            next_id: 1,
            _pump: None,
        }
    }

    /// `jobRunning`.
    pub fn running(&self) -> bool {
        self.job.as_ref().is_some_and(ScriptJob::running)
    }

    /// Start a job, replacing a finished one (`startScriptJob`).
    pub fn start(&mut self, input: JobInput, cx: &mut Context<Self>) -> Result<(), String> {
        if self.running() {
            return Err("A script is still running. Cancel it or wait for it to finish.".into());
        }
        let handle = self
            .backend
            .start_script_job(ScriptJobRequest {
                body: input.body.clone(),
                targets: input.targets.clone(),
                mode: input.mode,
                stop_on_failure: input.stop_on_failure,
                timeout_seconds: input.timeout_seconds,
            })
            .map_err(|error| error.message)?;
        let id = self.next_id;
        self.next_id += 1;
        self.job = Some(ScriptJob::new(id, &input, Instant::now()));
        let handle = Rc::new(handle);
        self.handle = Some(handle.clone());
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = handle.next_event().await {
                let finished = matches!(event, ScriptJobEvent::Finished { .. });
                if this
                    .update(cx, |this, cx| this.on_event(id, event, cx))
                    .is_err()
                    || finished
                {
                    break;
                }
            }
        }));
        cx.notify();
        Ok(())
    }

    /// `cancelScriptJob`: no more hosts start; dials still connecting give up, their waiting
    /// questions included, and running scripts stop with everything they started.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.running() {
            return;
        }
        if let Some(job) = &mut self.job {
            job.cancelled = true;
        }
        if let Some(handle) = &self.handle {
            handle.cancel();
        }
        cx.notify();
    }

    /// Run the finished job's script again, unchanged, on the hosts where it failed
    /// (`rerunFailedHosts`). `targets` are the hosts as they are now.
    pub fn rerun_failed(
        &mut self,
        targets: &[ScriptTarget],
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(job) = self.job.as_ref().filter(|job| !job.running()) else {
            return Ok(());
        };
        let input = job.rerun_input(targets);
        self.start(input, cx)
    }

    fn on_event(&mut self, id: u64, event: ScriptJobEvent, cx: &mut Context<Self>) {
        match event {
            ScriptJobEvent::Prompt(request) => {
                let (reply, answer) = async_channel::bounded::<DialAnswer>(1);
                let prompt = match request.prompt.clone() {
                    ScriptPrompt::Auth(info) => DialPrompt::Auth(info),
                    ScriptPrompt::HostKey(challenge) => DialPrompt::HostKey(challenge),
                };
                // The window answers in its own terms; pass the answer on to the job.
                let back = request.reply.clone();
                cx.spawn(async move |_, _| {
                    let answer = match answer.recv().await {
                        Ok(DialAnswer::Auth(response)) => ScriptPromptAnswer::Auth(response),
                        Ok(DialAnswer::HostKey(accept)) => ScriptPromptAnswer::HostKey(accept),
                        Err(_) => match request.prompt {
                            ScriptPrompt::Auth(_) => ScriptPromptAnswer::Auth(None),
                            ScriptPrompt::HostKey(_) => ScriptPromptAnswer::HostKey(false),
                        },
                    };
                    let _ = back.try_send(answer);
                })
                .detach();
                cx.emit(ScriptJobsEvent::Prompt {
                    key: prompt_key(id, request.id),
                    prompt,
                    reply,
                });
            }
            ScriptJobEvent::PromptWithdrawn { id: prompt } => {
                cx.emit(ScriptJobsEvent::Withdraw {
                    key: prompt_key(id, prompt),
                });
            }
            event => {
                if let Some(job) = self.job.as_mut().filter(|job| job.id == id) {
                    job.apply(&event);
                    cx.notify();
                }
            }
        }
    }
}
