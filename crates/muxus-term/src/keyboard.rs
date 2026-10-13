//! Key events → the bytes a terminal program reads.
//!
//! The encoders reproduce xterm.js exactly: the legacy encoder is adapted from
//! `src/common/input/Keyboard.ts`, the kitty keyboard protocol from
//! `src/common/input/KittyKeyboard.ts` and the keydown/keypress flow from
//! `src/browser/CoreBrowserTerminal.ts` (xterm.js, MIT). Muxus's own normalization
//! (`client/src/terminal/keyboard-input.ts`) is applied on top.
//!
//! Events are described the way a browser describes them ([`KeyboardEvent`]: DOM `key`, `code`
//! and legacy `keyCode`), so the encoders stay a line-for-line port. [`KeyboardEvent::from_key`]
//! builds one from a toolkit key name.

const ESC: &str = "\x1b";

/// A key event in DOM terms.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyboardEvent {
    /// DOM `key`: the produced character ("a", "A", "é") or a key name ("Enter", "ArrowUp", "F5").
    pub key: String,
    /// DOM `code`: the physical key ("KeyA", "Digit1", "NumpadEnter", "ShiftLeft").
    pub code: String,
    /// Legacy DOM `keyCode` (65 for A, 13 for Enter, 112 for F1, …).
    pub key_code: u32,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    /// The key is held down and the platform repeats it.
    pub repeat: bool,
}

/// What a keydown asks of the terminal beyond (or instead of) sending bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardResultType {
    #[default]
    SendKey,
    PageUp,
    PageDown,
    SelectAll,
}

/// The encoder's verdict on one key event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyboardResult {
    pub kind: KeyboardResultType,
    /// The event is consumed even when it produces no bytes.
    pub cancel: bool,
    /// Bytes to send, if any.
    pub key: Option<String>,
}

// Regular and shifted characters of the digit and punctuation keys, by keyCode.
fn keycode_key_mapping(key_code: u32) -> Option<(&'static str, &'static str)> {
    Some(match key_code {
        48 => ("0", ")"),
        49 => ("1", "!"),
        50 => ("2", "@"),
        51 => ("3", "#"),
        52 => ("4", "$"),
        53 => ("5", "%"),
        54 => ("6", "^"),
        55 => ("7", "&"),
        56 => ("8", "*"),
        57 => ("9", "("),
        186 => (";", ":"),
        187 => ("=", "+"),
        188 => (",", "<"),
        189 => ("-", "_"),
        190 => (".", ">"),
        191 => ("/", "?"),
        192 => ("`", "~"),
        219 => ("[", "{"),
        220 => ("\\", "|"),
        221 => ("]", "}"),
        222 => ("'", "\""),
        _ => return None,
    })
}

