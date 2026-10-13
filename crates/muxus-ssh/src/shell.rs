//! Bridges an SSH session channel to the terminal byte-stream contract
//! ([`muxus_core::stream::TransportChannels`]).

use muxus_core::stream::{
    BreakOutcome, TransportChannels, TransportEnds, TransportEvent, TransportInput,
};
use russh::ChannelMsg;

use crate::connection::manager::{ShellTransport, SshSessionSummary, TerminalShell};
use crate::connection::session::SessionChannel;

/// Default queue depth between the terminal and the SSH channel.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// An open terminal session as the terminal sees it.
pub struct ShellStream {
    pub channels: TransportChannels,
    pub summary: SshSessionSummary,
    pub transport: ShellTransport,
    /// The transport this session runs on (forwards, SFTP and stats attach to it by id).
    pub connection_id: String,
}

impl TerminalShell {
    /// Starts pumping bytes between the channel and the terminal. The session keeps its
    /// connection lease until the channel closes.
    pub fn into_stream(self, capacity: usize) -> ShellStream {
        let (channels, ends) = TransportChannels::pair(capacity);
        let TerminalShell {
            lease,
            channel,
            transport,
            summary,
        } = self;
        let connection_id = lease.connection().id.clone();
        let run = pump(channel, ends);
        tokio::spawn(async move {
            run.await;
            // The lease ends with the session.
            drop(lease);
        });
        ShellStream {
            channels,
            summary,
            transport,
            connection_id,
        }
    }
}

/// Why a BREAK cannot go out on an SSH channel yet.
pub const BREAK_UNSUPPORTED: &str = "the SSH engine cannot send BREAK requests yet";

/// Runs a session channel against the transport-facing ends until either side closes.
///
/// The feedback counts each input once the channel window has taken it, and answers every BREAK
/// (RFC 4335 `break` is not available in the SSH engine, so each one fails with
/// [`BREAK_UNSUPPORTED`]).
/// The feedback is declared when `pump` is called, so the terminal side sees it before the
/// returned future first runs.
pub fn pump(
    channel: SessionChannel,
    ends: TransportEnds,
) -> impl std::future::Future<Output = ()> + Send {
    ends.feedback.track_written_input();
    ends.feedback.report_breaks();
    let feedback = ends.feedback.clone();
    async move {
        run_pump(channel, ends).await;
        feedback.close();
    }
}

async fn run_pump(mut channel: SessionChannel, ends: TransportEnds) {
    let mut exit_code: Option<i32> = None;
    let mut exit_signal: Option<String> = None;
    loop {
        tokio::select! {
            input = ends.input.recv() => match input {
                Ok(TransportInput::Data(bytes)) => {
                    if channel.channel.data_bytes(bytes).await.is_err() {
                        let _ = ends.events.send(TransportEvent::Error("the SSH channel is closed".into())).await;
                        return;
                    }
                    ends.feedback.input_written();
                }
                Ok(TransportInput::Resize(size)) => {
                    let _ = channel
                        .channel
                        .window_change(
                            u32::from(size.cols),
                            u32::from(size.rows),
                            u32::from(size.pixel_width),
                            u32::from(size.pixel_height),
                        )
                        .await;
                }
                Ok(TransportInput::Break { duration_ms }) => {
                    // RFC 4335 break needs a channel request the SSH engine does not expose yet.
                    tracing::warn!(duration_ms, "SSH break is not supported by this SSH engine");
                    ends.feedback.break_sent(BreakOutcome::Failed(BREAK_UNSUPPORTED.into()));
                }
                Ok(TransportInput::Close) | Err(_) => {
                    let _ = channel.channel.eof().await;
                    let _ = channel.channel.close().await;
                    let _ = ends.events.send(TransportEvent::Exit { code: exit_code, signal: exit_signal }).await;
                    return;
                }
            },
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if ends.events.send(TransportEvent::Data(data.to_vec())).await.is_err() {
                        let _ = channel.channel.close().await;
                        return;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = i32::try_from(exit_status).ok(),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => exit_signal = Some(signal_name_text(&signal_name)),
                Some(ChannelMsg::Close) | None => {
                    let _ = ends.events.send(TransportEvent::Exit { code: exit_code, signal: exit_signal }).await;
                    return;
                }
                Some(_) => {}
            },
        }
    }
}

fn signal_name_text(signal: &russh::Sig) -> String {
    let debug = format!("{signal:?}");
    match debug
        .strip_prefix("Custom(\"")
        .and_then(|s| s.strip_suffix("\")"))
    {
        Some(custom) => custom.to_string(),
        None => debug,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use muxus_core::stream::{TerminalSize, TransportEvent, TransportInput};

    use super::*;
    use crate::connection::profile::SshProfile;
    use crate::connection::tests::{Sandbox, TestIo};
    use crate::testing::server::{ServerSpec, start};

    async fn next_data(events: &async_channel::Receiver<TransportEvent>, needle: &str) -> String {
        let mut out = String::new();
        let wait = async {
            while let Ok(event) = events.recv().await {
                if let TransportEvent::Data(bytes) = event {
                    out.push_str(&String::from_utf8_lossy(&bytes));
                    if out.contains(needle) {
                        break;
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .unwrap();
        out
    }

    #[tokio::test]
    async fn pumps_bytes_resizes_and_reports_the_exit_status() {
        let server = start(
            ServerSpec::new()
                .password("pw")
                .methods(&[russh::MethodKind::Password]),
        )
        .await;
        let sandbox = Sandbox::new();
        sandbox.write_config(&[
            "Host lab",
            "  HostName 127.0.0.1",
            "  User tester",
            &format!("  Port {}", server.port()),
            "  StrictHostKeyChecking accept-new",
        ]);
        let manager = sandbox.manager();
        let shell = manager
            .connect_shell(
                &SshProfile::new("lab"),
                TestIo::accepting("pw"),
                80,
                24,
                "xterm-256color",
                None,
                None,
            )
            .await
            .unwrap();
        let stream = shell.into_stream(DEFAULT_CHANNEL_CAPACITY);
        let TransportChannels { input, events, .. } = stream.channels;
        assert!(next_data(&events, "shell ok").await.contains("shell ok"));
        input
            .send(TransportInput::Data(b"ls\r".to_vec()))
            .await
            .unwrap();
        assert!(next_data(&events, "ls").await.contains("ls"));
        input
            .send(TransportInput::Resize(TerminalSize {
                cols: 120,
                rows: 40,
                pixel_width: 0,
                pixel_height: 0,
            }))
            .await
            .unwrap();
        input
            .send(TransportInput::Data(b"exit\r".to_vec()))
            .await
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await.unwrap() {
                    TransportEvent::Exit { code, .. } => return code,
                    _ => continue,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(exit, Some(3));
        assert!(
            server
                .events()
                .contains(&"window-change 120x40".to_string())
        );
        // The session's lease ended with it.
        let conn = manager.get(&stream.connection_id);
        if let Some(conn) = conn {
            tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .unwrap();
        }
    }
}
