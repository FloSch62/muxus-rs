//! OSC 52 clipboard handling (port of `client/src/terminal/osc52-clipboard.ts` and
//! `osc52-clipboard-provider.ts`, with the decoding of xterm's `ClipboardAddon`).
//!
//! Programs may write the system clipboard (when the preference allows it) but never read it: a
//! query is answered with an empty payload, on the session that asked, so clipboard contents never
//! reach a program running in the terminal.

use base64::Engine;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// What one OSC 52 payload asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc52Action {
    /// Put this text on the clipboard. Text that is not valid base64 clears it, as xterm does.
    Write(String),
    /// Send these bytes back to the program (the reply to a read query).
    Reply(String),
    /// Nothing to do: malformed, or a write the preference forbids.
    Ignore,
}

const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

fn decode_text(data: &str) -> String {
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    match LENIENT.decode(compact) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// Handle the payload of one `OSC 52 ; <selection> ; <data>` sequence (everything after `52;`).
pub fn handle_osc52(data: &str, allow_write: bool) -> Osc52Action {
    // Queries are consumed first and answered with an empty clipboard.
    if let Some(separator) = data.find(';')
        && &data[separator + 1..] == "?"
    {
        return Osc52Action::Reply(format!("\x1b]52;{};\x07", &data[..separator]));
    }
    let args: Vec<&str> = data.split(';').collect();
    if args.len() < 2 {
        return Osc52Action::Ignore;
    }
    if args[1] == "?" {
        // A query with trailing fields: the write-only provider still reveals nothing.
        return Osc52Action::Reply(format!("\x1b]52;{};\x07", args[0]));
    }
    if !allow_write {
        return Osc52Action::Ignore;
    }
    Osc52Action::Write(decode_text(args[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    #[test]
    fn writes_decoded_text_when_terminal_clipboard_writes_are_allowed() {
        let text = "copied through zellij ✓";
        assert_eq!(
            handle_osc52(&format!("c;{}", encode(text)), true),
            Osc52Action::Write(text.into())
        );
    }

    #[test]
    fn checks_the_current_preference_for_every_write() {
        assert_eq!(
            handle_osc52(&format!("c;{}", encode("blocked")), false),
            Osc52Action::Ignore
        );
        assert_eq!(
            handle_osc52(&format!("c;{}", encode("allowed")), true),
            Osc52Action::Write("allowed".into())
        );
    }

    #[test]
    fn never_exposes_clipboard_contents_to_read_queries() {
        assert_eq!(
            handle_osc52("c;?", true),
            Osc52Action::Reply("\x1b]52;c;\x07".into())
        );
        assert_eq!(
            handle_osc52("p;?", false),
            Osc52Action::Reply("\x1b]52;p;\x07".into())
        );
    }

    #[test]
    fn clears_the_clipboard_for_text_that_is_not_base64_and_ignores_malformed_payloads() {
        assert_eq!(
            handle_osc52("c;not base64!", true),
            Osc52Action::Write(String::new())
        );
        assert_eq!(handle_osc52("c", true), Osc52Action::Ignore);
        // Missing padding is accepted, as the browser decoder does.
        assert_eq!(handle_osc52("c;aGk", true), Osc52Action::Write("hi".into()));
    }
}
