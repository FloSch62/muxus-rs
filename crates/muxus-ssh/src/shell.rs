//! Bridges an SSH session channel to the terminal byte-stream contract
//! ([`muxus_core::stream::TransportChannels`]).

use std::collections::VecDeque;
use std::time::Duration;

use muxus_core::stream::{
    BreakOutcome, TransportChannels, TransportEnds, TransportEvent, TransportInput,
};
use russh::ChannelMsg;
use tokio::time::Instant;

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

/// How long a BREAK waits for the server's answer beyond the break itself: long enough for a
/// console server that answers only after holding the break.
pub const BREAK_REPLY_GRACE: Duration = Duration::from_secs(5);

/// The answer for a server that sent `SSH_MSG_CHANNEL_FAILURE`.
pub const BREAK_REFUSED: &str = "The SSH server refused the BREAK request";
/// The answer for a server that did not reply within the break and [`BREAK_REPLY_GRACE`].
pub const BREAK_UNANSWERED: &str = "The SSH server did not answer the BREAK request";
/// Why a BREAK could not go out at all.
pub const CHANNEL_CLOSED: &str = "the SSH channel is closed";

/// Runs a session channel against the transport-facing ends until either side closes.
///
/// The feedback counts each input once the channel window has taken it, and answers every BREAK
/// with the server's reply to an RFC 4335 `break` request (`channel-break.ts`).
/// The feedback is declared when `pump` is called, so the terminal side sees it before the
/// returned future first runs.
pub fn pump(
    channel: SessionChannel,
    ends: TransportEnds,
) -> impl std::future::Future<Output = ()> + Send {
    pump_with_reply_grace(channel, ends, BREAK_REPLY_GRACE)
}

fn pump_with_reply_grace(
    channel: SessionChannel,
    ends: TransportEnds,
    reply_grace: Duration,
) -> impl std::future::Future<Output = ()> + Send {
    ends.feedback.track_written_input();
    ends.feedback.report_breaks();
    let feedback = ends.feedback.clone();
    async move {
        let mut breaks = BreakReplies::new(reply_grace);
        run_pump(channel, &ends, &mut breaks).await;
        // A BREAK still waiting for its reply will not get one now.
        for _ in 0..breaks.unsettled() {
            feedback.break_sent(BreakOutcome::Failed(CHANNEL_CLOSED.into()));
        }
        feedback.close();
    }
}