fn char_from_code(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

/// xterm's legacy (non-kitty) key encoder: `evaluateKeyboardEvent`.
pub fn evaluate_keyboard_event(
    ev: &KeyboardEvent,
    application_cursor_mode: bool,
    is_mac: bool,
    mac_option_is_meta: bool,
) -> KeyboardResult {
    let mut result = KeyboardResult::default();
    let modifiers = u32::from(ev.shift)
        | (u32::from(ev.alt) << 1)
        | (u32::from(ev.ctrl) << 2)
        | (u32::from(ev.meta) << 3);
    let csi_mod = |final_: &str| format!("{ESC}[1;{}{final_}", modifiers + 1);
    let arrow = |result: &mut KeyboardResult, letter: &str| {
        if ev.meta {
            return;
        }
        result.key = Some(if modifiers != 0 {
            csi_mod(letter)
        } else if application_cursor_mode {
            format!("{ESC}O{letter}")
        } else {
            format!("{ESC}[{letter}")
        });
    };
    let tilde = |number: u32| {
        if modifiers != 0 {
            format!("{ESC}[{number};{}~", modifiers + 1)
        } else {
            format!("{ESC}[{number}~")
        }
    };
    match ev.key_code {
        0 => {
            let letter = match ev.key.as_str() {
                "UIKeyInputUpArrow" => Some("A"),
                "UIKeyInputLeftArrow" => Some("D"),
                "UIKeyInputRightArrow" => Some("C"),
                "UIKeyInputDownArrow" => Some("B"),
                _ => None,
            };
            if let Some(letter) = letter {
                result.key = Some(if application_cursor_mode {
                    format!("{ESC}O{letter}")
                } else {
                    format!("{ESC}[{letter}")
                });
            }
        }
        8 => {
            // backspace: ^H or ^?
            let base = if ev.ctrl { "\x08" } else { "\x7f" };
            result.key = Some(if ev.alt {
                format!("{ESC}{base}")
            } else {
                base.into()
            });
        }
        9 => {
            if ev.shift {
                result.key = Some(format!("{ESC}[Z"));
            } else {
                result.key = Some("\t".into());
                result.cancel = true;
            }
        }
        13 => {
            if ev.key == "c" && ev.ctrl {
                // Safari on iPad sends key 13 when typing ctrl-c on a hardware keyboard.
                result.key = Some("\x03".into());
            } else {
                result.key = Some(if ev.alt {
                    format!("{ESC}\r")
                } else {
                    "\r".into()
                });
            }
            result.cancel = true;
        }
        27 => {
            result.key = Some(if ev.alt {
                format!("{ESC}{ESC}")
            } else {
                ESC.into()
            });
            result.cancel = true;
        }
        37 => arrow(&mut result, "D"),
        39 => arrow(&mut result, "C"),
        38 => arrow(&mut result, "A"),
        40 => arrow(&mut result, "B"),
        45 => {
            // <Ctrl> or <Shift> + <Insert> copy and paste on some systems.
            if !ev.shift && !ev.ctrl {
                result.key = Some(format!("{ESC}[2~"));
            }
        }
        46 => result.key = Some(tilde(3)),
        36 => {
            result.key = Some(if modifiers != 0 {
                csi_mod("H")
            } else if application_cursor_mode {
                format!("{ESC}OH")
            } else {
                format!("{ESC}[H")
            });
        }
        35 => {
            result.key = Some(if modifiers != 0 {
                csi_mod("F")
            } else if application_cursor_mode {
                format!("{ESC}OF")
            } else {
                format!("{ESC}[F")
            });
        }
        33 => {
            if ev.shift {
                result.kind = KeyboardResultType::PageUp;
            } else if ev.ctrl {
                result.key = Some(format!("{ESC}[5;{}~", modifiers + 1));
            } else {
                result.key = Some(format!("{ESC}[5~"));
            }
        }
        34 => {
            if ev.shift {
                result.kind = KeyboardResultType::PageDown;
            } else if ev.ctrl {
                result.key = Some(format!("{ESC}[6;{}~", modifiers + 1));
            } else {
                result.key = Some(format!("{ESC}[6~"));
            }
        }
        112..=115 => {
            let letter = ["P", "Q", "R", "S"][(ev.key_code - 112) as usize];
            result.key = Some(if modifiers != 0 {
                csi_mod(letter)
            } else {
                format!("{ESC}O{letter}")
            });
        }
        116..=123 => {
            let number = [15, 17, 18, 19, 20, 21, 23, 24][(ev.key_code - 116) as usize];
            result.key = Some(tilde(number));
        }
        key_code => {
            if ev.ctrl && !ev.shift && !ev.alt && !ev.meta {
                // a-z and space
                if (65..=90).contains(&key_code) {
                    result.key = Some(char_from_code(key_code - 64));
                } else if key_code == 32 {
                    result.key = Some("\0".into());
                } else if (51..=55).contains(&key_code) {
                    // escape, file sep, group sep, record sep, unit sep
                    result.key = Some(char_from_code(key_code - 51 + 27));
                } else if key_code == 56 {
                    result.key = Some("\x7f".into());
                } else if ev.key == "/" {
                    result.key = Some("\x1f".into());
                } else if key_code == 219 {
                    result.key = Some(ESC.into());
                } else if key_code == 220 {
                    result.key = Some("\x1c".into());
                } else if key_code == 221 {
                    result.key = Some("\x1d".into());
                }
            } else if (!is_mac || mac_option_is_meta) && ev.alt && !ev.meta {
                // On macOS this is a third level shift when !macOptionIsMeta. Use <Esc> instead.
                if let Some((plain, shifted)) = keycode_key_mapping(key_code) {
                    result.key = Some(format!("{ESC}{}", if ev.shift { shifted } else { plain }));
                } else if (65..=90).contains(&key_code) {
                    let code = if ev.ctrl {
                        key_code - 64
                    } else {
                        key_code + 32
                    };
                    let mut key = char_from_code(code);
                    if ev.shift {
                        key = key.to_uppercase();
                    }
                    result.key = Some(format!("{ESC}{key}"));
                } else if key_code == 32 {
                    result.key = Some(format!("{ESC}{}", if ev.ctrl { "\0" } else { " " }));
                } else if ev.key == "Dead" && ev.code.starts_with("Key") {
                    // Alt produces a dead key with some letters in the US layout (N/E/U).
                    let mut key: String = ev.code.chars().skip(3).take(1).collect();
                    if !ev.shift {
                        key = key.to_lowercase();
                    }
                    result.key = Some(format!("{ESC}{key}"));
                    result.cancel = true;
                }
            } else if is_mac && !ev.alt && !ev.ctrl && !ev.shift && ev.meta {
                if key_code == 65 {
                    // cmd + a
                    result.kind = KeyboardResultType::SelectAll;
                }
            } else if !ev.key.is_empty()
                && !ev.ctrl
                && !ev.alt
                && !ev.meta
                && key_code >= 48
                && ev.key.encode_utf16().count() == 1
            {
                // Only keys that result in a single character; not num lock, volume up, etc.
                result.key = Some(ev.key.clone());
            } else if !ev.key.is_empty() && ev.ctrl && ev.shift {
                result.key = match ev.code.as_str() {
                    "Minus" => Some("\x1f".into()),  // ^_ (Ctrl+Shift+-)
                    "Digit2" => Some("\0".into()),   // ^@ (Ctrl+Shift+2)
                    "Digit6" => Some("\x1e".into()), // ^^ (Ctrl+Shift+6)
                    _ => None,
                };
            }
        }
    }
    result
}

/// Kitty keyboard protocol enhancement flags.
pub mod kitty_flags {
    pub const DISAMBIGUATE_ESCAPE_CODES: u16 = 0b00001;
    pub const REPORT_EVENT_TYPES: u16 = 0b00010;
    pub const REPORT_ALTERNATE_KEYS: u16 = 0b00100;
    pub const REPORT_ALL_KEYS_AS_ESCAPE_CODES: u16 = 0b01000;
    pub const REPORT_ASSOCIATED_TEXT: u16 = 0b10000;
}

/// Kitty keyboard event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyEventType {
    Press = 1,
    Repeat = 2,
    Release = 3,
}

