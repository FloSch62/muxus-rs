//! The connection diagnosis an ended SSH or Telnet tab prints when D is pressed (port of
//! `client/src/terminal/connection-diagnostics.ts` and `diagnoseFromTerminalInput` in
//! `TerminalViewImpl.tsx`). The checks run in the backend (`muxus_session::diagnostics`);
//! this is how their report reads in the terminal.

use muxus_core::api::{ConnectionCheckStatus, ConnectionDiagnosticsResponse};
use muxus_core::protocol::TerminalProfile;
use muxus_session::Backend;
use muxus_session::recovery::{reconnect_prompt, terminal_notice};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const MUTED: &str = "\x1b[90m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";

fn mark(status: ConnectionCheckStatus) -> String {
    let (color, glyph) = match status {
        ConnectionCheckStatus::Ok => (GREEN, '✔'),
        ConnectionCheckStatus::Fail => (RED, '✘'),
        ConnectionCheckStatus::Warn => (YELLOW, '!'),
        ConnectionCheckStatus::Info => (MUTED, '•'),
        ConnectionCheckStatus::Skipped => (MUTED, '–'),
    };
    format!("{color}{glyph}{RESET}")
}

/// Shown while the checks run; the report replaces it.
pub const DIAGNOSIS_PROGRESS: &str = "\x1b[90mChecking the connection …\x1b[0m";

/// One marked row per check, consecutive rows of one kind under a single
/// label, the conclusion last. Ends with a line break.
pub fn format_connection_diagnostics(report: &ConnectionDiagnosticsResponse) -> String {
    // `padEnd` counts UTF-16 code units; labels are ASCII.
    let width = report
        .checks
        .iter()
        .map(|check| check.label.encode_utf16().count())
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!(
        "{BOLD}➤ Connection diagnostics for {MAGENTA}{}{RESET}",
        terminal_notice(&report.checked)
    )];
    let mut previous: Option<&str> = None;
    for check in &report.checks {
        let label = if previous == Some(check.label.as_str()) {
            ""
        } else {
            check.label.as_str()
        };
        previous = Some(&check.label);
        let separator = if label.is_empty() { ' ' } else { ':' };
        let pad = width.saturating_sub(label.encode_utf16().count());
        lines.push(format!(
            "  {} {label}{} {separator} {}",
            mark(check.status),
            " ".repeat(pad),
            terminal_notice(&check.detail)
        ));
    }
    lines.push(format!(
        "  {BOLD}→{RESET} {}",
        terminal_notice(&report.conclusion)
    ));
    lines.iter().map(|line| format!("{line}\r\n")).collect()
}

/// The line written instead of a report when the checks could not run.
pub fn diagnosis_failed(message: &str) -> String {
    format!(
        "\x1b[31m[diagnostics failed: {}]\x1b[0m\r\n",
        terminal_notice(message)
    )
}

/// What replaces the progress line once the checks settle: the report (or
/// the failure), then the ended session's prompt again (`settle`).
pub fn settled_diagnosis(text: &str) -> String {
    format!(
        "\r\x1b[2K{text}\x1b[1;36m{}\x1b[0m\r\n",
        reconnect_prompt(true)
    )
}

/// What the terminal writes the moment D is pressed.
pub fn diagnosis_started() -> String {
    format!("\r\n{DIAGNOSIS_PROGRESS}")
}