async fn run_pump(mut channel: SessionChannel, ends: &TransportEnds, breaks: &mut BreakReplies) {
    let mut exit_code: Option<i32> = None;
    let mut exit_signal: Option<String> = None;
    loop {
        let deadline = breaks.next_deadline();
        tokio::select! {
            input = ends.input.recv() => match input {
                Ok(TransportInput::Data(bytes)) => {
                    if channel.channel.data_bytes(bytes).await.is_err() {
                        let _ = ends.events.send(TransportEvent::Error(CHANNEL_CLOSED.into())).await;
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
                    // want-reply is set, so a server that cannot pass the BREAK on (OpenSSH
                    // without a pty, for one) says so.
                    match channel.channel.request_break(true, duration_ms).await {
                        Ok(()) => breaks.sent(Instant::now(), duration_ms),
                        Err(_) => ends.feedback.break_sent(BreakOutcome::Failed(CHANNEL_CLOSED.into())),
                    }
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
                // Every want-reply request before the shell started had its reply awaited, so
                // a reply now answers a BREAK.
                Some(ChannelMsg::Success) => {
                    if let Some(outcome) = breaks.replied(true) {
                        ends.feedback.break_sent(outcome);
                    }
                }
                Some(ChannelMsg::Failure) => {
                    if let Some(outcome) = breaks.replied(false) {
                        ends.feedback.break_sent(outcome);
                    }
                }
                Some(ChannelMsg::Close) | None => {
                    let _ = ends.events.send(TransportEvent::Exit { code: exit_code, signal: exit_signal }).await;
                    return;
                }
                Some(_) => {}
            },
            () = sleep_until(deadline) => {
                for outcome in breaks.expire(Instant::now()) {
                    ends.feedback.break_sent(outcome);
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// BREAK requests waiting for the server's reply, oldest first.
///
/// Replies to channel requests come in the order the requests went out (RFC 4254 5.4). A
/// BREAK the server does not answer in time is reported unanswered but stays queued, so a late
/// reply still lines up with the request it answers instead of the next one.
#[derive(Debug)]
struct BreakReplies {
    reply_grace: Duration,
    pending: VecDeque<PendingBreak>,
}

#[derive(Debug)]
struct PendingBreak {
    deadline: Instant,
    /// Already reported (as unanswered).
    settled: bool,
}

impl BreakReplies {
    fn new(reply_grace: Duration) -> Self {
        Self {
            reply_grace,
            pending: VecDeque::new(),
        }
    }

    /// A `break` request went out at `now`.
    fn sent(&mut self, now: Instant, duration_ms: u32) {
        let mut deadline = now + Duration::from_millis(u64::from(duration_ms)) + self.reply_grace;
        // Outcomes are reported in request order, so no BREAK gives up before an older one.
        if let Some(last) = self.pending.back() {
            deadline = deadline.max(last.deadline);
        }
        self.pending.push_back(PendingBreak {
            deadline,
            settled: false,
        });
    }

    /// The server replied to the oldest outstanding request; the outcome to report, if that
    /// BREAK was still waiting.
    fn replied(&mut self, accepted: bool) -> Option<BreakOutcome> {
        let request = self.pending.pop_front()?;
        if request.settled {
            return None;
        }
        Some(if accepted {
            BreakOutcome::Sent
        } else {
            BreakOutcome::Declined(BREAK_REFUSED.into())
        })
    }

    /// When the oldest waiting BREAK gives up.
    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .find(|request| !request.settled)
            .map(|request| request.deadline)
    }

    /// Reports every waiting BREAK whose time is up as unanswered.
    fn expire(&mut self, now: Instant) -> Vec<BreakOutcome> {
        let mut outcomes = Vec::new();
        for request in self.pending.iter_mut().filter(|request| !request.settled) {
            if request.deadline > now {
                break;
            }
            request.settled = true;
            outcomes.push(BreakOutcome::Declined(BREAK_UNANSWERED.into()));
        }
        outcomes
    }

    /// BREAKs still waiting for an outcome.
    fn unsettled(&self) -> usize {
        self.pending
            .iter()
            .filter(|request| !request.settled)
            .count()
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
    use crate::connection::manager::MuxedConnectionLease;
    use crate::connection::profile::SshProfile;
    use crate::connection::tests::{Sandbox, TestIo};
    use crate::testing::server::{BreakAnswer, ServerSpec, TestServer, start};

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

    // --- RFC 4335 break (channel-break.test.ts) -------------------------------------------

    /// A shell on a server that answers `break` requests as told, pumped with `reply_grace`.
    struct BreakShell {
        server: TestServer,
        channels: TransportChannels,
        _lease: MuxedConnectionLease,
        _sandbox: Sandbox,
    }

    async fn break_shell(answers: &[BreakAnswer], reply_grace: Duration) -> BreakShell {
        let server = start(ServerSpec {
            break_answers: answers.to_vec(),
            ..ServerSpec::new()
                .password("pw")
                .methods(&[russh::MethodKind::Password])
        })
        .await;
        let sandbox = Sandbox::new();
        sandbox.write_config(&[
            "Host console",
            "  HostName 127.0.0.1",
            "  User console",
            &format!("  Port {}", server.port()),
            "  StrictHostKeyChecking accept-new",
        ]);
        let shell = sandbox
            .manager()
            .connect_shell(
                &SshProfile::new("console"),
                TestIo::accepting("pw"),
                80,
                24,
                "xterm-256color",
                None,
                None,
            )
            .await
            .unwrap();
        let (channels, ends) = TransportChannels::pair(DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(pump_with_reply_grace(shell.channel, ends, reply_grace));
        assert!(
            next_data(&channels.events, "shell ok")
                .await
                .contains("shell ok")
        );
        BreakShell {
            server,
            channels,
            _lease: shell.lease,
            _sandbox: sandbox,
        }
    }

    async fn break_outcome(shell: &BreakShell, duration_ms: u32) -> Option<BreakOutcome> {
        shell
            .channels
            .input
            .send(TransportInput::Break { duration_ms })
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(10),
            shell.channels.feedback.break_outcome(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn sends_an_rfc_4335_break_with_its_length_and_want_reply_set() {
        let shell = break_shell(&[BreakAnswer::Accept], BREAK_REPLY_GRACE).await;
        assert!(shell.channels.feedback.reports_breaks());

        // Only a want-reply request is answered with SSH_MSG_CHANNEL_SUCCESS.
        assert_eq!(break_outcome(&shell, 250).await, Some(BreakOutcome::Sent));
        assert_eq!(shell.server.breaks(), vec![250]);

        // The session keeps working after the request, and a BREAK is never terminal input.
        shell
            .channels
            .input
            .send(TransportInput::Data(b"still here".to_vec()))
            .await
            .unwrap();
        assert!(
            next_data(&shell.channels.events, "still here")
                .await
                .contains("still here")
        );
    }

    #[tokio::test]
    async fn reports_a_server_that_refuses_the_request() {
        let shell = break_shell(
            &[BreakAnswer::Refuse, BreakAnswer::Accept],
            BREAK_REPLY_GRACE,
        )
        .await;

        assert_eq!(
            break_outcome(&shell, 1_500).await,
            Some(BreakOutcome::Declined(BREAK_REFUSED.into()))
        );
        assert_eq!(shell.server.breaks(), vec![1_500]);
        // The next one is answered on its own.
        assert_eq!(break_outcome(&shell, 250).await, Some(BreakOutcome::Sent));
    }

    #[tokio::test]
    async fn gives_up_on_a_server_that_never_answers() {
        let shell = break_shell(&[BreakAnswer::Ignore], Duration::from_millis(50)).await;

        assert_eq!(
            break_outcome(&shell, 10).await,
            Some(BreakOutcome::Declined(BREAK_UNANSWERED.into()))
        );
        assert_eq!(shell.server.breaks(), vec![10]);
    }

    #[tokio::test]
    async fn a_break_still_waiting_when_the_channel_closes_fails() {
        let shell = break_shell(&[BreakAnswer::Ignore], BREAK_REPLY_GRACE).await;
        shell
            .channels
            .input
            .send(TransportInput::Break { duration_ms: 250 })
            .await
            .unwrap();
        // The server ends the shell before it ever answers.
        shell
            .channels
            .input
            .send(TransportInput::Data(b"exit\r".to_vec()))
            .await
            .unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(3),
            shell.channels.feedback.break_outcome(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Some(BreakOutcome::Failed(CHANNEL_CLOSED.into())));
        // No more outcomes once the transport ended.
        assert_eq!(shell.channels.feedback.break_outcome().await, None);
    }

    #[test]
    fn a_late_reply_lines_up_with_the_request_it_answers() {
        let start = Instant::now();
        let mut breaks = BreakReplies::new(Duration::from_secs(5));
        breaks.sent(start, 250);
        assert_eq!(
            breaks.next_deadline(),
            Some(start + Duration::from_millis(5_250))
        );
        assert!(breaks.expire(start + Duration::from_secs(5)).is_empty());
        assert_eq!(
            breaks.expire(start + Duration::from_millis(5_250)),
            vec![BreakOutcome::Declined(BREAK_UNANSWERED.into())]
        );
        assert_eq!(breaks.next_deadline(), None);
        assert_eq!(breaks.unsettled(), 0);

        breaks.sent(start + Duration::from_secs(6), 250);
        // The first BREAK's late refusal is not taken for the second's answer.
        assert_eq!(breaks.replied(false), None);
        assert_eq!(breaks.replied(true), Some(BreakOutcome::Sent));
        // A reply nobody waits for is ignored.
        assert_eq!(breaks.replied(true), None);
    }

    #[test]
    fn breaks_give_up_in_the_order_they_were_sent() {
        let start = Instant::now();
        let mut breaks = BreakReplies::new(Duration::from_millis(100));
        breaks.sent(start, 1_000);
        // Shorter, but sent later: it may not give up before the first.
        breaks.sent(start + Duration::from_millis(10), 0);
        assert_eq!(breaks.unsettled(), 2);
        assert!(breaks.expire(start + Duration::from_millis(200)).is_empty());
        assert_eq!(breaks.expire(start + Duration::from_millis(1_100)).len(), 2);
        assert_eq!(breaks.replied(true), None);
        assert_eq!(breaks.replied(false), None);
    }
}
