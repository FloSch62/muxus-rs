//! GPUI keystrokes in the DOM terms the terminal key encoders take.

use gpui_kit::Keystroke;
use muxus_term::keyboard::KeyboardEvent;

/// Characters a US keyboard types with Shift; GPUI reports them without the Shift modifier.
const SHIFTED_SYMBOLS: &str = "~!@#$%^&*()_+{}|:\"<>?";

pub(crate) fn keyboard_event(keystroke: &Keystroke, repeat: bool) -> KeyboardEvent {
    let modifiers = &keystroke.modifiers;
    let key = keystroke.key.as_str();
    let mut shift = modifiers.shift;
    if key.chars().count() == 1 && key.chars().all(|c| SHIFTED_SYMBOLS.contains(c)) {
        shift = true;
    }
    let mut event = KeyboardEvent::from_key(
        &key.to_lowercase_if_named(),
        keystroke.key_char.as_deref(),
        shift,
        modifiers.alt,
        modifiers.control,
        modifiers.platform,
    );
    event.repeat = repeat;
    event
}

trait NamedKey {
    fn to_lowercase_if_named(&self) -> String;
}

impl NamedKey for str {
    /// Named keys ("Enter", "PageUp") in lower case; single characters keep their case.
    fn to_lowercase_if_named(&self) -> String {
        if self.chars().count() > 1 {
            self.to_lowercase()
        } else {
            self.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Modifiers;

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(String::from),
        }
    }

    #[test]
    fn maps_gpui_keystrokes() {
        let ev = keyboard_event(&keystroke("enter", None, Modifiers::default()), false);
        assert_eq!((ev.key.as_str(), ev.key_code), ("Enter", 13));
        let ev = keyboard_event(&keystroke("!", Some("!"), Modifiers::default()), false);
        assert!(ev.shift);
        assert_eq!(ev.code, "Digit1");
        let ev = keyboard_event(&keystroke("c", None, Modifiers::control()), false);
        assert!(ev.ctrl);
        assert_eq!(ev.key_code, 67);
    }
}