fn kitty_functional_key_code(key: &str) -> Option<u32> {
    Some(match key {
        "Escape" => 27,
        "Enter" => 13,
        "Tab" => 9,
        "Backspace" => 127,
        "CapsLock" => 57358,
        "ScrollLock" => 57359,
        "NumLock" => 57360,
        "PrintScreen" => 57361,
        "Pause" => 57362,
        "ContextMenu" => 57363,
        // F13-F35 (F1-F12 use legacy encoding)
        "F13" => 57376,
        "F14" => 57377,
        "F15" => 57378,
        "F16" => 57379,
        "F17" => 57380,
        "F18" => 57381,
        "F19" => 57382,
        "F20" => 57383,
        "F21" => 57384,
        "F22" => 57385,
        "F23" => 57386,
        "F24" => 57387,
        "F25" => 57388,
        "KP_0" => 57399,
        "KP_1" => 57400,
        "KP_2" => 57401,
        "KP_3" => 57402,
        "KP_4" => 57403,
        "KP_5" => 57404,
        "KP_6" => 57405,
        "KP_7" => 57406,
        "KP_8" => 57407,
        "KP_9" => 57408,
        "KP_Decimal" => 57409,
        "KP_Divide" => 57410,
        "KP_Multiply" => 57411,
        "KP_Subtract" => 57412,
        "KP_Add" => 57413,
        "KP_Enter" => 57414,
        "KP_Equal" => 57415,
        "ShiftLeft" => 57441,
        "ShiftRight" => 57447,
        "ControlLeft" => 57442,
        "ControlRight" => 57448,
        "AltLeft" => 57443,
        "AltRight" => 57449,
        "MetaLeft" => 57444,
        "MetaRight" => 57450,
        "MediaPlayPause" => 57430,
        "MediaStop" => 57432,
        "MediaTrackNext" => 57435,
        "MediaTrackPrevious" => 57436,
        "AudioVolumeDown" => 57438,
        "AudioVolumeUp" => 57439,
        "AudioVolumeMute" => 57440,
        _ => return None,
    })
}

fn kitty_csi_tilde_key(key: &str) -> Option<u32> {
    Some(match key {
        "Insert" => 2,
        "Delete" => 3,
        "PageUp" => 5,
        "PageDown" => 6,
        "F5" => 15,
        "F6" => 17,
        "F7" => 18,
        "F8" => 19,
        "F9" => 20,
        "F10" => 21,
        "F11" => 23,
        "F12" => 24,
        _ => return None,
    })
}

fn kitty_csi_letter_key(key: &str) -> Option<char> {
    Some(match key {
        "ArrowUp" => 'A',
        "ArrowDown" => 'B',
        "ArrowRight" => 'C',
        "ArrowLeft" => 'D',
        "Home" => 'H',
        "End" => 'F',
        _ => return None,
    })
}

fn kitty_ss3_function_key(key: &str) -> Option<char> {
    Some(match key {
        "F1" => 'P',
        "F2" => 'Q',
        "F3" => 'R',
        "F4" => 'S',
        _ => return None,
    })
}

fn kitty_numpad_key_code(ev: &KeyboardEvent) -> Option<u32> {
    let suffix = ev.code.strip_prefix("Numpad")?;
    if suffix.len() == 1
        && let Some(digit) = suffix.chars().next().and_then(|c| c.to_digit(10))
    {
        return Some(57399 + digit);
    }
    Some(match suffix {
        "Decimal" => 57409,
        "Divide" => 57410,
        "Multiply" => 57411,
        "Subtract" => 57412,
        "Add" => 57413,
        "Enter" => 57414,
        "Equal" => 57415,
        _ => return None,
    })
}

fn kitty_modifier_key_code(ev: &KeyboardEvent) -> Option<u32> {
    Some(match ev.code.as_str() {
        "ShiftLeft" => 57441,
        "ShiftRight" => 57447,
        "ControlLeft" => 57442,
        "ControlRight" => 57448,
        "AltLeft" => 57443,
        "AltRight" => 57449,
        "MetaLeft" => 57444,
        "MetaRight" => 57450,
        _ => return None,
    })
}

fn single_utf16(key: &str) -> bool {
    key.encode_utf16().count() == 1
}

