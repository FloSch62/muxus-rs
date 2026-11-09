//! A terminal to a host set to "Use system OpenSSH": the computer's `ssh` under a PTY, its login
//! shared with the file browser and the status bar (the system OpenSSH branch of
//! `handleSession` in `server/src/ws/terminal-socket.ts`).

use std::sync::Arc;

use muxus_core::protocol::{ExitReason, SshProfile, TerminalClientMessage, TerminalServerMessage};
use muxus_core::stream::{TerminalSize, TransportEvent};
use muxus_local::pty::{LocalPtyPlan, home_dir, pty_environment, spawn_pty_program};
use muxus_ssh::system_openssh::{SSH_OWN_FAILURE, SSH_OWN_FAILURE_MESSAGE, SessionTools};

use super::{Finisher, LoggingControl, Session, clamp, lock};
use crate::logging::SessionRecorder;
use crate::login_sequence::LoginSequenceHost;
use crate::terminal::link::TransportLink;

/// `ssh` only sends BREAK through an escape sequence, which these sessions turn off.
const NO_BREAK: &str = "BREAK is not available with system OpenSSH.";

impl Session {
    pub(super) async fn system_ssh(
        self: &Arc<Self>,
        profile: SshProfile,
        cols: u32,
        rows: u32,
        recorder: SessionRecorder,
        handle_logging: LoggingControl,
    ) -> Result<(), String> {
        let shared = self.shared.clone();
        let environment = shared.local_environment();
        let tools = SessionTools {
            env: pty_environment(&environment),
            ..shared.connections.system_sessions().defaults()
        };
        let session = shared.connections.system_sessions().start(
            &profile,
            shared.connections.sftp_allowed_for(&profile),
            &tools,
        )?;
        if self.facade.is_closed() {
            session.close();
            return Ok(());
        }
        let plan = LocalPtyPlan {
            program: session.binary().display().to_string(),
            args: session.terminal_args(),
            cwd: home_dir(&environment.vars).unwrap_or_else(|| "/".into()),
            env: session.env().clone(),
        };
        let size = TerminalSize {
            cols: clamp(cols),
            rows: clamp(rows),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pty = match spawn_pty_program(plan, size) {
            Ok(pty) => pty,
            Err(error) => {
                session.terminal_ended();
                return Err(error.to_string());
            }
        };
        let (link, events) = TransportLink::new(pty.channels);
        {
            let link = link.clone();
            session.set_terminal_killer(move || link.close());
        }
        // A PTY takes a write at once.
        self.set_link_writers(&link, false);
        {
            let link = link.clone();
            let facade = self.facade.clone();
            self.control.set_on_message(move |message| {
                if handle_logging(&message) {
                    return;
                }
                match message {
                    TerminalClientMessage::Resize { cols, rows } => link.resize(cols, rows),
                    TerminalClientMessage::SendBreak => {
                        facade.send_message(TerminalServerMessage::BreakResult {
                            ok: false,
                            message: Some(NO_BREAK.into()),
                        })
                    }
                    _ => {}
                }
            });
        }
        // A container or console session never shows the host's own prompts.
        let login_host = if profile.environment.is_some() {
            None
        } else {
            match &profile.profile_id {
                Some(id) => Some(LoginSequenceHost::Profile(id.clone())),
                None => shared
                    .connections
                    .listed_metadata_alias_for(&profile)
                    .map(LoginSequenceHost::Alias),
            }
        };
        let login = self.prepare_login_sequence(login_host.clone(), &recorder);
        let finisher = Finisher::new(
            self.facade.clone(),
            recorder.clone(),
            format!("ssh {}", profile.target),
        );
        let pacer = lock(&self.io).pacer.clone();
        let anti_idle_recorder = recorder.clone();
        let pump = {
            let facade = self.facade.clone();
            let login = login.clone();
            let session = session.clone();
            async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        TransportEvent::Data(data) => {
                            recorder.output(&data);
                            if let Some(login) = &login {
                                login.output(&data);
                            }
                            if let Some(pacer) = &pacer {
                                pacer.output(&data);
                            }
                            facade.send_data(data);
                            facade.wait_for_room().await;
                        }
                        TransportEvent::Exit { code, .. } => {
                            session.terminal_ended();
                            if code == Some(SSH_OWN_FAILURE) {
                                finisher.finish(
                                    ExitReason::Failed,
                                    Some(SSH_OWN_FAILURE_MESSAGE.into()),
                                );
                            } else {
                                *lock(&finisher.exit_code) = code;
                                finisher.finish(ExitReason::Completed, None);
                            }
                            return;
                        }
                        TransportEvent::Error(message) => {
                            session.terminal_ended();
                            finisher.finish(ExitReason::Failed, Some(message));
                            return;
                        }
                    }
                }
                session.terminal_ended();
            }
        };
        {
            let link = link.clone();
            let session = session.clone();
            self.facade.on_close(move || {
                link.close();
                session.close();
            });
        }
        if let Some(login) = &login {
            login.start();
        }
        // `ssh` asks for passwords in the terminal itself, so, as on Telnet, the first wait
        // starts at the first input and nothing is typed into a login prompt.
        self.start_anti_idle(login_host, &anti_idle_recorder, true)
            .await;
        self.facade.send_message(TerminalServerMessage::Ready {
            conn_id: session.id.clone(),
            host: None,
            user: None,
            sftp_available: Some(session.sftp_available),
            system_open_ssh: Some(true),
            summary: None,
        });
        tokio::spawn(pump);
        tracing::info!(target = %profile.target, conn_id = %session.id, "system openssh session started");
        if profile.profile_id.is_some() {
            self.record_saved_host(profile.profile_id.clone()).await;
        } else if let Some(alias) = shared.connections.listed_metadata_alias_for(&profile)
            && let Err(error) = self
                .db(move |db| db.record_open_ssh_connection(&alias).map(drop))
                .await
        {
            tracing::warn!(%error, target = %profile.target, "could not record recent connection");
        }
        Ok(())
    }
}
