//! GPUI keystrokes as the DOM `KeyboardEvent`s the RDP and VNC clients take.
//!
//! In the TypeScript build the browser reported `code` (the physical key) and `key` (what it
//! produces), and keydown/keyup for the modifier keys themselves; the RDP client's Shift and
//! lock-key handling (`RdpKeyboard`) relies on all three. GPUI reports something narrower:
//!
//! - no physical key, only the layout's key name (`a`, `!`, `enter`, `f5`), so `code` is
//!   derived from that name in US-layout terms. On a US layout this is exactly what Chrome
//!   reports; on other layouts letters and symbols map to the US key that produces them;
//! - modifier keys produce no key events, only a modifiers-changed event, from which the
//!   modifier keydown/keyup pairs are rebuilt (always the left-hand key);
//! - Shift is dropped from a keystroke whose key is a symbol (`shift-1` arrives as `!`), so the
//!   flags of every event come from the window's live modifier state instead. Without this the
//!   RDP client would release Shift before the key and type `1` for `!`;
//! - Caps Lock state is known, Num Lock and Scroll Lock are not (reported off).

use muxus_session::desktop::{KeyEvent, Modifiers};
use muxus_ui::gpui_kit::{Keystroke, Modifiers as GpuiModifiers};

/// The DOM `code` for a GPUI key name, in US-layout terms.
pub fn dom_code(key: &str) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGITS: [&str; 10] = [
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];
    const FUNCTION_KEYS: [&str; 24] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14",
        "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
    ];
    let mut chars = key.chars();
    if let (Some(first), None) = (chars.next(), chars.next()) {
        if first.is_ascii_alphabetic() {
            return Some(LETTERS[usize::from(first.to_ascii_lowercase() as u8 - b'a')]);
        }
        if first.is_ascii_digit() {
            return Some(DIGITS[usize::from(first as u8 - b'0')]);
        }
        return Some(match first {
            // Shifted digits on a US keyboard.
            ')' => "Digit0",
            '!' => "Digit1",
            '@' => "Digit2",
            '#' => "Digit3",
            '$' => "Digit4",
            '%' => "Digit5",
            '^' => "Digit6",
            '&' => "Digit7",
            '*' => "Digit8",
            '(' => "Digit9",
            '-' | '_' => "Minus",
            '=' | '+' => "Equal",
            '[' | '{' => "BracketLeft",
            ']' | '}' => "BracketRight",
            '\\' | '|' => "Backslash",
            ';' | ':' => "Semicolon",
            '\'' | '"' => "Quote",
            ',' | '<' => "Comma",
            '.' | '>' => "Period",
            '/' | '?' => "Slash",
            '`' | '~' => "Backquote",
            _ => return None,
        });
    }
    if let Some(number) = key.strip_prefix('f')
        && let Ok(number) = number.parse::<usize>()
        && (1..=24).contains(&number)
    {
        return Some(FUNCTION_KEYS[number - 1]);
    }
    Some(match key {
        "enter" => "Enter",
        "tab" => "Tab",
        "space" => "Space",
        "backspace" => "Backspace",
        "escape" => "Escape",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "print" => "PrintScreen",
        "pause" => "Pause",
        "scroll_lock" => "ScrollLock",
        "num_lock" => "NumLock",
        "caps_lock" => "CapsLock",
        "menu" => "ContextMenu",
        // Keypad keys arrive without their `kp_` prefix.
        "add" => "NumpadAdd",
        "subtract" => "NumpadSubtract",
        "multiply" => "NumpadMultiply",
        "divide" => "NumpadDivide",
        "decimal" => "NumpadDecimal",
        "separator" => "NumpadComma",
        "begin" => "Numpad5",
        _ => return None,
    })
}

/// The DOM `key` for a named GPUI key, `None` for keys named by their character.
fn named_key(key: &str) -> Option<String> {
    if let Some(number) = key.strip_prefix('f')
        && number
            .parse::<u8>()
            .is_ok_and(|number| (1..=24).contains(&number))
    {
        return Some(format!("F{number}"));
    }
    Some(
        match key {
            "enter" => "Enter",
            "tab" => "Tab",
            "space" => " ",
            "backspace" => "Backspace",
            "escape" => "Escape",
            "delete" => "Delete",
            "insert" => "Insert",
            "home" => "Home",
            "end" => "End",
            "pageup" => "PageUp",
            "pagedown" => "PageDown",
            "left" => "ArrowLeft",
            "right" => "ArrowRight",
            "up" => "ArrowUp",
            "down" => "ArrowDown",
            "print" => "PrintScreen",
            "pause" => "Pause",
            "scroll_lock" => "ScrollLock",
            "num_lock" => "NumLock",
            "caps_lock" => "CapsLock",
            "menu" => "ContextMenu",
            "add" => "+",
            "subtract" => "-",
            "multiply" => "*",
            "divide" => "/",
            "decimal" => ".",
            "separator" => ",",
            "begin" => "Clear",
            _ => return None,
        }
        .to_owned(),
    )
}

