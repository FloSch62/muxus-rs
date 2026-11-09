//! GPUI keystrokes as the DOM `KeyboardEvent`s the RDP and VNC clients take.
//!
//! In the TypeScript build the browser reported `code` (the physical key) and `key` (what it
//! produces), and keydown/keyup for the modifier keys themselves; the RDP client's Shift and
//! lock-key handling (`RdpKeyboard`) relies on all three. GPUI reports something narrower:
//!
//! - no physical key, only the layout's key name (`a`, `!`, `enter`, `f5`), so `code` is
//!   derived from that name in US-layout terms. On a US layout this is exactly what Chrome
//!   reports; on other layouts letters and symbols map to the US key that produces them. RDP
//!   therefore sends the characters keys type as Unicode events and keeps scancodes for
//!   shortcuts and named keys (`muxus_rdp`'s `typed_char`);
//! - no AltGr state: a character Alt (Ctrl+Alt on Windows, Option on macOS) changed is
//!   marked `alt_graph`, so RDP types it instead of sending an Alt shortcut;
//! - modifier keys produce no key events, only a modifiers-changed event, from which the
//!   modifier keydown/keyup pairs are rebuilt (always the left-hand key);
//! - Shift is dropped from a keystroke whose key is a symbol (`shift-1` arrives as `!`), so the
//!   flags of every event come from the window's live modifier state instead. Without this the
//!   RDP client would release Shift before the key and type `1` for `!`;
//! - Caps Lock state is known, Num Lock and Scroll Lock are not. Num Lock is reported as
//!   unknown, so the clients leave the remote one alone instead of switching it off; Scroll
//!   Lock is reported off;
//! - a key's release names it by what it types now, which is not what it typed when it went
//!   down once a modifier changed in between (`!` comes up as `1`, AltGr+Q's `@` as `q`). The
//!   translator remembers each held key and reports its release with the `code` and `key` it
//!   went down with, so the clients let go of what they pressed;
//! - dead keys and input methods compose in the platform's text input, as in the terminal.
//!   A key that types a character is therefore left to text input, and its character is
//!   typed when text input delivers it ([`KeyTranslator::text`]); a dead key types nothing
//!   until the key after it delivers the composed character.

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

/// Whether AltGr (or macOS Option) made the keystroke's character: Alt is held (Windows
/// reports AltGr as Ctrl+Alt) and the character differs from the key's own, as `@` from
/// AltGr+Q. Alt+F types a plain `f` and stays a shortcut.
fn alt_graph(keystroke: &Keystroke, modifiers: &GpuiModifiers) -> bool {
    modifiers.alt
        && !modifiers.platform
        && keystroke.key_char.as_deref().is_some_and(|typed| {
            !typed.is_empty()
                && !typed.chars().any(char::is_control)
                && typed.to_lowercase() != keystroke.key.to_lowercase()
        })
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
        num_lock: None,
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

/// The character a DOM `key` types: one printable character other than a space.
fn typed_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(typed), None) if !typed.is_control() && !typed.is_whitespace() => Some(typed),
        _ => None,
    }
}

/// Which keys' characters the platform's text input delivers to a view that takes text
/// input. Such a key's press is not sent: the character arrives afterwards (see
/// [`KeyTranslator::text`]) or, for a dead key, is held back until the key after it, which
/// then delivers the composed character (`´` then `e` is `é`, `^` then Space is `^`). Input
/// methods deliver what they compose the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextDelivery {
    /// No text input: a character goes out with its key's press.
    #[default]
    Off,
    /// Every key that types a character, AltGr's and Option's included (macOS, Windows).
    Always,
    /// Only keys pressed without Ctrl, Alt or Super: GPUI on Linux hands no others to text
    /// input (AltGr is not one of them there).
    Unmodified,
}

impl TextDelivery {
    /// What this platform's GPUI delivers.
    pub fn platform() -> Self {
        if cfg!(any(windows, target_os = "macos")) {
            Self::Always
        } else {
            Self::Unmodified
        }
    }
}

/// What the view does with a key's press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDown {
    /// Send this to the session.
    Send(KeyEvent),
    /// Let the platform's text input have the key: its character comes through
    /// [`KeyTranslator::text`].
    AwaitText,
    /// Nothing to send, and nothing for the platform to do either (a dead key repeating).
    Swallow,
}

