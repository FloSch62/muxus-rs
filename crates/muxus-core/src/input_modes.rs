//! A host's terminal input modes (port of `client/src/terminal/input-modes.ts`):
//! what its Backspace and Enter keys send, and how its terminal shows lines
//! and input. Shared here because the host metadata, backups and the terminal
//! all use them.

use crate::api::{
    OpenSshMetadataPatch, OpenSshProfileMetadata, TerminalBackspaceKey, TerminalEnterKey,
};
use crate::js::Patch;
use crate::paste_pacing::with_enter_key;

/// What a host's Backspace and Enter keys send, and how its terminal shows lines and input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TerminalInputModes {
    pub backspace_key: TerminalBackspaceKey,
    pub enter_key: TerminalEnterKey,
    /// xterm.js `convertEol`: a received LF also returns to the start of the line.
    pub implicit_cr: bool,
    pub local_echo: bool,
}

/// `BACKSPACE_KEY_OPTIONS`: the editor's choices.
pub const BACKSPACE_KEY_OPTIONS: [(TerminalBackspaceKey, &str); 2] = [
    (TerminalBackspaceKey::Del, "DEL, 0x7F (default)"),
    (TerminalBackspaceKey::CtrlH, "^H, 0x08"),
];

/// `ENTER_KEY_OPTIONS`: the editor's choices.
pub const ENTER_KEY_OPTIONS: [(TerminalEnterKey, &str); 3] = [
    (TerminalEnterKey::Cr, "CR (default)"),
    (TerminalEnterKey::Crlf, "CR LF"),
    (TerminalEnterKey::Lf, "LF"),
];

/// A host's modes; whatever it leaves unset keeps what xterm.js does.
pub fn host_terminal_input_modes(host: Option<&OpenSshProfileMetadata>) -> TerminalInputModes {
    let Some(host) = host else {
        return TerminalInputModes::default();
    };
    TerminalInputModes {
        backspace_key: host.backspace_key.unwrap_or_default(),
        enter_key: host.enter_key.unwrap_or_default(),
        implicit_cr: host.implicit_cr.unwrap_or(false),
        local_echo: host.local_echo.unwrap_or(false),
    }
}

/// Whether a host changes anything about its keys or echo.
pub fn custom_terminal_input_modes(modes: &TerminalInputModes) -> bool {
    *modes != TerminalInputModes::default()
}

/// The modes written into a metadata patch; the defaults are stored as unset.
pub fn apply_terminal_input_modes_patch(
    modes: &TerminalInputModes,
    patch: &mut OpenSshMetadataPatch,
) {
    patch.backspace_key = match modes.backspace_key {
        TerminalBackspaceKey::Del => Patch::Clear,
        key => Patch::Set(key),
    };
    patch.enter_key = match modes.enter_key {
        TerminalEnterKey::Cr => Patch::Clear,
        key => Patch::Set(key),
    };
    patch.implicit_cr = Patch::Set(modes.implicit_cr);
    patch.local_echo = Patch::Set(modes.local_echo);
}

/// One keystroke as xterm.js encodes it, sent with the host's keys. Only plain
/// Backspace and Enter change; with Ctrl or Alt they send what they always did.
pub fn keyboard_input_data(data: &str, modes: &TerminalInputModes) -> String {
    if data == "\r" {
        return with_enter_key(data, modes.enter_key);
    }
    if data == "\x7f" && modes.backspace_key == TerminalBackspaceKey::CtrlH {
        return "\x08".to_owned();
    }
    data.to_owned()
}

/// C0 and C1 controls but the CR LF shown for Enter; xterm.js would answer C1 queries.
fn unshown_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{9}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// What local echo shows for input, as an echoing device would: printable text
/// as typed, Enter as a new line, Backspace as erasing the character before the
/// cursor. Escape sequences (arrows, function keys, Alt combinations, terminal
/// replies) and other control characters, Tab included, are not shown.
pub fn local_echo_text(data: &str) -> String {
    if data == "\x7f" || data == "\x08" {
        return "\x08 \x08".to_owned();
    }
    if data.starts_with('\x1b') {
        return String::new();
    }
    // `/\r\n?|\n/g` → CR LF.
    let mut lines = String::with_capacity(data.len());
    let mut chars = data.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push_str("\r\n");
            }
            '\n' => lines.push_str("\r\n"),
            c => lines.push(c),
        }
    }
    lines.chars().filter(|&c| !unshown_control(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_modes_keep_what_xterm_does() {
        assert_eq!(
            host_terminal_input_modes(None),
            TerminalInputModes::default()
        );
        let metadata = OpenSshProfileMetadata {
            enter_key: Some(TerminalEnterKey::Lf),
            local_echo: Some(true),
            ..Default::default()
        };
        let modes = host_terminal_input_modes(Some(&metadata));
        assert_eq!(modes.enter_key, TerminalEnterKey::Lf);
        assert_eq!(modes.backspace_key, TerminalBackspaceKey::Del);
        assert!(modes.local_echo && !modes.implicit_cr);
        assert!(custom_terminal_input_modes(&modes));
    }

    #[test]
    fn stores_the_defaults_as_unset() {
        let mut patch = OpenSshMetadataPatch::default();
        apply_terminal_input_modes_patch(&TerminalInputModes::default(), &mut patch);
        assert_eq!(
            serde_json::to_string(&patch).unwrap(),
            r#"{"backspaceKey":null,"enterKey":null,"implicitCr":false,"localEcho":false}"#
        );
    }

    #[test]
    fn sends_and_echoes_keys_as_the_host_expects() {
        let modes = TerminalInputModes {
            backspace_key: TerminalBackspaceKey::CtrlH,
            enter_key: TerminalEnterKey::Crlf,
            ..Default::default()
        };
        assert_eq!(keyboard_input_data("\r", &modes), "\r\n");
        assert_eq!(keyboard_input_data("\x7f", &modes), "\x08");
        assert_eq!(keyboard_input_data("\x1b\x7f", &modes), "\x1b\x7f");
        assert_eq!(local_echo_text("a\rb\nc\r\n"), "a\r\nb\r\nc\r\n");
        assert_eq!(local_echo_text("\x7f"), "\x08 \x08");
        assert_eq!(local_echo_text("\x1b[A"), "");
        assert_eq!(local_echo_text("a\tb\u{85}"), "ab");
    }
}