/// The kitty keyboard protocol encoder (`KittyKeyboard.evaluate`).
pub fn evaluate_kitty_keyboard_event(
    ev: &KeyboardEvent,
    flags: u16,
    event_type: KittyEventType,
    mac_option_as_alt: bool,
) -> KeyboardResult {
    use kitty_flags::*;
    let mut result = KeyboardResult::default();

    let mut mods = 0u32;
    if ev.shift {
        mods |= 1;
    }
    if ev.alt {
        mods |= 2;
    }
    if ev.ctrl {
        mods |= 4;
    }
    if ev.meta {
        mods |= 8;
    }
    let modifiers = if mods > 0 { mods + 1 } else { 0 };
    let is_mod = matches!(ev.key.as_str(), "Shift" | "Control" | "Alt" | "Meta");
    let report_event_types = flags & REPORT_EVENT_TYPES != 0;

    if !report_event_types && event_type == KittyEventType::Release {
        return result;
    }
    if is_mod && flags & REPORT_ALL_KEYS_AS_ESCAPE_CODES == 0 {
        return result;
    }
    // Kitty treats the lock keys as modifier keys for suppressing press events.
    let is_lock = matches!(ev.key.as_str(), "CapsLock" | "NumLock" | "ScrollLock");
    if is_lock && flags & REPORT_ALL_KEYS_AS_ESCAPE_CODES == 0 {
        return result;
    }

    let needs_event_type = report_event_types && event_type != KittyEventType::Press;
    let mod_param = |modifiers: u32| {
        if modifiers > 0 {
            modifiers.to_string()
        } else {
            "1".into()
        }
    };

    if let Some(letter) = kitty_csi_letter_key(&ev.key) {
        let mut seq;
        if modifiers > 0 || needs_event_type {
            seq = format!("{ESC}[1;{}", mod_param(modifiers));
            if needs_event_type {
                seq += &format!(":{}", event_type as u32);
            }
            seq.push(letter);
        } else {
            seq = format!("{ESC}[{letter}");
        }
        result.key = Some(seq);
        result.cancel = true;
        return result;
    }

    if let Some(letter) = kitty_ss3_function_key(&ev.key) {
        let mut seq;
        if modifiers > 0 || needs_event_type {
            seq = format!("{ESC}[1;{}", mod_param(modifiers));
            if needs_event_type {
                seq += &format!(":{}", event_type as u32);
            }
            seq.push(letter);
        } else {
            seq = format!("{ESC}O{letter}");
        }
        result.key = Some(seq);
        result.cancel = true;
        return result;
    }

    if let Some(number) = kitty_csi_tilde_key(&ev.key) {
        let mut seq = format!("{ESC}[{number}");
        if modifiers > 0 || needs_event_type {
            seq += &format!(";{}", mod_param(modifiers));
            if needs_event_type {
                seq += &format!(":{}", event_type as u32);
            }
        }
        seq.push('~');
        result.key = Some(seq);
        result.cancel = true;
        return result;
    }

    let key_code = kitty_key_code(ev, mac_option_as_alt);
    let Some(key_code) = key_code else {
        return result;
    };

    // Enter/Tab/Backspace.
    let special_key = matches!(key_code, 13 | 9 | 127);
    // Enter/Tab/Backspace have no release events unless all keys are reported as escape codes.
    if special_key
        && event_type == KittyEventType::Release
        && flags & REPORT_ALL_KEYS_AS_ESCAPE_CODES == 0
    {
        return result;
    }

    let is_func =
        kitty_functional_key_code(&ev.key).is_some() || kitty_numpad_key_code(ev).is_some();

    let use_csi_u = flags & REPORT_ALL_KEYS_AS_ESCAPE_CODES != 0
        || (report_event_types && event_type == KittyEventType::Release)
        // REPORT_EVENT_TYPES without DISAMBIGUATE_ESCAPE_CODES makes little sense, so it implies it.
        || ((flags & DISAMBIGUATE_ESCAPE_CODES != 0 || report_event_types)
            && ((is_func && !special_key)
                || (modifiers > 0 && !single_utf16(&ev.key))
                || modifiers.saturating_sub(1) > 1));

    if use_csi_u {
        result.key = Some(kitty_csi_u_sequence(
            ev, key_code, modifiers, event_type, flags, is_func, is_mod,
        ));
        result.cancel = true;
    } else {
        let legacy = match key_code {
            13 => Some("\r"),
            9 => Some("\t"),
            127 => Some("\x7f"),
            _ => None,
        };
        if let Some(byte) = legacy {
            result.key = Some(byte.into());
        } else if single_utf16(&ev.key) && !ev.ctrl && !ev.alt && !ev.meta {
            result.key = Some(ev.key.clone());
        }
    }
    result
}

fn kitty_key_code(ev: &KeyboardEvent, mac_option_as_alt: bool) -> Option<u32> {
    if let Some(code) = kitty_numpad_key_code(ev) {
        return Some(code);
    }
    if let Some(code) = kitty_modifier_key_code(ev) {
        return Some(code);
    }
    if let Some(code) = kitty_functional_key_code(&ev.key) {
        return Some(code);
    }
    if (ev.shift || (mac_option_as_alt && ev.alt)) && !ev.code.is_empty() {
        if let Some(digit) = ev.code.strip_prefix("Digit")
            && digit.len() == 1
            && let Some(c) = digit.chars().next().filter(char::is_ascii_digit)
        {
            return Some(c as u32);
        }
        if let Some(letter) = ev.code.strip_prefix("Key")
            && letter.len() == 1
        {
            return letter.chars().next().map(|c| c.to_ascii_lowercase() as u32);
        }
    }
    if single_utf16(&ev.key) {
        let code = ev.key.chars().next()? as u32;
        return Some(if (65..=90).contains(&code) {
            code + 32
        } else {
            code
        });
    }
    None
}

fn kitty_csi_u_sequence(
    ev: &KeyboardEvent,
    key_code: u32,
    modifiers: u32,
    event_type: KittyEventType,
    flags: u16,
    is_func: bool,
    is_mod: bool,
) -> String {
    use kitty_flags::*;
    let report_event_types = flags & REPORT_EVENT_TYPES != 0;
    let report_alternate_keys = flags & REPORT_ALTERNATE_KEYS != 0;
    let mut seq = format!("{ESC}[{key_code}");

    if report_alternate_keys
        && ev.shift
        && single_utf16(&ev.key)
        && !is_func
        && !is_mod
        && let Some(shifted) = ev.key.chars().next()
    {
        seq += &format!(":{}", shifted as u32);
    }

    let report_associated_text = flags & REPORT_ASSOCIATED_TEXT != 0
        && event_type != KittyEventType::Release
        && single_utf16(&ev.key)
        && !is_func
        && !is_mod
        && !ev.ctrl;
    let text_code = if report_associated_text {
        ev.key.chars().next().map(|c| c as u32)
    } else {
        None
    };

    let needs_event_type = report_event_types
        && event_type != KittyEventType::Press
        && (event_type == KittyEventType::Release || text_code.is_none());

    if modifiers > 0 || needs_event_type || text_code.is_some() {
        seq.push(';');
        if modifiers > 0 {
            seq += &modifiers.to_string();
        } else if needs_event_type {
            seq.push('1');
        }
        if needs_event_type {
            seq += &format!(":{}", event_type as u32);
        }
    }
    if let Some(text) = text_code {
        seq += &format!(";{text}");
    }
    seq.push('u');
    seq
}

