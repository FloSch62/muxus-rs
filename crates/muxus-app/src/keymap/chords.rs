//! Chord grammar (port of `client/src/keymap/chords.ts`).
//!
//! Zero or more modifiers followed by a `KeyboardEvent.code`, joined with `+`:
//! "Mod+Shift+ArrowRight", "Alt+Digit1", "Ctrl+PageDown".
//!
//! `Mod` resolves to Command on macOS and Control everywhere else, so one
//! default table serves every platform. Matching uses `code` (the physical
//! key) rather than `key`, which keeps chords stable across keyboard layouts
//! and unaffected by Shift/AltGr producing a different character.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Whether chords follow macOS conventions (`Mod` is Command, glyph labels).
pub const IS_MAC: bool = cfg!(target_os = "macos");

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// A key press in DOM `KeyboardEvent` terms. The UI translates its own key
/// events into this shape before looking up bindings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChordEvent {
    /// The character the key produced, e.g. "z" on the key printed Z.
    pub key: String,
    /// The physical key in US-layout terms, e.g. "KeyY" for that same key on QWERTZ.
    pub code: String,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
    pub meta_key: bool,
}

const MODIFIER_CODES: [&str; 9] = [
    "ControlLeft",
    "ControlRight",
    "AltLeft",
    "AltRight",
    "ShiftLeft",
    "ShiftRight",
    "MetaLeft",
    "MetaRight",
    "CapsLock",
];

fn key_label_override(code: &str) -> Option<&'static str> {
    Some(match code {
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "PageUp" => "PgUp",
        "PageDown" => "PgDn",
        "Escape" => "Esc",
        "Space" => "Space",
        "Equal" => "=",
        "Minus" => "-",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Backslash" => "\\",
        "Backquote" => "`",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Semicolon" => ";",
        "Quote" => "'",
        "Delete" => "Del",
        "Insert" => "Ins",
        _ => return None,
    })
}

/// Modifier keys never form a chord on their own.
pub fn is_modifier_code(code: &str) -> bool {
    MODIFIER_CODES.contains(&code)
}

/// Accept "T"/"1" shorthand alongside literal `KeyboardEvent.code` values.
fn canonical_code(token: &str) -> String {
    let mut chars = token.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if ch.is_ascii_alphabetic() {
            return format!("Key{}", ch.to_ascii_uppercase());
        }
        if ch.is_ascii_digit() {
            return format!("Digit{ch}");
        }
    }
    token.to_owned()
}

pub fn parse_chord(text: &str) -> Option<Chord> {
    let mut parts: Vec<&str> = text
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let key = parts.pop()?;
    let mut chord = Chord {
        code: canonical_code(key),
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    };
    if is_modifier_code(&chord.code) {
        return None;
    }
    for part in parts {
        match part.to_lowercase().as_str() {
            "mod" => {
                if IS_MAC {
                    chord.meta = true;
                } else {
                    chord.ctrl = true;
                }
            }
            "ctrl" | "control" => chord.ctrl = true,
            "alt" | "option" => chord.alt = true,
            "shift" => chord.shift = true,
            "meta" | "cmd" | "command" | "super" => chord.meta = true,
            _ => return None,
        }
    }
    Some(chord)
}

/// Stable lookup key for a chord; identical for equivalent chords.
pub fn chord_signature(chord: &Chord) -> String {
    format!(
        "{}{}{}{}:{}",
        if chord.ctrl { "C" } else { "" },
        if chord.alt { "A" } else { "" },
        if chord.shift { "S" } else { "" },
        if chord.meta { "M" } else { "" },
        chord.code
    )
}

pub fn event_signature(event: &ChordEvent) -> String {
    chord_signature(&chord_from_event(event))
}

/// Letters and digits are identified by the character the key produces, so a
/// chord is pressed on the cap it is printed on: Ctrl+Shift+Z is the key
/// labelled Z on a German keyboard, not the physical QWERTY position. Every
/// other key keeps its layout-independent code: arrows and page keys have no
/// character, and modified keys whose character is mangled (Option+1 on macOS,
/// Alt+1 on AZERTY) still resolve by position.
pub fn chord_from_event(event: &ChordEvent) -> Chord {
    Chord {
        code: printed_key_code(&event.key).unwrap_or_else(|| event.code.clone()),
        ctrl: event.ctrl_key,
        alt: event.alt_key,
        shift: event.shift_key,
        meta: event.meta_key,
    }
}