/// The DOM `key` for a keystroke: the character it types, or the named key.
pub fn dom_key(keystroke: &Keystroke, modifiers: &GpuiModifiers, caps_lock: bool) -> String {
    if let Some(named) = named_key(&keystroke.key) {
        return named;
    }
    // GPUI leaves the character out under Ctrl (it would be a control code); the browser
    // still reports the letter.
    if let Some(typed) = &keystroke.key_char
        && !typed.is_empty()
        && !typed.chars().any(char::is_control)
    {
        return typed.clone();
    }
    let mut chars = keystroke.key.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), None) if letter.is_ascii_alphabetic() => {
            if modifiers.shift != caps_lock {
                letter.to_ascii_uppercase().to_string()
            } else {
                letter.to_ascii_lowercase().to_string()
            }
        }
        (Some(_), None) => keystroke.key.clone(),
        _ => "Unidentified".to_owned(),
    }
}

/// `KeyboardEvent`'s modifier flags for this state.
pub fn dom_modifiers(modifiers: &GpuiModifiers, caps_lock: bool) -> Modifiers {
    Modifiers {
        shift: modifiers.shift,
        ctrl: modifiers.control,
        alt: modifiers.alt,
        meta: modifiers.platform,
        alt_graph: false,
        caps_lock,
        num_lock: false,
        scroll_lock: false,
    }
}

/// Whether a modifier is held, its DOM `code` and its DOM `key`.
type ModifierKey = (fn(&GpuiModifiers) -> bool, &'static str, &'static str);

/// The modifier keys GPUI reports as state, with the key a browser would name for each.
const MODIFIER_KEYS: [ModifierKey; 4] = [
    (|m| m.control, "ControlLeft", "Control"),
    (|m| m.shift, "ShiftLeft", "Shift"),
    (|m| m.alt, "AltLeft", "Alt"),
    (|m| m.platform, "MetaLeft", "Meta"),
];

/// Turns one view's GPUI key events into DOM key events, rebuilding the modifier and Caps
/// Lock key events GPUI reports only as state.
#[derive(Debug, Default, Clone)]
pub struct KeyTranslator {
    modifiers: GpuiModifiers,
    caps_lock: bool,
}

