//! Saved text typed into a session (port of `client/src/terminal/typed-input.ts`).

use muxus_core::api::TerminalEnterKey;
use muxus_core::js::utf16_len;
use muxus_core::paste_pacing::{
    MAX_PACED_PASTE_LENGTH, PastePacing, is_paced_paste, terminal_paste_data,
};
use muxus_core::protocol::TerminalClientMessage;

/// How saved text goes out: written at once, or through the backend pacer.
// A short-lived value, sent right away; boxing the message would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedInput {
    Direct(String),
    /// A `paste` message, never bracketed.
    Paced(TerminalClientMessage),
}

/// Saved text is typed, never bracketed, so Enter still executes; its line breaks go out as the
/// host's Enter key. Paced hosts route it through the backend pacer, which also queues it behind
/// a running paste; `behind_paste` queues it there too when only that paste was paced.
pub fn typed_input(
    text: &str,
    pacing: &PastePacing,
    behind_paste: bool,
    enter_key: TerminalEnterKey,
) -> TypedInput {
    if text.is_empty()
        || utf16_len(text) > MAX_PACED_PASTE_LENGTH
        || !(behind_paste || is_paced_paste(pacing))
    {
        return TypedInput::Direct(terminal_paste_data(text, false, enter_key));
    }
    TypedInput::Paced(TerminalClientMessage::Paste {
        text: text.to_string(),
        bracketed: false,
        line_delay_ms: pacing.line_delay_ms,
        char_delay_ms: pacing.char_delay_ms,
        prompt: pacing.prompt.clone(),
        enter_key: (enter_key != TerminalEnterKey::Cr).then_some(enter_key),
    })
}

/// Port of `tests/unit/client/typed-input.test.ts`.
#[cfg(test)]
mod tests {
    use muxus_core::validate::Validate;
    use serde_json::json;

    use super::*;

    const CR: TerminalEnterKey = TerminalEnterKey::Cr;

    fn paced() -> PastePacing {
        PastePacing::delays(250, 0)
    }

    fn message(value: serde_json::Value) -> TerminalClientMessage {
        let mut message: TerminalClientMessage = serde_json::from_value(value).unwrap();
        message.validate().unwrap();
        message
    }

    #[test]
    fn writes_text_at_once_when_the_host_does_not_pace_pastes() {
        assert_eq!(
            typed_input("show version\r", &PastePacing::delays(0, 0), false, CR),
            TypedInput::Direct("show version\r".into())
        );
    }

    #[test]
    fn types_through_the_backend_pacer_never_bracketed() {
        assert_eq!(
            typed_input("conf t\rinterface Gi0/1\rend\r", &paced(), false, CR),
            TypedInput::Paced(message(json!({
                "op": "paste", "text": "conf t\rinterface Gi0/1\rend\r", "bracketed": false,
                "lineDelayMs": 250, "charDelayMs": 0,
            })))
        );
    }

    #[test]
    fn paces_a_single_line_when_the_host_sets_a_character_delay() {
        assert!(matches!(
            typed_input("show ip route\r", &PastePacing::delays(0, 20), false, CR),
            TypedInput::Paced(_)
        ));
    }

    #[test]
    fn queues_behind_a_running_paste_even_when_only_that_paste_was_paced() {
        assert_eq!(
            typed_input("uptime\r", &PastePacing::delays(0, 0), true, CR),
            TypedInput::Paced(message(json!({
                "op": "paste", "text": "uptime\r", "bracketed": false,
                "lineDelayMs": 0, "charDelayMs": 0,
            })))
        );
    }

    #[test]
    fn types_line_breaks_as_the_hosts_enter_key_written_at_once_or_paced() {
        let unpaced = PastePacing::delays(0, 0);
        assert_eq!(
            typed_input("conf t\rend\r", &unpaced, false, TerminalEnterKey::Crlf),
            TypedInput::Direct("conf t\r\nend\r\n".into())
        );
        assert_eq!(
            typed_input("conf t\rend\r", &unpaced, false, TerminalEnterKey::Lf),
            TypedInput::Direct("conf t\nend\n".into())
        );
        assert_eq!(
            typed_input("conf t\rend\r", &paced(), false, TerminalEnterKey::Lf),
            TypedInput::Paced(message(json!({
                "op": "paste", "text": "conf t\rend\r", "bracketed": false,
                "lineDelayMs": 250, "charDelayMs": 0, "enterKey": "lf",
            })))
        );
    }

    #[test]
    fn keeps_escape_characters_intact() {
        match typed_input("\x1bq", &paced(), false, CR) {
            TypedInput::Paced(TerminalClientMessage::Paste { text, .. }) => {
                assert_eq!(text, "\x1bq")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn falls_back_to_a_direct_write_for_empty_or_oversized_text() {
        let long = "x".repeat(MAX_PACED_PASTE_LENGTH + 1);
        assert_eq!(
            typed_input("", &paced(), false, CR),
            TypedInput::Direct(String::new())
        );
        assert_eq!(
            typed_input(&long, &paced(), false, CR),
            TypedInput::Direct(long.clone())
        );
    }
}
