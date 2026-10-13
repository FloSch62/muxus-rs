//! How a terminal tab recovers from a lost connection (`client/src/connection-recovery.ts`).
//!
//! The backend side of recovery is the session itself: a session outlives its window for the
//! reattach grace (see [`crate::terminal`]), and a forced reconnect passes `freshTransport` so
//! the replacement does not join the transport that just failed. These are the decisions the
//! tab makes around that: when to reattach, when to redial on its own, and what to tell the
//! user.

use muxus_core::protocol::{ExitReason, ProfileKind};

/// The window a live session may be interrupted before the tab shows it as lost.
pub const CONNECTION_INTERRUPTION_GRACE_MS: u64 = 5_000;

/// Fast retries to reattach a backend terminal before opening an entirely new session.
pub const RENDERER_REATTACH_DELAYS_MS: [u64; 4] = [0, 250, 1_000, 2_000];

/// Delays before each automatic reconnect attempt after a lost connection.
pub const AUTO_RECONNECT_DELAYS_MS: [u64; 3] = [2_000, 5_000, 15_000];

/// Uptime after which a drop is a fresh incident with a fresh attempt budget.
pub const AUTO_RECONNECT_STABLE_MS: u64 = 30_000;

/// A multiplexer to reattach after an SSH reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReattachMode {
    Tmux,
    Screen,
}

/// What the tab knew when its session ended: the `exit` message, if one came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub message: Option<String>,
    pub reason: ExitReason,
}

/// How the session's channel closed (the socket close of the TypeScript client).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelClose {
    pub code: u16,
    pub reason: Option<String>,
    pub was_clean: bool,
}

/// Delay before retrying a stable terminal id, or `None` when exhausted.
pub fn renderer_reattach_delay_ms(attempts: usize) -> Option<u64> {
    RENDERER_REATTACH_DELAYS_MS.get(attempts).copied()
}

/// The inputs to [`auto_reconnect_delay_ms`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoReconnectInput {
    /// The auto-reconnect preference.
    pub enabled: bool,
    pub profile_kind: ProfileKind,
    pub reason: ExitReason,
    /// Automatic attempts already made since the last stable connection.
    pub attempts: usize,
    /// An interactive auth prompt was shown; redialling would only re-prompt.
    pub saw_auth_prompt: bool,
}

/// Delay before the next automatic redial, or `None` to wait for the user.
pub fn auto_reconnect_delay_ms(input: AutoReconnectInput) -> Option<u64> {
    if !input.enabled || input.profile_kind == ProfileKind::Local {
        return None;
    }
    if input.reason == ExitReason::Completed {
        return None;
    }
    // A failed dial may continue a chain that a real drop started, but never start one (that
    // would loop on hosts that refuse to connect at all), and never talk over an auth prompt
    // the user walked away from.
    if input.reason == ExitReason::Failed && (input.attempts == 0 || input.saw_auth_prompt) {
        return None;
    }
    AUTO_RECONNECT_DELAYS_MS.get(input.attempts).copied()
}

/// Whether an ended session offers the diagnose key: a network session that failed or dropped,
/// not a shell that exited on its own.
pub fn can_diagnose_connection(profile_kind: ProfileKind, reason: ExitReason) -> bool {
    reason != ExitReason::Completed
        && matches!(profile_kind, ProfileKind::Ssh | ProfileKind::Telnet)
}

/// The key that runs connection diagnostics instead of reconnecting.
pub fn is_diagnose_key(data: &str) -> bool {
    data == "d" || data == "D"
}

/// The line that tells an ended session's user what a key press does.
pub fn reconnect_prompt(diagnose: bool) -> &'static str {
    if diagnose {
        "Press D to diagnose the connection, any other key to reconnect"
    } else {
        "Press any key to reconnect"
    }
}

/// The countdown line shown while an automatic reconnect is pending.
pub fn auto_reconnect_notice(delay_ms: u64, attempt: usize, diagnose: bool) -> String {
    let keys = if diagnose {
        "D diagnoses, any other key reconnects now"
    } else {
        "any key reconnects now"
    };
    format!(
        "Reconnecting in {}s (attempt {attempt} of {}) — {keys}",
        (delay_ms as f64 / 1000.0).round(),
        AUTO_RECONNECT_DELAYS_MS.len()
    )
}