/// Punctuation that means the same key wherever it is printed: "?" is the
/// question-mark key on both US and German layouts even though they sit on
/// different caps. Characters a layout only produces with Shift over another
/// key (German ";" over the comma key) are deliberately absent, so those keep
/// resolving by position.
fn printed_punctuation(ch: char) -> Option<&'static str> {
    Some(match ch {
        '-' | '_' => "Minus",
        '=' | '+' => "Equal",
        '/' | '?' => "Slash",
        ',' => "Comma",
        '.' => "Period",
        '[' => "BracketLeft",
        ']' => "BracketRight",
        _ => return None,
    })
}

fn numpad_label(code: &str) -> Option<&'static str> {
    Some(match code {
        "NumpadAdd" => "+",
        "NumpadSubtract" => "-",
        "NumpadMultiply" => "*",
        "NumpadDivide" => "/",
        "NumpadDecimal" => ".",
        "NumpadEnter" => "Enter",
        _ => return None,
    })
}

fn printed_key_code(key: &str) -> Option<String> {
    // `key.length !== 1` in UTF-16 terms: one character from the Basic
    // Multilingual Plane.
    let mut chars = key.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if ch.len_utf16() != 1 {
        return None;
    }
    // `toLowerCase` keeps a single ASCII letter a single letter.
    let lower = ch.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        return Some(format!("Key{}", lower.to_ascii_uppercase()));
    }
    if lower.is_ascii_digit() {
        return Some(format!("Digit{lower}"));
    }
    printed_punctuation(ch).map(str::to_owned)
}

/// Serialize a captured chord with explicit modifiers (never "Mod").
pub fn chord_to_string(chord: &Chord) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if chord.ctrl {
        parts.push("Ctrl");
    }
    if chord.alt {
        parts.push("Alt");
    }
    if chord.shift {
        parts.push("Shift");
    }
    if chord.meta {
        parts.push("Meta");
    }
    parts.push(&chord.code);
    parts.join("+")
}

/// F1 through F24.
fn is_function_key(code: &str) -> bool {
    code.strip_prefix('F')
        .and_then(|number| {
            (!number.starts_with('0') && number.chars().all(|ch| ch.is_ascii_digit()))
                .then(|| number.parse::<u32>().ok())
                .flatten()
        })
        .is_some_and(|number| (1..=24).contains(&number))
}

/// A chord needs a non-Shift modifier (or a function key) so bindings can
/// never swallow ordinary typing in the terminal.
pub fn is_bindable_chord(chord: &Chord) -> bool {
    if is_modifier_code(&chord.code) {
        return false;
    }
    if is_function_key(&chord.code) {
        return true;
    }
    chord.ctrl || chord.alt || chord.meta
}

fn key_label(code: &str) -> String {
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.to_owned();
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.to_owned();
    }
    if let Some(rest) = code.strip_prefix("Numpad") {
        return format!("Num {}", numpad_label(code).unwrap_or(rest));
    }
    key_label_override(code).map_or_else(|| code.to_owned(), str::to_owned)
}

/// Platform-native rendering: "⌃⇧→" on macOS, "Ctrl+Shift+→" elsewhere.
pub fn format_chord(chord: &Chord) -> String {
    if IS_MAC {
        return format!(
            "{}{}{}{}{}",
            if chord.ctrl { "⌃" } else { "" },
            if chord.alt { "⌥" } else { "" },
            if chord.shift { "⇧" } else { "" },
            if chord.meta { "⌘" } else { "" },
            key_label(&chord.code)
        );
    }
    let mut parts: Vec<String> = Vec::new();
    if chord.ctrl {
        parts.push("Ctrl".into());
    }
    if chord.alt {
        parts.push("Alt".into());
    }
    if chord.shift {
        parts.push("Shift".into());
    }
    if chord.meta {
        parts.push("Meta".into());
    }
    parts.push(key_label(&chord.code));
    parts.join("+")
}

// Chord text comes from a fixed catalog plus the user's overrides, and every
// menu, tooltip, and palette row re-renders it. Parse each one once.
static CHORD_STRING_LABELS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(Default::default);

/// Render a stored chord string; unparseable input is echoed unchanged.
pub fn format_chord_string(text: &str) -> String {
    if let Ok(cache) = CHORD_STRING_LABELS.lock()
        && let Some(label) = cache.get(text)
    {
        return label.clone();
    }
    let label = parse_chord(text).map_or_else(|| text.to_owned(), |chord| format_chord(&chord));
    if let Ok(mut cache) = CHORD_STRING_LABELS.lock() {
        cache.insert(text.to_owned(), label.clone());
    }
    label
}