/// How a held key reaches the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// As the event reported on its press.
    Key,
    /// Through text input, which has not delivered a character for it (yet): a dead key.
    Awaiting,
    /// As the character text input delivered for it.
    Text,
}

/// A key down on the desktop: the keystroke it went down with and how it was reported.
#[derive(Debug, Clone)]
struct HeldKey {
    keystroke: Keystroke,
    /// The window's modifiers when it went down.
    modifiers: GpuiModifiers,
    code: String,
    key: String,
    typing: Typing,
}

impl HeldKey {
    /// Whether the key typed a character (rather than naming a key such as Enter).
    fn typed(&self) -> bool {
        typed_char(&self.key).is_some()
    }

    /// Whether the key finished a composition: GPUI names such a key after the character it
    /// composed (`eacute`, `asciicircum`), not after the key that came up.
    fn composed(&self) -> bool {
        let key = self.keystroke.key.as_str();
        key.chars().count() > 1 && named_key(key).is_none() && self.typed()
    }
}

/// Turns one view's GPUI key events into DOM key events, rebuilding the modifier and Caps
/// Lock key events GPUI reports only as state.
#[derive(Debug, Default, Clone)]
pub struct KeyTranslator {
    modifiers: GpuiModifiers,
    caps_lock: bool,
    /// Keys down, oldest first.
    held: Vec<HeldKey>,
    text_input: TextDelivery,
    /// The newest held key waits for its character from text input.
    pending: bool,
}

impl KeyTranslator {
    /// A translator for a view without text input.
    pub fn new() -> Self {
        Self::default()
    }

    /// A translator for a view that takes text input, as this platform delivers it.
    pub fn with_text_input(text_input: TextDelivery) -> Self {
        Self {
            text_input,
            ..Self::default()
        }
    }

    /// Adopt the current state without reporting it (focus came back). A modifier held before
    /// the desktop had focus is pressed on the server with the next key, from its flags.
    pub fn sync(&mut self, modifiers: GpuiModifiers, caps_lock: bool) {
        self.modifiers = modifiers;
        self.caps_lock = caps_lock;
    }

    /// Every key counts as up again (focus left the desktop, which let go of them).
    pub fn release_all(&mut self) {
        self.held.clear();
        self.pending = false;
    }

    /// The held key a key event names. GPUI reports no physical key, so a key is found by its
    /// name: the same name, or the same US key (`!` comes up as `1` once Shift is up). A
    /// character key whose name changed otherwise (with a layout's own Shift level, as German
    /// Shift+7 `/` coming up as `7`; with AltGr, which GPUI does not report on Linux; or with
    /// a composition, `é` coming up as `e`) is the oldest key that typed a character, one
    /// pressed under other modifiers than now first; a named key (Space ending `^ Space`) can
    /// only be the end of a composition.
    fn held_index(&self, keystroke: &Keystroke) -> Option<usize> {
        let key = keystroke.key.as_str();
        if let Some(index) = self
            .held
            .iter()
            .position(|held| held.keystroke.key.eq_ignore_ascii_case(key))
        {
            return Some(index);
        }
        if let Some(code) = dom_code(key)
            && let Some(index) = self.held.iter().position(|held| held.code == code)
        {
            return Some(index);
        }
        let character = key.chars().count() == 1;
        let named = named_key(key).is_some();
        let candidate = |held: &HeldKey| (character && held.typed()) || (named && held.composed());
        self.held
            .iter()
            .position(|held| candidate(held) && held.modifiers != self.modifiers)
            .or_else(|| self.held.iter().position(candidate))
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

    /// Whether text input delivers the character of a key going down as `event`: a key
    /// that types something other than white space and is no shortcut (the keys RDP types as
    /// characters), keypad keys aside. What it types may be more than one character (Windows
    /// reports `´x` for X after a dead key it does not combine with) or come from a named key
    /// (Space after `^` types `^`).
    fn awaits_text(&self, keystroke: &Keystroke, event: &KeyEvent) -> bool {
        let unmodified = !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform
            && !keystroke.modifiers.function;
        let delivered = match self.text_input {
            TextDelivery::Off => false,
            TextDelivery::Always => true,
            TextDelivery::Unmodified => unmodified,
        };
        let flags = &event.modifiers;
        let types = keystroke
            .key_char
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty() && !text.chars().any(char::is_control));
        delivered
            && types
            && !event.code.starts_with("Numpad")
            && !flags.meta
            && (!(flags.ctrl || flags.alt) || flags.alt_graph)
    }