/// Turn the exit message and close information into one user-facing explanation.
pub fn connection_failure_reason(exit: Option<&TerminalExit>, close: &ChannelClose) -> String {
    if let Some(message) = exit
        .and_then(|exit| exit.message.as_deref())
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        return message.to_string();
    }
    match exit.map(|exit| (exit.reason, exit.code)) {
        Some((ExitReason::Completed, None | Some(0))) => {
            return "The shell exited normally.".into();
        }
        Some((ExitReason::Completed, Some(code))) => {
            return format!("The shell exited with status {code}.");
        }
        Some((ExitReason::Failed, _)) => return "The connection attempt failed.".into(),
        Some((ExitReason::Disconnected, _)) => {
            return "The remote connection closed unexpectedly.".into();
        }
        None => {}
    }
    if let Some(reason) = close
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        return reason.to_string();
    }
    if close.code == 1006 {
        return "The connection to the Muxus backend was interrupted.".into();
    }
    if close.was_clean {
        return "The connection was closed.".into();
    }
    format!(
        "The connection closed unexpectedly (WebSocket code {}).",
        close.code
    )
}

/// Delay the final lost state only for a session that had reached ready.
pub fn should_delay_connection_lost(exit: Option<&TerminalExit>, was_ready: bool) -> bool {
    was_ready && exit.is_none_or(|exit| exit.reason != ExitReason::Completed)
}

/// SSH is only visibly responsive once its terminal channel produces output.
pub fn should_wait_for_terminal_output(profile_kind: ProfileKind, received_output: bool) -> bool {
    profile_kind == ProfileKind::Ssh && !received_output
}

/// Shell input sent once a replacement SSH transport is ready.
pub fn reattach_command(mode: ReattachMode) -> &'static str {
    match mode {
        ReattachMode::Tmux => {
            "if command -v tmux >/dev/null 2>&1; then tmux attach-session 2>/dev/null || tmux new-session; else printf '\\r\\nMuxus: tmux is not installed.\\r\\n'; fi\r"
        }
        ReattachMode::Screen => {
            "if command -v screen >/dev/null 2>&1; then screen -xRR; else printf '\\r\\nMuxus: screen is not installed.\\r\\n'; fi\r"
        }
    }
}

/// Keep server-provided failure text from becoming terminal control input: CSI sequences go,
/// line breaks become single spaces, other control characters are dropped.
pub fn terminal_notice(message: &str) -> String {
    let without_csi = strip_csi(message);
    let mut clean = String::new();
    for character in without_csi.chars() {
        let code = u32::from(character);
        if code == 10 || code == 13 {
            if !clean.is_empty() && !clean.ends_with(' ') {
                clean.push(' ');
            }
        } else if code == 9 || (code >= 32 && code != 127) {
            clean.push(character);
        }
    }
    clean
}