#[cfg(test)]
pub(crate) fn key_event(
    key: &str,
    code: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> ChordEvent {
    ChordEvent {
        key: key.into(),
        code: code.into(),
        ctrl_key: ctrl,
        alt_key: alt,
        shift_key: shift,
        meta_key: meta,
    }
}

// Tests run as a non-macOS platform, as the TypeScript ones do, so "Mod"
// resolves to Control.
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    fn ctrl_shift(key: &str, code: &str) -> ChordEvent {
        key_event(key, code, true, false, true, false)
    }

    #[test]
    fn parses_modifiers_key_shorthand_and_rejects_nonsense() {
        assert_eq!(
            parse_chord("Mod+Shift+ArrowRight"),
            Some(Chord {
                code: "ArrowRight".into(),
                ctrl: true,
                alt: false,
                shift: true,
                meta: false,
            })
        );
        assert_eq!(
            parse_chord("Mod+K").map(|chord| chord.code),
            Some("KeyK".into())
        );
        assert_eq!(
            parse_chord("Alt+1").map(|chord| chord.code),
            Some("Digit1".into())
        );
        assert_eq!(parse_chord("Hyper+KeyK"), None);
        assert_eq!(parse_chord("Ctrl+ShiftLeft"), None);
        assert_eq!(parse_chord(""), None);
    }

    #[test]
    fn resolves_punctuation_by_the_character_printed_on_the_cap_then_by_position() {
        let zoom_in = parse_chord("Ctrl+Shift+Equal").unwrap();
        assert_eq!(
            event_signature(&ctrl_shift("+", "Equal")),
            chord_signature(&zoom_in)
        );
        assert_ne!(
            event_signature(&key_event("=", "Equal", true, false, false, false)),
            chord_signature(&zoom_in)
        );
        // "?" lives over ß on German and over the slash key on US: same shortcut.
        let help = parse_chord("Ctrl+Shift+Slash").unwrap();
        assert_eq!(
            event_signature(&ctrl_shift("?", "Minus")),
            chord_signature(&help)
        );
        assert_eq!(
            event_signature(&ctrl_shift("?", "Slash")),
            chord_signature(&help)
        );
        // A character a layout only reaches over another key (German ";" over the
        // comma key) stays with the physical key it was typed on.
        assert_eq!(
            event_signature(&ctrl_shift(";", "Comma")),
            chord_signature(&parse_chord("Ctrl+Shift+Comma").unwrap())
        );
    }

    #[test]
    fn round_trips_a_captured_event_into_a_storable_chord() {
        let chord = chord_from_event(&ctrl_shift("T", "KeyT"));
        assert_eq!(chord_to_string(&chord), "Ctrl+Shift+KeyT");
        assert_eq!(
            chord_signature(&parse_chord(&chord_to_string(&chord)).unwrap()),
            chord_signature(&chord)
        );
    }

    #[test]
    fn only_accepts_chords_that_cannot_swallow_ordinary_typing() {
        assert!(!is_bindable_chord(&parse_chord("Shift+KeyT").unwrap()));
        assert!(!is_bindable_chord(&parse_chord("KeyT").unwrap()));
        assert!(is_bindable_chord(&parse_chord("F5").unwrap()));
        assert!(is_bindable_chord(&parse_chord("F24").unwrap()));
        assert!(!is_bindable_chord(&parse_chord("F25").unwrap()));
        assert!(!is_bindable_chord(&parse_chord("F05").unwrap()));
        assert!(is_bindable_chord(&parse_chord("Alt+ArrowLeft").unwrap()));
        assert!(is_modifier_code("ShiftLeft"));
    }

    #[test]
    fn renders_keys_the_way_the_shortcut_list_shows_them() {
        assert_eq!(
            format_chord(&parse_chord("Mod+Shift+ArrowRight").unwrap()),
            "Ctrl+Shift+→"
        );
        assert_eq!(format_chord(&parse_chord("Alt+Digit1").unwrap()), "Alt+1");
        assert_eq!(
            format_chord(&parse_chord("Ctrl+PageDown").unwrap()),
            "Ctrl+PgDn"
        );
        assert_eq!(
            format_chord(&parse_chord("Ctrl+NumpadAdd").unwrap()),
            "Ctrl+Num +"
        );
        assert_eq!(format_chord_string("Mod+Shift+KeyB"), "Ctrl+Shift+B");
        assert_eq!(format_chord_string("Hyper+KeyB"), "Hyper+KeyB");
    }

    #[test]
    fn stores_a_rebound_chord_under_the_letter_the_user_pressed() {
        assert_eq!(
            chord_to_string(&chord_from_event(&ctrl_shift("z", "KeyY"))),
            "Ctrl+Shift+KeyZ"
        );
    }
}
