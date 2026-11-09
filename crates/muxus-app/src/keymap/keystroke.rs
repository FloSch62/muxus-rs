//! GPUI keystrokes in the DOM `KeyboardEvent` shape the keymap speaks
//! ([`ChordEvent`]): what the window's shortcut listener and the shortcut
//! recorder in the settings feed to the chord matcher.

use muxus_ui::gpui_kit::Keystroke;

use super::chords::ChordEvent;

/// The DOM `code` for a GPUI key name, in US-layout terms. GPUI reports the
/// layout's key rather than the physical key, so on other layouts letters map
/// to the key they produce.
pub fn dom_code(key: &str) -> Option<String> {
    let code = match key {
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "tab" => "Tab",
        "enter" => "Enter",
        "escape" => "Escape",
        "space" => "Space",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "-" => "Minus",
        "=" | "+" => "Equal",
        "[" => "BracketLeft",
        "]" => "BracketRight",
        "\\" => "Backslash",
        ";" => "Semicolon",
        "'" => "Quote",
        "," => "Comma",
        "." => "Period",
        "/" => "Slash",
        "`" => "Backquote",
        _ => {
            let mut chars = key.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                if first.is_ascii_alphabetic() {
                    return Some(format!("Key{}", first.to_ascii_uppercase()));
                }
                if first.is_ascii_digit() {
                    return Some(format!("Digit{first}"));
                }
                return None;
            }
            if let Some(n) = key.strip_prefix('f')
                && n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))
            {
                return Some(format!("F{n}"));
            }
            return None;
        }
    };
    Some(code.to_string())
}

/// `KeyboardEvent.key` for a keystroke: the produced character, or the named key.
pub fn dom_key(keystroke: &Keystroke) -> String {
    if let Some(ch) = &keystroke.key_char
        && !ch.is_empty()
        && !keystroke.modifiers.control
    {
        return ch.clone();
    }
    match keystroke.key.as_str() {
        "left" => "ArrowLeft".into(),
        "right" => "ArrowRight".into(),
        "up" => "ArrowUp".into(),
        "down" => "ArrowDown".into(),
        "tab" => "Tab".into(),
        "enter" => "Enter".into(),
        "escape" => "Escape".into(),
        "space" => " ".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        other => other.into(),
    }
}

/// The unshifted key for a character a US keyboard types with Shift.
fn us_unshifted(key: &str) -> Option<&'static str> {
    Some(match key {
        "~" => "`",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        "_" => "-",
        "+" => "=",
        "{" => "[",
        "}" => "]",
        "|" => "\\",
        ":" => ";",
        "\"" => "'",
        "<" => ",",
        ">" => ".",
        "?" => "/",
        _ => return None,
    })
}

/// The chord event for a GPUI keystroke; `None` for keys the keymap cannot name.
///
/// GPUI drops Shift from symbol and digit keys and reports the shifted
/// character instead (">" for Shift+Period), so a shifted US symbol is read
/// back as its key with Shift held.
pub fn chord_event(keystroke: &Keystroke) -> Option<ChordEvent> {
    let unshifted = us_unshifted(&keystroke.key);
    let code = dom_code(unshifted.unwrap_or(&keystroke.key))?;
    Some(ChordEvent {
        key: dom_key(keystroke),
        code,
        ctrl_key: keystroke.modifiers.control,
        alt_key: keystroke.modifiers.alt,
        shift_key: keystroke.modifiers.shift || unshifted.is_some(),
        meta_key: keystroke.modifiers.platform,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::chords::event_signature;
    use muxus_ui::gpui_kit::Modifiers;

    #[test]
    fn maps_keys_to_dom_codes() {
        assert_eq!(dom_code("t").as_deref(), Some("KeyT"));
        assert_eq!(dom_code("5").as_deref(), Some("Digit5"));
        assert_eq!(dom_code("left").as_deref(), Some("ArrowLeft"));
        assert_eq!(dom_code("f11").as_deref(), Some("F11"));
        assert_eq!(dom_code("[").as_deref(), Some("BracketLeft"));
        assert_eq!(dom_code("f99"), None);
        assert_eq!(dom_code("é"), None);
    }

    #[test]
    fn reads_a_chord_from_a_keystroke() {
        let chord = chord_event(&Keystroke::parse("ctrl-shift-t").unwrap()).unwrap();
        assert_eq!(chord.code, "KeyT");
        assert!(chord.ctrl_key && chord.shift_key && !chord.alt_key && !chord.meta_key);
        let arrow = chord_event(&Keystroke::parse("alt-left").unwrap()).unwrap();
        assert_eq!(
            (arrow.key.as_str(), arrow.code.as_str()),
            ("ArrowLeft", "ArrowLeft")
        );
        assert!(chord_event(&Keystroke::parse("ctrl-é").unwrap()).is_none());
    }

    /// GPUI reports Shift+Period as ">" with Shift cleared.
    fn shifted_symbol(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    #[test]
    fn reads_shift_back_from_a_shifted_symbol() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        for (key, modifiers, signature) in [
            (">", ctrl, "CS:Period"),
            ("<", ctrl, "CS:Comma"),
            ("?", ctrl, "CS:Slash"),
            ("{", ctrl, "CS:BracketLeft"),
            ("}", ctrl, "CS:BracketRight"),
            ("_", ctrl, "CS:Minus"),
            ("+", ctrl, "CS:Equal"),
            (")", ctrl, "CS:Digit0"),
            ("+", alt, "AS:Equal"),
            ("_", alt, "AS:Minus"),
        ] {
            let chord = chord_event(&shifted_symbol(key, modifiers)).unwrap();
            assert_eq!(event_signature(&chord), signature, "{key}");
        }
        let plain = chord_event(&shifted_symbol(".", ctrl)).unwrap();
        assert_eq!(event_signature(&plain), "C:Period");
    }
}
