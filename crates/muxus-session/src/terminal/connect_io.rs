//! The SSH connection's prompts over the session's own messages (the `io` of `handleSession`).

use std::sync::Arc;

use muxus_core::protocol::{
    AuthPromptInfo, AuthPromptResponse, HostKeyChallenge, TerminalClientMessage,
    TerminalServerMessage,
};
use muxus_ssh::connection::{BoxFuture, Cancelled, ConnectIo};

use super::control::ControlChannel;
use super::facade::Facade;
use crate::logging::SessionRecorder;

/// Status lines, auth prompts and host-key challenges of a connecting SSH session, sent to the
/// window; the answer is the next control message.
pub(crate) struct SessionConnectIo {
    pub facade: Arc<Facade>,
    pub control: Arc<ControlChannel>,
    pub recorder: Option<SessionRecorder>,
}

impl ConnectIo for SessionConnectIo {
    fn status(&self, message: &str, transient: bool) {
        if let Some(recorder) = &self.recorder {
            recorder.system(message);
        }
        self.facade.send_message(TerminalServerMessage::Status {
            message: message.to_string(),
            transient: transient.then_some(true),
        });
    }

    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>> {
        Box::pin(async move {
            let reply = self.control.claim_next();
            self.facade
                .send_message(TerminalServerMessage::AuthPrompt(info));
            match reply.await {
                Ok(TerminalClientMessage::AuthResponse(response)) => Ok(response),
                _ => Err(Cancelled),
            }
        })
    }

    fn host_key(&self, challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>> {
        Box::pin(async move {
            let reply = self.control.claim_next();
            self.facade
                .send_message(TerminalServerMessage::HostKey(challenge));
            match reply.await {
                Ok(TerminalClientMessage::HostKeyResponse { accept }) => Ok(accept),
                Ok(_) => Ok(false),
                Err(_) => Err(Cancelled),
            }
        })
    }
}