    /// A key went down (`repeat` for auto-repeat). `modifiers` and `caps_lock` are the window's
    /// live state, which keeps the Shift GPUI drops from symbol keystrokes.
    pub fn key_down(
        &mut self,
        keystroke: &Keystroke,
        repeat: bool,
        modifiers: GpuiModifiers,
        caps_lock: bool,
    ) -> KeyDown {
        self.sync(modifiers, caps_lock);
        // Auto-repeat repeats what the key went down with, whatever the modifiers are now.
        if repeat && let Some(index) = self.held_index(keystroke) {
            let held = &self.held[index];
            if held.typing == Typing::Awaiting {
                return KeyDown::Swallow;
            }
            let mut event = self.event(&held.code, held.key.clone(), true, true);
            event.modifiers.alt_graph =
                held.typing == Typing::Text || alt_graph(&held.keystroke, &self.modifiers);
            return KeyDown::Send(event);
        }
        let key = dom_key(keystroke, &self.modifiers, self.caps_lock);
        let code = dom_code(&keystroke.key).unwrap_or_default();
        let mut event = self.event(code, key, true, repeat);
        event.modifiers.alt_graph = alt_graph(keystroke, &self.modifiers);
        let awaits = self.awaits_text(keystroke, &event);
        // A key that goes down again without having come up here replaces its old entry.
        self.held
            .retain(|held| !held.keystroke.key.eq_ignore_ascii_case(&keystroke.key));
        self.held.push(HeldKey {
            keystroke: keystroke.clone(),
            modifiers: self.modifiers,
            code: event.code.clone(),
            key: event.key.clone(),
            typing: if awaits {
                Typing::Awaiting
            } else {
                Typing::Key
            },
        });
        self.pending = awaits;
        if awaits {
            KeyDown::AwaitText
        } else {
            KeyDown::Send(event)
        }
    }

    /// A key came up: the release of what it went down with, or nothing for a key whose
    /// character never came (a dead key).
    pub fn key_up(
        &mut self,
        keystroke: &Keystroke,
        modifiers: GpuiModifiers,
        caps_lock: bool,
    ) -> Option<KeyEvent> {
        self.sync(modifiers, caps_lock);
        if let Some(index) = self.held_index(keystroke) {
            if index + 1 == self.held.len() {
                self.pending = false;
            }
            let held = self.held.remove(index);
            if held.typing == Typing::Awaiting {
                return None;
            }
            let mut event = self.event(&held.code, held.key, false, false);
            event.modifiers.alt_graph =
                held.typing == Typing::Text || alt_graph(&held.keystroke, &self.modifiers);
            return Some(event);
        }
        let key = dom_key(keystroke, &self.modifiers, self.caps_lock);
        let code = dom_code(&keystroke.key).unwrap_or_default();
        let mut event = self.event(code, key, false, false);
        event.modifiers.alt_graph = alt_graph(keystroke, &self.modifiers);
        Some(event)
    }

    /// A character event typed through text input. It counts as AltGr's when Ctrl or Alt is
    /// held, so the clients type it rather than send a shortcut.
    fn text_event(&self, code: &str, typed: char, pressed: bool) -> KeyEvent {
        let mut event = self.event(code, typed.to_string(), pressed, false);
        event.modifiers.alt_graph = self.modifiers.alt || self.modifiers.control;
        event
    }