const KITTY_KEYPAD_ENTER: &str = "\u{e046}";

/// Work around xterm's Kitty encoder exposing KP_Enter's private-use code point on macOS when no
/// negotiated mode encoded it as a CSI-u sequence.
pub fn normalize_terminal_keyboard_input(
    data: &str,
    event: Option<&KeyboardEvent>,
    is_mac: bool,
) -> String {
    if is_mac
        && data == KITTY_KEYPAD_ENTER
        && let Some(event) = event
        && event.code == "NumpadEnter"
        && !event.alt
        && !event.ctrl
        && !event.meta
        && !event.shift
    {
        return "\r".into();
    }
    data.into()
}

/// The terminal state the encoders read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyEncodeContext {
    /// DECCKM: arrows send `ESC O A` instead of `ESC [ A`.
    pub application_cursor: bool,
    /// Kitty keyboard flags the program negotiated; zero uses the legacy encoder.
    pub kitty_flags: u16,
    pub is_mac: bool,
    /// macOS Option acts as Meta (xterm's `macOptionIsMeta`, off in Muxus).
    pub mac_option_is_meta: bool,
    /// Windows: Ctrl+Alt is AltGr.
    pub is_windows: bool,
}

/// What a key press does in the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Send these bytes to the session.
    Send(String),
    /// Scroll the viewport a page (Shift+PageUp/PageDown).
    ScrollPage { up: bool },
    /// Select the whole buffer (Cmd+A on macOS).
    SelectAll,
    /// The key produces text that arrives through text input (IME, dead keys, plain typing).
    Text,
    /// The terminal does nothing with this key.
    Ignored,
}

fn is_third_level_shift(ev: &KeyboardEvent, ctx: &KeyEncodeContext, keypress: bool) -> bool {
    let third_level = (ctx.is_mac && !ctx.mac_option_is_meta && ev.alt && !ev.ctrl && !ev.meta)
        || (ctx.is_windows && ev.alt && ev.ctrl && !ev.meta);
    if keypress {
        return third_level;
    }
    // Not for arrows, page down, home, backspace, etc.
    third_level && (ev.key_code == 0 || ev.key_code > 47)
}

/// The keydown flow of xterm's `CoreBrowserTerminal`: encode the key, and fall back to the text
/// the key produces (`key_char`, the keypress path) when the encoder has nothing for it.
///
/// Plain printable keys return [`KeyAction::Text`] when `text_input` is true, so the toolkit's
/// text input (and with it IME composition and dead keys) delivers them; the bytes are the same.
pub fn key_down(
    ev: &KeyboardEvent,
    key_char: Option<&str>,
    ctx: &KeyEncodeContext,
    text_input: bool,
) -> KeyAction {
    let result = if ctx.kitty_flags > 0 {
        let event_type = if ev.repeat {
            KittyEventType::Repeat
        } else {
            KittyEventType::Press
        };
        evaluate_kitty_keyboard_event(
            ev,
            ctx.kitty_flags,
            event_type,
            ctx.is_mac && ctx.mac_option_is_meta,
        )
    } else {
        evaluate_keyboard_event(
            ev,
            ctx.application_cursor,
            ctx.is_mac,
            ctx.mac_option_is_meta,
        )
    };
    match result.kind {
        KeyboardResultType::PageUp => return KeyAction::ScrollPage { up: true },
        KeyboardResultType::PageDown => return KeyAction::ScrollPage { up: false },
        KeyboardResultType::SelectAll => return KeyAction::SelectAll,
        KeyboardResultType::SendKey => {}
    }
    let keypress = |ev: &KeyboardEvent| -> KeyAction {
        let Some(text) = key_char.filter(|text| !text.is_empty()) else {
            return KeyAction::Ignored;
        };
        if (ev.alt || ev.ctrl || ev.meta) && !is_third_level_shift(ev, ctx, true) {
            return KeyAction::Ignored;
        }
        if text.chars().any(char::is_control) {
            return KeyAction::Ignored;
        }
        if text_input {
            KeyAction::Text
        } else {
            KeyAction::Send(text.into())
        }
    };
    if is_third_level_shift(ev, ctx, false) {
        return keypress(ev);
    }
    let Some(key) = result.key.filter(|key| !key.is_empty()) else {
        return keypress(ev);
    };
    let plain_text =
        !ev.ctrl && !ev.alt && !ev.meta && key_char == Some(key.as_str()) && ctx.kitty_flags == 0;
    if plain_text && text_input {
        return KeyAction::Text;
    }
    KeyAction::Send(normalize_terminal_keyboard_input(
        &key,
        Some(ev),
        ctx.is_mac,
    ))
}