/// Remove `ESC [ params intermediates final` sequences (`/\x1b\[[0-?]*[ -/]*[@-~]/g`).
fn strip_csi(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\u{1b}' && chars.get(index + 1) == Some(&'[') {
            let mut end = index + 2;
            while end < chars.len() && ('0'..='?').contains(&chars[end]) {
                end += 1;
            }
            while end < chars.len() && (' '..='/').contains(&chars[end]) {
                end += 1;
            }
            if end < chars.len() && ('@'..='~').contains(&chars[end]) {
                index = end + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(reason: ExitReason, code: Option<i32>, message: Option<&str>) -> TerminalExit {
        TerminalExit {
            code,
            message: message.map(str::to_string),
            reason,
        }
    }

    fn close(code: u16) -> ChannelClose {
        ChannelClose {
            code,
            ..ChannelClose::default()
        }
    }

    #[test]
    fn prefers_the_specific_server_reason() {
        assert_eq!(
            connection_failure_reason(
                Some(&exit(
                    ExitReason::Failed,
                    None,
                    Some("jump host could not reach router.internal:22")
                )),
                &close(1011)
            ),
            "jump host could not reach router.internal:22"
        );
    }

    #[test]
    fn explains_normal_nonzero_and_abnormal_closes() {
        assert_eq!(
            connection_failure_reason(
                Some(&exit(ExitReason::Completed, Some(0), None)),
                &close(1000)
            ),
            "The shell exited normally."
        );
        assert_eq!(
            connection_failure_reason(
                Some(&exit(ExitReason::Completed, Some(127), None)),
                &close(1000)
            ),
            "The shell exited with status 127."
        );
        assert_eq!(
            connection_failure_reason(None, &close(1006)),
            "The connection to the Muxus backend was interrupted."
        );
    }

    #[test]
    fn strips_terminal_control_input_from_server_error_text() {
        assert_eq!(terminal_notice("bad\u{1b}[31m\r\nnews"), "bad news");
    }

    #[test]
    fn uses_a_grace_period_only_after_a_live_session_is_interrupted() {
        assert_eq!(CONNECTION_INTERRUPTION_GRACE_MS, 5_000);
        assert!(should_delay_connection_lost(
            Some(&exit(
                ExitReason::Disconnected,
                None,
                Some("transport closed")
            )),
            true
        ));
        assert!(should_delay_connection_lost(None, true));
        assert!(!should_delay_connection_lost(
            Some(&exit(ExitReason::Failed, None, None)),
            false
        ));
        assert!(!should_delay_connection_lost(
            Some(&exit(ExitReason::Completed, None, None)),
            true
        ));
    }

    #[test]
    fn keeps_ssh_waiting_until_terminal_output_confirms_it() {
        assert!(should_wait_for_terminal_output(ProfileKind::Ssh, false));
        assert!(!should_wait_for_terminal_output(ProfileKind::Ssh, true));
        assert!(!should_wait_for_terminal_output(ProfileKind::Local, false));
        assert!(!should_wait_for_terminal_output(ProfileKind::Telnet, false));
        assert!(!should_wait_for_terminal_output(ProfileKind::Serial, false));
    }

    fn drop_input() -> AutoReconnectInput {
        AutoReconnectInput {
            enabled: true,
            profile_kind: ProfileKind::Ssh,
            reason: ExitReason::Disconnected,
            attempts: 0,
            saw_auth_prompt: false,
        }
    }

    #[test]
    fn redials_a_dropped_session_with_growing_delays_until_the_budget_runs_out() {
        let at = |attempts| {
            auto_reconnect_delay_ms(AutoReconnectInput {
                attempts,
                ..drop_input()
            })
        };
        assert_eq!(at(0), Some(2_000));
        assert_eq!(at(1), Some(5_000));
        assert_eq!(at(2), Some(15_000));
        assert_eq!(at(AUTO_RECONNECT_DELAYS_MS.len()), None);
    }

    #[test]
    fn never_dials_on_its_own_when_disabled_local_or_exited_normally() {
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                enabled: false,
                ..drop_input()
            }),
            None
        );
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                profile_kind: ProfileKind::Local,
                ..drop_input()
            }),
            None
        );
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                reason: ExitReason::Completed,
                ..drop_input()
            }),
            None
        );
    }

    #[test]
    fn continues_a_chain_through_failed_dials_but_never_starts_one() {
        let failed = AutoReconnectInput {
            reason: ExitReason::Failed,
            ..drop_input()
        };
        assert_eq!(auto_reconnect_delay_ms(failed), None);
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                attempts: 1,
                ..failed
            }),
            Some(5_000)
        );
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                attempts: 1,
                saw_auth_prompt: true,
                ..failed
            }),
            None
        );
        assert_eq!(
            auto_reconnect_delay_ms(AutoReconnectInput {
                saw_auth_prompt: true,
                ..drop_input()
            }),
            Some(2_000)
        );
    }

    #[test]
    fn retries_a_stable_backend_terminal_before_a_replacement() {
        let delays: Vec<u64> = (0..RENDERER_REATTACH_DELAYS_MS.len())
            .filter_map(renderer_reattach_delay_ms)
            .collect();
        assert_eq!(delays, RENDERER_REATTACH_DELAYS_MS);
        assert_eq!(
            renderer_reattach_delay_ms(RENDERER_REATTACH_DELAYS_MS.len()),
            None
        );
    }

    #[test]
    fn reattaches_tmux_or_screen() {
        let tmux = reattach_command(ReattachMode::Tmux);
        assert!(tmux.contains("tmux attach-session"));
        assert!(tmux.contains("tmux new-session"));
        assert!(tmux.ends_with('\r'));
        assert!(reattach_command(ReattachMode::Screen).contains("screen -xRR"));
    }

    #[test]
    fn offers_the_diagnose_key_for_failed_network_sessions() {
        assert!(can_diagnose_connection(
            ProfileKind::Ssh,
            ExitReason::Failed
        ));
        assert!(can_diagnose_connection(
            ProfileKind::Telnet,
            ExitReason::Disconnected
        ));
        assert!(!can_diagnose_connection(
            ProfileKind::Ssh,
            ExitReason::Completed
        ));
        assert!(!can_diagnose_connection(
            ProfileKind::Local,
            ExitReason::Failed
        ));
        assert!(!can_diagnose_connection(
            ProfileKind::Serial,
            ExitReason::Failed
        ));
        assert!(is_diagnose_key("d"));
        assert!(is_diagnose_key("D"));
        assert!(!is_diagnose_key("\u{4}"));
        assert!(!is_diagnose_key("dd"));
        assert_eq!(reconnect_prompt(false), "Press any key to reconnect");
        assert_eq!(
            reconnect_prompt(true),
            "Press D to diagnose the connection, any other key to reconnect"
        );
        assert_eq!(
            auto_reconnect_notice(2_000, 1, false),
            "Reconnecting in 2s (attempt 1 of 3) — any key reconnects now"
        );
        assert_eq!(
            auto_reconnect_notice(5_000, 2, true),
            "Reconnecting in 5s (attempt 2 of 3) — D diagnoses, any other key reconnects now"
        );
    }
}