impl KeyTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the current state without reporting it (focus came back). A modifier held before
    /// the desktop had focus is pressed on the server with the next key, from its flags.
    pub fn sync(&mut self, modifiers: GpuiModifiers, caps_lock: bool) {
        self.modifiers = modifiers;
        self.caps_lock = caps_lock;
    }

    fn event(&self, code: &str, key: String, pressed: bool, repeat: bool) -> KeyEvent {
        KeyEvent {
            code: code.to_owned(),
            key,
            pressed,
            repeat,
            modifiers: dom_modifiers(&self.modifiers, self.caps_lock),
        }
    }

    /// A key went down (`repeat` for auto-repeat). `modifiers` and `caps_lock` are the window's
    /// live state, which keeps the Shift GPUI drops from symbol keystrokes.
    pub fn key_down(
        &mut self,
        keystroke: &Keystroke,
        repeat: bool,
        modifiers: GpuiModifiers,
        caps_lock: bool,
    ) -> KeyEvent {
        self.sync(modifiers, caps_lock);
        let key = dom_key(keystroke, &self.modifiers, self.caps_lock);
        let code = dom_code(&keystroke.key).unwrap_or_default();
        self.event(code, key, true, repeat)
    }

    pub fn key_up(
        &mut self,
        keystroke: &Keystroke,
        modifiers: GpuiModifiers,
        caps_lock: bool,
    ) -> KeyEvent {
        self.sync(modifiers, caps_lock);
        let key = dom_key(keystroke, &self.modifiers, self.caps_lock);
        let code = dom_code(&keystroke.key).unwrap_or_default();
        self.event(code, key, false, false)
    }

    /// The modifier (and Caps Lock) key events behind a change of modifier state, presses
    /// first, in the order a browser would see a chord pressed.
    pub fn modifiers_changed(
        &mut self,
        modifiers: GpuiModifiers,
        caps_lock: bool,
    ) -> Vec<KeyEvent> {
        let before = std::mem::replace(&mut self.modifiers, modifiers);
        let caps_before = std::mem::replace(&mut self.caps_lock, caps_lock);
        let mut events = Vec::new();
        for (held, code, key) in MODIFIER_KEYS {
            if held(&modifiers) && !held(&before) {
                events.push(self.event(code, key.to_owned(), true, false));
            }
        }
        for (held, code, key) in MODIFIER_KEYS.iter().rev() {
            if !held(&modifiers) && held(&before) {
                events.push(self.event(code, (*key).to_owned(), false, false));
            }
        }
        if caps_lock != caps_before {
            // The browser reports the toggle as a press and release of the key itself.
            events.push(self.event("CapsLock", "CapsLock".into(), true, false));
            events.push(self.event("CapsLock", "CapsLock".into(), false, false));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use muxus_session::desktop::rdp::input::{InputOp, RdpKeyboard};
    use muxus_session::desktop::rdp::scancodes::scancode;

    use super::*;

    fn stroke(text: &str) -> Keystroke {
        Keystroke::parse(text).unwrap()
    }

    /// A keystroke as GPUI on Linux reports it: `key`, the typed character, and GPUI's own
    /// modifier flags (Shift dropped for symbols).
    fn typed(key: &str, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers: GpuiModifiers::default(),
            key: key.into(),
            key_char: key_char.map(str::to_owned),
        }
    }

    fn shift() -> GpuiModifiers {
        GpuiModifiers {
            shift: true,
            ..GpuiModifiers::default()
        }
    }

    fn ctrl() -> GpuiModifiers {
        GpuiModifiers {
            control: true,
            ..GpuiModifiers::default()
        }
    }

    #[test]
    fn names_physical_keys_in_us_layout_terms() {
        assert_eq!(dom_code("a"), Some("KeyA"));
        assert_eq!(dom_code("Z"), Some("KeyZ"));
        assert_eq!(dom_code("7"), Some("Digit7"));
        assert_eq!(dom_code("!"), Some("Digit1"));
        assert_eq!(dom_code(")"), Some("Digit0"));
        assert_eq!(dom_code("_"), Some("Minus"));
        assert_eq!(dom_code("+"), Some("Equal"));
        assert_eq!(dom_code("{"), Some("BracketLeft"));
        assert_eq!(dom_code("|"), Some("Backslash"));
        assert_eq!(dom_code(":"), Some("Semicolon"));
        assert_eq!(dom_code("\""), Some("Quote"));
        assert_eq!(dom_code("<"), Some("Comma"));
        assert_eq!(dom_code("?"), Some("Slash"));
        assert_eq!(dom_code("~"), Some("Backquote"));
        assert_eq!(dom_code("enter"), Some("Enter"));
        assert_eq!(dom_code("space"), Some("Space"));
        assert_eq!(dom_code("left"), Some("ArrowLeft"));
        assert_eq!(dom_code("pagedown"), Some("PageDown"));
        assert_eq!(dom_code("f1"), Some("F1"));
        assert_eq!(dom_code("f24"), Some("F24"));
        assert_eq!(dom_code("f25"), None);
        assert_eq!(dom_code("print"), Some("PrintScreen"));
        assert_eq!(dom_code("menu"), Some("ContextMenu"));
        assert_eq!(dom_code("add"), Some("NumpadAdd"));
        assert_eq!(dom_code("é"), None);
        assert_eq!(dom_code("xf86audioplay"), None);
    }

    #[test]
    fn reports_what_a_key_types() {
        let none = GpuiModifiers::default();
        assert_eq!(dom_key(&typed("a", Some("a")), &none, false), "a");
        assert_eq!(dom_key(&stroke("shift-a"), &shift(), false), "A");
        assert_eq!(dom_key(&typed("!", Some("!")), &shift(), false), "!");
        // Ctrl leaves no character; the browser still names the letter.
        assert_eq!(dom_key(&typed("c", None), &ctrl(), false), "c");
        assert_eq!(dom_key(&typed("c", None), &ctrl(), true), "C");
        assert_eq!(dom_key(&typed("enter", None), &none, false), "Enter");
        assert_eq!(dom_key(&typed("space", Some(" ")), &none, false), " ");
        assert_eq!(dom_key(&typed("tab", None), &shift(), false), "Tab");
        assert_eq!(dom_key(&typed("escape", None), &none, false), "Escape");
        assert_eq!(dom_key(&typed("f5", None), &none, false), "F5");
        assert_eq!(dom_key(&typed("up", None), &none, false), "ArrowUp");
        assert_eq!(dom_key(&typed("add", Some("+")), &none, false), "+");
        // Characters outside ASCII come through as typed (VNC sends their keysyms).
        assert_eq!(dom_key(&typed("é", Some("é")), &none, false), "é");
        let event = KeyTranslator::new().key_down(&typed("é", Some("é")), false, none, false);
        assert_eq!((event.code.as_str(), event.key.as_str()), ("", "é"));
    }

    #[test]
    fn rebuilds_modifier_key_events_from_state_changes() {
        let mut keys = KeyTranslator::new();
        let shift_and_ctrl = GpuiModifiers {
            shift: true,
            control: true,
            ..GpuiModifiers::default()
        };
        let pressed = keys.modifiers_changed(shift_and_ctrl, false);
        let described: Vec<_> = pressed
            .iter()
            .map(|event| (event.code.as_str(), event.key.as_str(), event.pressed))
            .collect();
        assert_eq!(
            described,
            [
                ("ControlLeft", "Control", true),
                ("ShiftLeft", "Shift", true)
            ]
        );
        // A keydown of a modifier carries its own flag, as in the browser.
        assert!(pressed[1].modifiers.shift && pressed[1].modifiers.ctrl);
        let released = keys.modifiers_changed(ctrl(), false);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].code, "ShiftLeft");
        assert!(!released[0].pressed);
        assert!(!released[0].modifiers.shift);
        let toggled = keys.modifiers_changed(ctrl(), true);
        let described: Vec<_> = toggled
            .iter()
            .map(|event| {
                (
                    event.code.as_str(),
                    event.pressed,
                    event.modifiers.caps_lock,
                )
            })
            .collect();
        assert_eq!(
            described,
            [("CapsLock", true, true), ("CapsLock", false, true)]
        );
        assert!(keys.modifiers_changed(ctrl(), true).is_empty());
    }

    /// The RDP Shift fix end to end: GPUI drops Shift from `shift-1` (it arrives as `!`), so
    /// the event's flags must come from the window state, or the client would let go of Shift
    /// before the key and the server would type `1`.
    #[test]
    fn keeps_shift_held_for_symbols_on_rdp() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let mut ops = Vec::new();
        for event in keys.modifiers_changed(shift(), false) {
            ops.extend(rdp.key(&event).unwrap());
        }
        let bang = typed("!", Some("!"));
        assert!(!bang.modifiers.shift, "GPUI reports no Shift on a symbol");
        ops.extend(
            rdp.key(&keys.key_down(&bang, false, shift(), false))
                .unwrap(),
        );
        ops.extend(rdp.key(&keys.key_up(&bang, shift(), false)).unwrap());
        let capital = stroke("shift-a");
        ops.extend(
            rdp.key(&keys.key_down(&capital, false, shift(), false))
                .unwrap(),
        );
        ops.extend(rdp.key(&keys.key_up(&capital, shift(), false)).unwrap());
        for event in keys.modifiers_changed(GpuiModifiers::default(), false) {
            ops.extend(rdp.key(&event).unwrap());
        }
        assert_eq!(
            ops,
            [
                // The first key tells the server the lock state (that releases every key on
                // the server, so it comes before Shift goes down).
                InputOp::SynchronizeLocks {
                    scroll_lock: false,
                    num_lock: false,
                    caps_lock: false
                },
                InputOp::KeyPressed(scancode::SHIFT_LEFT),
                // Shift stays down for the symbol and the capital.
                InputOp::KeyPressed(0x02),
                InputOp::KeyReleased(0x02),
                InputOp::KeyPressed(0x1e),
                InputOp::KeyReleased(0x1e),
                InputOp::KeyReleased(scancode::SHIFT_LEFT),
            ]
        );
    }

    #[test]
    fn marks_auto_repeat_and_skips_it_for_lock_keys_on_rdp() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let none = GpuiModifiers::default();
        let held = typed("a", Some("a"));
        rdp.key(&keys.key_down(&held, false, none, false)).unwrap();
        let repeat = keys.key_down(&held, true, none, false);
        assert!(repeat.repeat);
        assert_eq!(rdp.key(&repeat).unwrap(), [InputOp::KeyPressed(0x1e)]);
        let caps = keys.modifiers_changed(none, true);
        // A Caps Lock press toggles the server's lock itself, without a sync first.
        assert_eq!(rdp.key(&caps[0]).unwrap(), [InputOp::KeyPressed(0x3a)]);
    }

    #[test]
    fn reports_keys_rdp_cannot_send_as_unknown() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let event = keys.key_down(
            &typed("xf86audioplay", None),
            false,
            GpuiModifiers::default(),
            false,
        );
        assert_eq!(event.code, "");
        assert_eq!(event.key, "Unidentified");
        assert!(rdp.key(&event).is_none());
    }
}