/// Legacy `keyCode` of the US-layout key that types `c` (letters, digits and punctuation).
fn key_code_for_char(c: char) -> u32 {
    match c {
        'a'..='z' => c.to_ascii_uppercase() as u32,
        'A'..='Z' => c as u32,
        '0'..='9' => c as u32,
        ')' => 48,
        '!' => 49,
        '@' => 50,
        '#' => 51,
        '$' => 52,
        '%' => 53,
        '^' => 54,
        '&' => 55,
        '*' => 56,
        '(' => 57,
        ' ' => 32,
        ';' | ':' => 186,
        '=' | '+' => 187,
        ',' | '<' => 188,
        '-' | '_' => 189,
        '.' | '>' => 190,
        '/' | '?' => 191,
        '`' | '~' => 192,
        '[' | '{' => 219,
        '\\' | '|' => 220,
        ']' | '}' => 221,
        '\'' | '"' => 222,
        // Other printable characters (non-US layouts) get a key code no rule above claims, so
        // they type themselves instead of turning into a letter chord.
        _ if !c.is_control() => 160,
        _ => 0,
    }
}

/// DOM `code` of the US-layout key that types `c`.
fn code_for_char(c: char) -> String {
    match c {
        'a'..='z' | 'A'..='Z' => format!("Key{}", c.to_ascii_uppercase()),
        '0'..='9' => format!("Digit{c}"),
        ')' => "Digit0".into(),
        '!' => "Digit1".into(),
        '@' => "Digit2".into(),
        '#' => "Digit3".into(),
        '$' => "Digit4".into(),
        '%' => "Digit5".into(),
        '^' => "Digit6".into(),
        '&' => "Digit7".into(),
        '*' => "Digit8".into(),
        '(' => "Digit9".into(),
        ' ' => "Space".into(),
        ';' | ':' => "Semicolon".into(),
        '=' | '+' => "Equal".into(),
        ',' | '<' => "Comma".into(),
        '-' | '_' => "Minus".into(),
        '.' | '>' => "Period".into(),
        '/' | '?' => "Slash".into(),
        '`' | '~' => "Backquote".into(),
        '[' | '{' => "BracketLeft".into(),
        '\\' | '|' => "Backslash".into(),
        ']' | '}' => "BracketRight".into(),
        '\'' | '"' => "Quote".into(),
        _ => String::new(),
    }
}

