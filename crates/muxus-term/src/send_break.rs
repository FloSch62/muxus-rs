//! Which sessions can send BREAK (port of the pure half of `client/src/terminal/send-break.ts`;
//! the command and the per-tab dispatch live in the app).

/// Serial, Telnet and SSH sessions have a line to send BREAK on; shells and desktops do not.
pub fn supports_break(profile_kind: Option<&str>) -> bool {
    matches!(profile_kind, Some("serial" | "telnet" | "ssh"))
}

/// Whether the session is attached. An SSH console that has printed nothing yet still counts: a
/// silent console is exactly when BREAK is wanted.
pub fn can_send_break(profile_kind: Option<&str>, status: &str, has_connection: bool) -> bool {
    supports_break(profile_kind)
        && (status == "connected" || (status == "connecting" && has_connection))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn belongs_to_serial_telnet_and_ssh_tabs_only() {
        for kind in ["ssh", "telnet", "serial"] {
            assert!(supports_break(Some(kind)), "{kind}");
        }
        for kind in ["local", "rdp", "vnc"] {
            assert!(!supports_break(Some(kind)), "{kind}");
            assert!(!can_send_break(Some(kind), "connected", true), "{kind}");
        }
        assert!(!supports_break(None));
    }

    #[test]
    fn waits_for_the_session_but_not_for_a_silent_ssh_console_to_print() {
        assert!(!can_send_break(Some("ssh"), "connecting", false));
        assert!(can_send_break(Some("ssh"), "connecting", true));
        assert!(!can_send_break(Some("ssh"), "closed", true));
    }
}