/// Run the checks for an ended tab's profile and return the text that
/// replaces the progress line (report or failure, then the prompt).
pub async fn diagnose_connection(backend: &Backend, profile: TerminalProfile) -> String {
    let text = match backend.diagnose_connection(profile).await {
        Ok(report) => format_connection_diagnostics(&report),
        Err(error) => diagnosis_failed(&error.message),
    };
    settled_diagnosis(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::ConnectionCheck;

    fn plain(text: &str) -> String {
        let pattern = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
        pattern.replace_all(text, "").into_owned()
    }

    fn plain_has(text: &str, needle: &str) -> bool {
        plain(text).contains(needle)
    }

    fn check(label: &str, status: ConnectionCheckStatus, detail: &str) -> ConnectionCheck {
        ConnectionCheck {
            label: label.into(),
            status,
            detail: detail.into(),
        }
    }

    #[test]
    fn formats_rows_under_shared_labels() {
        let report = ConnectionDiagnosticsResponse {
            checked: "web-01:22".into(),
            checks: vec![
                check("DNS", ConnectionCheckStatus::Ok, "web-01 → 10.0.0.5"),
                check("TCP", ConnectionCheckStatus::Fail, "10.0.0.5: refused"),
                check("TCP", ConnectionCheckStatus::Skipped, "10.0.0.6: not tried"),
                check("Agent", ConnectionCheckStatus::Info, "2 keys"),
            ],
            conclusion: "Nothing listens on port 22.".into(),
        };
        let text = format_connection_diagnostics(&report);
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "\x1b[1m➤ Connection diagnostics for \x1b[35mweb-01:22\x1b[0m"
        );
        assert_eq!(lines[1], "  \x1b[32m✔\x1b[0m DNS   : web-01 → 10.0.0.5");
        assert_eq!(lines[2], "  \x1b[31m✘\x1b[0m TCP   : 10.0.0.5: refused");
        assert_eq!(lines[3], "  \x1b[90m–\x1b[0m         10.0.0.6: not tried");
        assert_eq!(lines[4], "  \x1b[90m•\x1b[0m Agent : 2 keys");
        assert_eq!(lines[5], "  \x1b[1m→\x1b[0m Nothing listens on port 22.");
        assert_eq!(lines[6], "");
    }

    #[test]
    fn strips_control_sequences_from_details() {
        let report = ConnectionDiagnosticsResponse {
            checked: "a\x1b[31m:22".into(),
            checks: vec![],
            conclusion: "x\x1b[2Jy".into(),
        };
        let text = format_connection_diagnostics(&report);
        assert!(!text.contains("\x1b[31m:22"));
        assert!(!text.contains("\x1b[2J"));
    }

    #[test]
    fn prints_one_marked_row_per_check_and_the_conclusion_last() {
        let text = format_connection_diagnostics(&ConnectionDiagnosticsResponse {
            checked: "web.example.com:22".into(),
            checks: vec![
                check(
                    "DNS",
                    ConnectionCheckStatus::Ok,
                    "web.example.com → 192.0.2.10 (3 ms)",
                ),
                check(
                    "Ping",
                    ConnectionCheckStatus::Warn,
                    "192.0.2.10 did not reply",
                ),
                check(
                    "TCP",
                    ConnectionCheckStatus::Info,
                    "[2001:db8::10]:22: no IPv6 route",
                ),
                check(
                    "TCP",
                    ConnectionCheckStatus::Fail,
                    "192.0.2.10:22 refused the connection",
                ),
            ],
            conclusion: "Nothing listens on port 22.".into(),
        });
        assert!(text.ends_with("\r\n"));
        let plain = plain(&text);
        let lines: Vec<&str> = plain.split("\r\n").collect();
        assert_eq!(
            lines,
            [
                "➤ Connection diagnostics for web.example.com:22",
                "  ✔ DNS  : web.example.com → 192.0.2.10 (3 ms)",
                "  ! Ping : 192.0.2.10 did not reply",
                "  • TCP  : [2001:db8::10]:22: no IPv6 route",
                "  ✘        192.0.2.10:22 refused the connection",
                "  → Nothing listens on port 22.",
                "",
            ]
        );
    }

    #[test]
    fn keeps_control_sequences_from_the_server_out_of_the_terminal() {
        let text = format_connection_diagnostics(&ConnectionDiagnosticsResponse {
            checked: "h:22".into(),
            checks: vec![check(
                "SSH",
                ConnectionCheckStatus::Fail,
                "answered \"\x1b]0;owned\x07x\"",
            )],
            conclusion: "line one\nline two".into(),
        });
        // Only the formatter's own colours remain; no byte from the report can act.
        let visible = plain(&text).replace("\r\n", "");
        let controls: Vec<char> = visible
            .chars()
            .filter(|c| (*c as u32) < 0x20 || *c as u32 == 0x7f)
            .collect();
        assert!(controls.is_empty(), "{controls:?}");
        assert!(plain(&text).contains("  → line one line two"));
    }

    #[test]
    fn settles_with_the_prompt_again() {
        assert_eq!(
            settled_diagnosis(&diagnosis_failed("no route\r\nto host")),
            "\r\x1b[2K\x1b[31m[diagnostics failed: no route to host]\x1b[0m\r\n\x1b[1;36mPress D to diagnose the connection, any other key to reconnect\x1b[0m\r\n"
        );
        assert_eq!(
            diagnosis_started(),
            "\r\n\x1b[90mChecking the connection …\x1b[0m"
        );
    }

    #[test]
    fn diagnoses_a_telnet_port_nothing_listens_on() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        // A port that was just free: bind, read the number, close.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut options = muxus_session::BackendOptions::sandbox(
            scratch.path().join("data"),
            scratch.path().join("home"),
        );
        options.runtime = muxus_session::RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime.block_on(Backend::start(options)).unwrap();
        let profile: TerminalProfile = serde_json::from_value(serde_json::json!({
            "kind": "telnet", "host": "127.0.0.1", "port": port
        }))
        .unwrap();
        let text = runtime.block_on(diagnose_connection(&backend, profile));
        let plain = plain(&text);
        assert!(
            plain.contains(&format!("Connection diagnostics for 127.0.0.1:{port}")),
            "{plain}"
        );
        assert!(plain.contains("✘"), "{plain}");
        assert!(
            plain.ends_with("Press D to diagnose the connection, any other key to reconnect\r\n")
        );
        let local: TerminalProfile =
            serde_json::from_value(serde_json::json!({ "kind": "local" })).unwrap();
        let refused = runtime.block_on(diagnose_connection(&backend, local));
        assert!(
            plain_has(
                &refused,
                "[diagnostics failed: invalid diagnostics request]"
            ),
            "{refused}"
        );
        runtime.block_on(backend.shutdown());
    }
}