impl KeyboardEvent {
    /// Build an event from a toolkit key: `key` is the key's name in lower case ("a", "enter",
    /// "up", "pageup", "f5", "kp_enter", "-"…), `key_char` the text it types with the current
    /// modifiers, if any.
    pub fn from_key(
        key: &str,
        key_char: Option<&str>,
        shift: bool,
        alt: bool,
        ctrl: bool,
        meta: bool,
    ) -> Self {
        let named: Option<(&str, &str, u32)> = match key {
            "enter" | "return" => Some(("Enter", "Enter", 13)),
            "escape" => Some(("Escape", "Escape", 27)),
            "backspace" => Some(("Backspace", "Backspace", 8)),
            "tab" => Some(("Tab", "Tab", 9)),
            "up" => Some(("ArrowUp", "ArrowUp", 38)),
            "down" => Some(("ArrowDown", "ArrowDown", 40)),
            "left" => Some(("ArrowLeft", "ArrowLeft", 37)),
            "right" => Some(("ArrowRight", "ArrowRight", 39)),
            "home" => Some(("Home", "Home", 36)),
            "end" => Some(("End", "End", 35)),
            "pageup" => Some(("PageUp", "PageUp", 33)),
            "pagedown" => Some(("PageDown", "PageDown", 34)),
            "insert" => Some(("Insert", "Insert", 45)),
            "delete" => Some(("Delete", "Delete", 46)),
            "shift" => Some(("Shift", "ShiftLeft", 16)),
            "control" | "ctrl" => Some(("Control", "ControlLeft", 17)),
            "alt" => Some(("Alt", "AltLeft", 18)),
            "platform" | "meta" | "super" | "cmd" => Some(("Meta", "MetaLeft", 91)),
            "capslock" => Some(("CapsLock", "CapsLock", 20)),
            "menu" => Some(("ContextMenu", "ContextMenu", 93)),
            "kp_enter" | "numpadenter" => Some(("Enter", "NumpadEnter", 13)),
            _ => None,
        };
        let mut ev = KeyboardEvent {
            shift,
            alt,
            ctrl,
            meta,
            ..Default::default()
        };
        if let Some((dom_key, code, key_code)) = named {
            ev.key = dom_key.into();
            ev.code = code.into();
            ev.key_code = key_code;
            return ev;
        }
        if let Some(number) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
            && (1..=24).contains(&number)
        {
            ev.key = format!("F{number}");
            ev.code = ev.key.clone();
            // F1 is 112; the legacy codes continue past F12.
            ev.key_code = 111 + number;
            return ev;
        }
        if key == "space" {
            ev.key = " ".into();
            ev.code = "Space".into();
            ev.key_code = 32;
            return ev;
        }
        let base = key.chars().next().filter(|_| key.chars().count() == 1);
        // The DOM key is the produced character; Ctrl and Alt chords report the shifted base key.
        let produced =
            key_char.filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
        ev.key = match (produced, base) {
            (Some(text), _) if !ctrl => text.into(),
            (_, Some(c)) if shift => c.to_uppercase().collect(),
            (_, Some(c)) => c.to_string(),
            (Some(text), None) => text.into(),
            (None, None) => key.into(),
        };
        if let Some(c) = base.or_else(|| ev.key.chars().next()) {
            ev.code = code_for_char(c);
            ev.key_code = key_code_for_char(c);
        }
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(code: &str, shift: bool) -> KeyboardEvent {
        KeyboardEvent {
            code: code.into(),
            shift,
            ..Default::default()
        }
    }

    #[test]
    fn turns_the_macos_kp_enter_private_use_glyph_into_carriage_return() {
        assert_eq!(
            normalize_terminal_keyboard_input(
                "\u{e046}",
                Some(&key_event("NumpadEnter", false)),
                true
            ),
            "\r"
        );
    }

    #[test]
    fn does_not_change_the_ordinary_return_key() {
        assert_eq!(
            normalize_terminal_keyboard_input("\r", Some(&key_event("Enter", false)), true),
            "\r"
        );
    }

    #[test]
    fn preserves_negotiated_kitty_keypad_reporting() {
        let kitty_keypad_enter = "\x1b[57414u";
        assert_eq!(
            normalize_terminal_keyboard_input(
                kitty_keypad_enter,
                Some(&key_event("NumpadEnter", false)),
                true
            ),
            kitty_keypad_enter
        );
    }

    #[test]
    fn leaves_modified_non_macos_and_non_keyboard_input_untouched() {
        assert_eq!(
            normalize_terminal_keyboard_input(
                "\u{e046}",
                Some(&key_event("NumpadEnter", true)),
                true
            ),
            "\u{e046}"
        );
        assert_eq!(
            normalize_terminal_keyboard_input(
                "\u{e046}",
                Some(&key_event("NumpadEnter", false)),
                false
            ),
            "\u{e046}"
        );
        assert_eq!(
            normalize_terminal_keyboard_input("\u{e046}", None, true),
            "\u{e046}"
        );
    }

    fn send(
        key: &str,
        key_char: Option<&str>,
        mods: (bool, bool, bool, bool),
        ctx: KeyEncodeContext,
    ) -> KeyAction {
        let ev = KeyboardEvent::from_key(key, key_char, mods.0, mods.1, mods.2, mods.3);
        key_down(&ev, key_char, &ctx, false)
    }

    const NONE: (bool, bool, bool, bool) = (false, false, false, false);
    const SHIFT: (bool, bool, bool, bool) = (true, false, false, false);
    const ALT: (bool, bool, bool, bool) = (false, true, false, false);
    const CTRL: (bool, bool, bool, bool) = (false, false, true, false);
    const CTRL_SHIFT: (bool, bool, bool, bool) = (true, false, true, false);

    fn bytes(action: KeyAction) -> String {
        match action {
            KeyAction::Send(data) => data,
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn encodes_legacy_keys_like_xterm() {
        let ctx = KeyEncodeContext::default();
        assert_eq!(bytes(send("enter", None, NONE, ctx)), "\r");
        assert_eq!(bytes(send("enter", None, ALT, ctx)), "\x1b\r");
        assert_eq!(bytes(send("backspace", None, NONE, ctx)), "\x7f");
        assert_eq!(bytes(send("backspace", None, CTRL, ctx)), "\x08");
        assert_eq!(bytes(send("backspace", None, ALT, ctx)), "\x1b\x7f");
        assert_eq!(bytes(send("tab", None, SHIFT, ctx)), "\x1b[Z");
        assert_eq!(bytes(send("escape", None, NONE, ctx)), "\x1b");
        assert_eq!(bytes(send("up", None, NONE, ctx)), "\x1b[A");
        assert_eq!(bytes(send("left", None, CTRL, ctx)), "\x1b[1;5D");
        assert_eq!(bytes(send("right", None, SHIFT, ctx)), "\x1b[1;2C");
        assert_eq!(bytes(send("home", None, NONE, ctx)), "\x1b[H");
        assert_eq!(bytes(send("end", None, NONE, ctx)), "\x1b[F");
        assert_eq!(bytes(send("delete", None, NONE, ctx)), "\x1b[3~");
        assert_eq!(bytes(send("delete", None, CTRL, ctx)), "\x1b[3;5~");
        assert_eq!(bytes(send("insert", None, NONE, ctx)), "\x1b[2~");
        assert_eq!(send("insert", None, SHIFT, ctx), KeyAction::Ignored);
        assert_eq!(bytes(send("pageup", None, NONE, ctx)), "\x1b[5~");
        assert_eq!(bytes(send("pagedown", None, CTRL, ctx)), "\x1b[6;5~");
        assert_eq!(
            send("pageup", None, SHIFT, ctx),
            KeyAction::ScrollPage { up: true }
        );
        assert_eq!(bytes(send("f1", None, NONE, ctx)), "\x1bOP");
        assert_eq!(bytes(send("f4", None, SHIFT, ctx)), "\x1b[1;2S");
        assert_eq!(bytes(send("f5", None, NONE, ctx)), "\x1b[15~");
        assert_eq!(bytes(send("f12", None, CTRL, ctx)), "\x1b[24;5~");
        let app = KeyEncodeContext {
            application_cursor: true,
            ..ctx
        };
        assert_eq!(bytes(send("up", None, NONE, app)), "\x1bOA");
        assert_eq!(bytes(send("home", None, NONE, app)), "\x1bOH");
    }

    #[test]
    fn encodes_control_and_alt_chords() {
        let ctx = KeyEncodeContext::default();
        assert_eq!(bytes(send("c", None, CTRL, ctx)), "\x03");
        assert_eq!(bytes(send("space", None, CTRL, ctx)), "\0");
        assert_eq!(bytes(send("[", None, CTRL, ctx)), "\x1b");
        assert_eq!(bytes(send("\\", None, CTRL, ctx)), "\x1c");
        assert_eq!(bytes(send("]", None, CTRL, ctx)), "\x1d");
        assert_eq!(bytes(send("/", None, CTRL, ctx)), "\x1f");
        assert_eq!(bytes(send("3", None, CTRL, ctx)), "\x1b");
        assert_eq!(bytes(send("8", None, CTRL, ctx)), "\x7f");
        assert_eq!(bytes(send("-", None, CTRL_SHIFT, ctx)), "\x1f");
        assert_eq!(bytes(send("2", None, CTRL_SHIFT, ctx)), "\0");
        assert_eq!(bytes(send("6", None, CTRL_SHIFT, ctx)), "\x1e");
        assert_eq!(bytes(send("b", Some("b"), ALT, ctx)), "\x1bb");
        assert_eq!(
            bytes(send("b", Some("B"), (true, true, false, false), ctx)),
            "\x1bB"
        );
        assert_eq!(bytes(send(".", Some("."), ALT, ctx)), "\x1b.");
        assert_eq!(
            bytes(send("1", Some("!"), (true, true, false, false), ctx)),
            "\x1b!"
        );
    }

    #[test]
    fn types_printable_keys() {
        let ctx = KeyEncodeContext::default();
        assert_eq!(bytes(send("a", Some("a"), NONE, ctx)), "a");
        assert_eq!(bytes(send("a", Some("A"), SHIFT, ctx)), "A");
        assert_eq!(bytes(send("1", Some("!"), SHIFT, ctx)), "!");
        assert_eq!(bytes(send("e", Some("é"), NONE, ctx)), "é");
        let ev = KeyboardEvent::from_key("a", Some("a"), false, false, false, false);
        assert_eq!(key_down(&ev, Some("a"), &ctx, true), KeyAction::Text);
    }

    #[test]
    fn leaves_option_composed_characters_to_text_input_on_macos() {
        let ctx = KeyEncodeContext {
            is_mac: true,
            ..Default::default()
        };
        assert_eq!(bytes(send("f", Some("ƒ"), ALT, ctx)), "ƒ");
        assert_eq!(
            send("a", None, (false, false, false, true), ctx),
            KeyAction::SelectAll
        );
    }

    #[test]
    fn encodes_the_kitty_keyboard_protocol() {
        let disambiguate = KeyEncodeContext {
            kitty_flags: kitty_flags::DISAMBIGUATE_ESCAPE_CODES,
            ..Default::default()
        };
        assert_eq!(bytes(send("a", Some("a"), NONE, disambiguate)), "a");
        assert_eq!(bytes(send("c", None, CTRL, disambiguate)), "\x1b[99;5u");
        assert_eq!(bytes(send("escape", None, NONE, disambiguate)), "\x1b[27u");
        assert_eq!(bytes(send("enter", None, NONE, disambiguate)), "\r");
        assert_eq!(
            bytes(send("enter", None, SHIFT, disambiguate)),
            "\x1b[13;2u"
        );
        assert_eq!(bytes(send("up", None, NONE, disambiguate)), "\x1b[A");
        assert_eq!(bytes(send("up", None, CTRL, disambiguate)), "\x1b[1;5A");
        assert_eq!(bytes(send("f1", None, NONE, disambiguate)), "\x1bOP");
        assert_eq!(bytes(send("delete", None, NONE, disambiguate)), "\x1b[3~");
        assert_eq!(bytes(send("a", Some("A"), SHIFT, disambiguate)), "A");

        let all = KeyEncodeContext {
            kitty_flags: 0b11111,
            ..Default::default()
        };
        assert_eq!(bytes(send("a", Some("a"), NONE, all)), "\x1b[97;;97u");
        assert_eq!(bytes(send("a", Some("A"), SHIFT, all)), "\x1b[97:65;2;65u");
        assert_eq!(bytes(send("kp_enter", None, NONE, all)), "\x1b[57414u");
    }

    #[test]
    fn reports_kitty_key_releases_and_repeats() {
        let ev = KeyboardEvent::from_key("a", Some("a"), false, false, false, false);
        let flags = kitty_flags::DISAMBIGUATE_ESCAPE_CODES | kitty_flags::REPORT_EVENT_TYPES;
        assert_eq!(
            evaluate_kitty_keyboard_event(&ev, flags, KittyEventType::Release, false)
                .key
                .as_deref(),
            Some("\x1b[97;1:3u")
        );
        let up = KeyboardEvent::from_key("up", None, false, false, false, false);
        assert_eq!(
            evaluate_kitty_keyboard_event(&up, flags, KittyEventType::Repeat, false)
                .key
                .as_deref(),
            Some("\x1b[1;1:2A")
        );
        assert_eq!(
            evaluate_kitty_keyboard_event(
                &up,
                kitty_flags::DISAMBIGUATE_ESCAPE_CODES,
                KittyEventType::Release,
                false
            )
            .key,
            None
        );
    }

    #[test]
    fn maps_toolkit_keys_to_dom_events() {
        let ev = KeyboardEvent::from_key("a", Some("A"), true, false, false, false);
        assert_eq!(
            (ev.key.as_str(), ev.code.as_str(), ev.key_code),
            ("A", "KeyA", 65)
        );
        let ev = KeyboardEvent::from_key("f5", None, false, false, false, false);
        assert_eq!((ev.key.as_str(), ev.key_code), ("F5", 116));
        let ev = KeyboardEvent::from_key(";", Some(":"), true, false, false, false);
        assert_eq!(
            (ev.key.as_str(), ev.code.as_str(), ev.key_code),
            (":", "Semicolon", 186)
        );
        let ev = KeyboardEvent::from_key("c", None, false, false, true, false);
        assert_eq!((ev.key.as_str(), ev.key_code), ("c", 67));
    }
}