    /// Text the platform's text input delivered: what a key typed, a dead key's composition
    /// or an input method's commit. The last character belongs to the key waiting for it,
    /// which holds it until it comes up (a second delivery for the same key, as Windows
    /// sends `´` and `x` for a dead key followed by a key it does not combine with, lets go
    /// of the first); the others, and text no key waits for, are typed at once.
    pub fn text(&mut self, text: &str, modifiers: GpuiModifiers, caps_lock: bool) -> Vec<KeyEvent> {
        self.sync(modifiers, caps_lock);
        let mut chars: Vec<char> = text.chars().filter(|typed| !typed.is_control()).collect();
        let last = if self.pending { chars.pop() } else { None };
        let mut events = Vec::new();
        for typed in chars {
            events.push(self.text_event("", typed, true));
            events.push(self.text_event("", typed, false));
        }
        let (Some(typed), Some(held)) = (last, self.held.last()) else {
            return events;
        };
        if held.typing == Typing::Text
            && let Some(before) = typed_char(&held.key)
        {
            events.push(self.text_event(&held.code, before, false));
        }
        // The key's own code goes with its own character; a composed one has no key.
        let own = named_key(&held.keystroke.key).is_none()
            && held
                .keystroke
                .key_char
                .as_deref()
                .is_some_and(|own| own.chars().eq([typed]));
        let code = if own {
            dom_code(&held.keystroke.key).unwrap_or_default()
        } else {
            ""
        };
        let event = self.text_event(code, typed, true);
        if let Some(held) = self.held.last_mut() {
            held.code = event.code.clone();
            held.key = event.key.clone();
            held.typing = Typing::Text;
        }
        events.push(event);
        events
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

    impl KeyDown {
        fn sent(self) -> KeyEvent {
            match self {
                Self::Send(event) => event,
                other => panic!("nothing sent: {other:?}"),
            }
        }
    }

    fn stroke(text: &'static str) -> Keystroke {
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
        let event = KeyTranslator::new()
            .key_down(&typed("é", Some("é")), false, none, false)
            .sent();
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
    /// before the next key. Characters go as Unicode; Shift+Tab still needs Shift held.
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
            rdp.key(&keys.key_down(&bang, false, shift(), false).sent())
                .unwrap(),
        );
        ops.extend(
            rdp.key(&keys.key_up(&bang, shift(), false).unwrap())
                .unwrap(),
        );
        let capital = stroke("shift-a");
        ops.extend(
            rdp.key(&keys.key_down(&capital, false, shift(), false).sent())
                .unwrap(),
        );
        ops.extend(
            rdp.key(&keys.key_up(&capital, shift(), false).unwrap())
                .unwrap(),
        );
        let tab = typed("tab", None);
        ops.extend(
            rdp.key(&keys.key_down(&tab, false, shift(), false).sent())
                .unwrap(),
        );
        ops.extend(
            rdp.key(&keys.key_up(&tab, shift(), false).unwrap())
                .unwrap(),
        );
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
                    // Unknown locally: the RDP client assumes it on rather than forcing it off.
                    num_lock: true,
                    caps_lock: false
                },
                InputOp::KeyPressed(scancode::SHIFT_LEFT),
                // Shift stays down for the symbol, the capital and Shift+Tab.
                InputOp::UnicodePressed('!'),
                InputOp::UnicodeReleased('!'),
                InputOp::UnicodePressed('A'),
                InputOp::UnicodeReleased('A'),
                InputOp::KeyPressed(scancode::TAB),
                InputOp::KeyReleased(scancode::TAB),
                InputOp::KeyReleased(scancode::SHIFT_LEFT),
            ]
        );
    }

    /// What a German layout types on RDP: characters as Unicode, whatever US key the view
    /// guessed for them; shortcuts as keys.
    #[test]
    fn types_what_a_non_us_layout_types_on_rdp() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let none = GpuiModifiers::default();
        let mut press = |keystroke: Keystroke, modifiers: GpuiModifiers| {
            rdp.key(&keys.key_down(&keystroke, false, modifiers, false).sent())
        };
        let ctrl_alt = GpuiModifiers {
            control: true,
            alt: true,
            ..none
        };
        let alt = GpuiModifiers { alt: true, ..none };
        let sent = [
            press(typed("z", Some("z")), none),
            press(typed("/", Some("/")), shift()),
            // AltGr+Q on Linux (no modifier flag) and on Windows (Ctrl+Alt).
            press(typed("@", Some("@")), none),
            press(
                Keystroke {
                    modifiers: ctrl_alt,
                    key: "q".into(),
                    key_char: Some("@".into()),
                },
                ctrl_alt,
            ),
            press(typed("ö", Some("ö")), none),
        ];
        let unicode: Vec<_> = sent
            .iter()
            .map(|ops| {
                ops.as_ref().unwrap().iter().find_map(|op| match op {
                    InputOp::UnicodePressed(typed) => Some(*typed),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            unicode,
            [Some('z'), Some('/'), Some('@'), Some('@'), Some('ö')]
        );
        // Ctrl+Z and Alt+F stay shortcuts.
        let ctrl_z = press(typed("z", None), ctrl()).unwrap();
        assert_eq!(ctrl_z.last(), Some(&InputOp::KeyPressed(0x2c)));
        assert!(ctrl_z.contains(&InputOp::KeyPressed(scancode::CONTROL_LEFT)));
        let alt_f = press(typed("f", Some("f")), alt).unwrap();
        assert_eq!(alt_f.last(), Some(&InputOp::KeyPressed(0x21)));
        assert!(alt_f.contains(&InputOp::KeyPressed(scancode::ALT_LEFT)));
    }

    /// What RDP sends for a run of key events, after the first key's lock sync.
    fn rdp_ops(keys: &mut KeyTranslator, steps: &[Step]) -> Vec<InputOp> {
        let mut rdp = RdpKeyboard::new();
        let mut ops = Vec::new();
        for step in steps {
            let events = match step {
                Step::Down(keystroke, modifiers) | Step::Repeat(keystroke, modifiers) => {
                    let repeat = matches!(step, Step::Repeat(..));
                    match keys.key_down(keystroke, repeat, *modifiers, false) {
                        KeyDown::Send(event) => vec![event],
                        KeyDown::AwaitText | KeyDown::Swallow => Vec::new(),
                    }
                }
                Step::Up(keystroke, modifiers) => keys
                    .key_up(keystroke, *modifiers, false)
                    .into_iter()
                    .collect(),
                Step::Text(text, modifiers) => keys.text(text, *modifiers, false),
            };
            for event in events {
                ops.extend(rdp.key(&event).unwrap_or_default());
            }
        }
        ops.retain(|op| !matches!(op, InputOp::SynchronizeLocks { .. }));
        ops
    }

    enum Step {
        Down(Keystroke, GpuiModifiers),
        Repeat(Keystroke, GpuiModifiers),
        Up(Keystroke, GpuiModifiers),
        /// What the platform's text input delivered.
        Text(&'static str, GpuiModifiers),
    }

    /// A key comes up named by what it types then; the server must let go of what it pressed.
    #[test]
    fn releases_the_character_a_key_went_down_with() {
        let none = GpuiModifiers::default();
        let mut keys = KeyTranslator::new();
        // US Shift+1, Shift let go first: `!` comes up as `1`, and repeats stay `!`.
        // Linux AltGr+Q (GPUI reports no modifier): `@` comes up as `q`.
        let ops = rdp_ops(
            &mut keys,
            &[
                Step::Down(typed("!", Some("!")), shift()),
                Step::Repeat(typed("1", Some("1")), none),
                Step::Up(typed("1", Some("1")), none),
                Step::Down(typed("@", Some("@")), none),
                Step::Up(typed("q", Some("q")), none),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('!'),
                InputOp::UnicodeRepeated('!'),
                InputOp::UnicodeReleased('!'),
                InputOp::UnicodePressed('@'),
                InputOp::UnicodeReleased('@'),
            ]
        );
        assert!(keys.held.is_empty());
    }

    #[test]
    fn matches_releases_of_keys_held_together() {
        let none = GpuiModifiers::default();
        let mut keys = KeyTranslator::new();
        // German Shift+7 and Shift+8 held, Shift let go, then the keys: the oldest goes
        // first. W held throughout comes up by its own name.
        let ops = rdp_ops(
            &mut keys,
            &[
                Step::Down(typed("w", Some("w")), none),
                Step::Down(typed("/", Some("/")), shift()),
                Step::Down(typed("(", Some("(")), shift()),
                Step::Up(typed("7", Some("7")), none),
                Step::Up(typed("8", Some("8")), none),
                // A dead key coming up (its press went to the input method) lets go of nothing.
                Step::Up(typed("dead_acute", None), none),
                Step::Up(typed("w", Some("w")), none),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('w'),
                InputOp::UnicodePressed('/'),
                InputOp::UnicodePressed('('),
                InputOp::UnicodeReleased('/'),
                InputOp::UnicodeReleased('('),
                InputOp::UnicodeReleased('w'),
            ]
        );
    }

    /// GPUI on Linux names the key that finishes a composition after what it composed; it
    /// comes up under its own name.
    #[test]
    fn releases_a_composed_character_when_its_key_comes_up() {
        let none = GpuiModifiers::default();
        let mut keys = KeyTranslator::new();
        let ops = rdp_ops(
            &mut keys,
            &[
                Step::Down(typed("eacute", Some("é")), none),
                Step::Up(typed("e", Some("e")), none),
                Step::Down(typed("asciicircum", Some("^")), none),
                Step::Up(typed("space", Some(" ")), none),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('é'),
                InputOp::UnicodeReleased('é'),
                InputOp::UnicodePressed('^'),
                InputOp::UnicodeReleased('^'),
            ]
        );
        // Focus leaving forgets what was held.
        keys.key_down(&typed("a", Some("a")), false, none, false)
            .sent();
        keys.release_all();
        assert!(keys.held.is_empty());
    }

    /// GPUI on Linux: a dead key types nothing itself (its accent waits as the composition),
    /// and the key after it arrives named after the composed character, which text input
    /// then delivers. Shortcuts and keys with Alt still go out on their press.
    #[test]
    fn types_dead_key_compositions_delivered_by_text_input_on_linux() {
        let none = GpuiModifiers::default();
        let mut keys = KeyTranslator::with_text_input(TextDelivery::Unmodified);
        assert_eq!(
            keys.key_down(&typed("eacute", Some("é")), false, none, false),
            KeyDown::AwaitText
        );
        keys.release_all();
        let alt = GpuiModifiers { alt: true, ..none };
        let ops = rdp_ops(
            &mut keys,
            &[
                // ´ then e: é, held until E comes up.
                Step::Down(typed("dead_acute", None), none),
                Step::Up(typed("dead_acute", None), none),
                Step::Down(typed("eacute", Some("é")), none),
                Step::Text("é", none),
                Step::Up(typed("e", Some("e")), none),
                // ´ then x, which it does not combine with: the accent, then x.
                Step::Down(typed("dead_acute", None), none),
                Step::Up(typed("dead_acute", None), none),
                Step::Text("´", none),
                Step::Down(typed("x", Some("x")), none),
                Step::Text("x", none),
                Step::Up(typed("x", Some("x")), none),
                // An input method's commit, no key waiting.
                Step::Text("日本", none),
                // Alt+F is a shortcut: GPUI hands it no text.
                Step::Down(
                    Keystroke {
                        modifiers: alt,
                        key: "f".into(),
                        key_char: Some("f".into()),
                    },
                    alt,
                ),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('é'),
                InputOp::UnicodeReleased('é'),
                InputOp::UnicodePressed('´'),
                InputOp::UnicodeReleased('´'),
                InputOp::UnicodePressed('x'),
                InputOp::UnicodeReleased('x'),
                InputOp::UnicodePressed('日'),
                InputOp::UnicodeReleased('日'),
                InputOp::UnicodePressed('本'),
                InputOp::UnicodeReleased('本'),
                InputOp::KeyPressed(scancode::ALT_LEFT),
                InputOp::KeyPressed(0x21),
            ]
        );
    }

    /// GPUI on Windows and macOS reports a dead key as its accent; sending it would type the
    /// accent and lose the composition, so it goes to text input, which types nothing until
    /// the next key delivers the composed character (Windows, with the key's own event) or
    /// commits it without one (macOS).
    #[test]
    fn leaves_dead_keys_to_text_input_on_windows_and_macos() {
        let none = GpuiModifiers::default();
        let ctrl_alt = GpuiModifiers {
            control: true,
            alt: true,
            ..none
        };
        let option = GpuiModifiers { alt: true, ..none };
        let mut keys = KeyTranslator::with_text_input(TextDelivery::Always);
        let ops = rdp_ops(
            &mut keys,
            &[
                // Windows: ´ (held long enough to repeat), then e.
                Step::Down(typed("´", Some("´")), none),
                Step::Repeat(typed("´", Some("´")), none),
                Step::Up(typed("´", Some("´")), none),
                Step::Down(typed("e", Some("é")), none),
                Step::Text("é", none),
                Step::Up(typed("e", Some("e")), none),
                // Windows: ´ then x delivers both characters for X.
                Step::Down(typed("´", Some("´")), none),
                Step::Up(typed("´", Some("´")), none),
                Step::Down(typed("x", Some("´x")), none),
                Step::Text("´", none),
                Step::Text("x", none),
                Step::Up(typed("x", Some("x")), none),
                // Windows: ^ then Space types ^, held while Space is.
                Step::Down(typed("^", Some("^")), none),
                Step::Up(typed("^", Some("^")), none),
                Step::Down(typed("space", Some("^")), none),
                Step::Text("^", none),
                Step::Up(typed("space", Some(" ")), none),
                // A plain Space is a key.
                Step::Down(typed("space", Some(" ")), none),
                // Windows AltGr+Q: typed, not a Ctrl+Alt shortcut.
                Step::Down(
                    Keystroke {
                        modifiers: ctrl_alt,
                        key: "q".into(),
                        key_char: Some("@".into()),
                    },
                    ctrl_alt,
                ),
                Step::Text("@", ctrl_alt),
                Step::Up(typed("q", None), ctrl_alt),
                // macOS Option+E is a dead key; the E after it never reaches the view, its
                // é comes as a commit and E's release lets go of nothing.
                Step::Down(
                    Keystroke {
                        modifiers: option,
                        key: "e".into(),
                        key_char: Some("´".into()),
                    },
                    option,
                ),
                Step::Up(typed("e", None), option),
                Step::Text("é", none),
                Step::Up(typed("e", Some("e")), none),
                // Ctrl+C stays a shortcut.
                Step::Down(typed("c", None), ctrl()),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('é'),
                InputOp::UnicodeReleased('é'),
                InputOp::UnicodePressed('´'),
                InputOp::UnicodeReleased('´'),
                InputOp::UnicodePressed('x'),
                InputOp::UnicodeReleased('x'),
                InputOp::UnicodePressed('^'),
                InputOp::UnicodeReleased('^'),
                InputOp::KeyPressed(0x39),
                InputOp::UnicodePressed('@'),
                InputOp::UnicodeReleased('@'),
                InputOp::UnicodePressed('é'),
                InputOp::UnicodeReleased('é'),
                InputOp::KeyPressed(scancode::CONTROL_LEFT),
                InputOp::KeyPressed(0x2e),
            ]
        );
    }

    #[test]
    fn repeats_and_releases_a_character_text_input_delivered() {
        let none = GpuiModifiers::default();
        let mut keys = KeyTranslator::with_text_input(TextDelivery::Unmodified);
        let ops = rdp_ops(
            &mut keys,
            &[
                Step::Down(typed("w", Some("w")), none),
                Step::Text("w", none),
                Step::Repeat(typed("w", Some("w")), none),
                Step::Up(typed("w", Some("w")), none),
            ],
        );
        assert_eq!(
            ops,
            [
                InputOp::UnicodePressed('w'),
                InputOp::UnicodeRepeated('w'),
                InputOp::UnicodeReleased('w'),
            ]
        );
        // The key's own character keeps its key's code (VNC servers with scancodes need it).
        keys.key_down(&typed("a", Some("a")), false, none, false);
        assert_eq!(keys.text("a", none, false)[0].code, "KeyA");
    }

    #[test]
    fn marks_auto_repeat_and_skips_it_for_lock_keys_on_rdp() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let none = GpuiModifiers::default();
        let held = typed("backspace", None);
        rdp.key(&keys.key_down(&held, false, none, false).sent())
            .unwrap();
        let repeat = keys.key_down(&held, true, none, false).sent();
        assert!(repeat.repeat);
        assert_eq!(rdp.key(&repeat).unwrap(), [InputOp::KeyPressed(0x0e)]);
        let caps = keys.modifiers_changed(none, true);
        // A Caps Lock press toggles the server's lock itself, without a sync first.
        assert_eq!(rdp.key(&caps[0]).unwrap(), [InputOp::KeyPressed(0x3a)]);
    }

    #[test]
    fn reports_keys_rdp_cannot_send_as_unknown() {
        let mut keys = KeyTranslator::new();
        let mut rdp = RdpKeyboard::new();
        let event = keys
            .key_down(
                &typed("xf86audioplay", None),
                false,
                GpuiModifiers::default(),
                false,
            )
            .sent();
        assert_eq!(event.code, "");
        assert_eq!(event.key, "Unidentified");
        assert!(rdp.key(&event).is_none());
    }
}
