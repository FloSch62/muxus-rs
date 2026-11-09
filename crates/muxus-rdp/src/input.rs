//! Keyboard, pointer and wheel input, translated the way the TypeScript client translates DOM
//! events (physical key codes → scancodes, view pixels → desktop pixels).
//!
//! Port of the input half of `client/src/remote-desktop/rdp-client.ts`. The output is a list of
//! [`InputOp`]s, the same device events the TypeScript code hands to IronRDP's input
//! transactions; the session applies them through IronRDP's input database.
//!
//! Keys that type a character go out as Unicode keyboard events instead, when their physical
//! key is not known for sure (see `typed_char`): what they type then no longer depends on
//! the server's keyboard layout guessing the right physical key.

use std::collections::BTreeSet;

use crate::scancodes::{scancode, scancode_for_code};

/// Modifier and lock state at the time of a key event, as `KeyboardEvent` reports it
/// (`shiftKey`, `ctrlKey`, `altKey`, `metaKey` and `getModifierState(…)`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    /// `getModifierState('AltGraph')`: Chrome on Linux reports AltGr as AltRight with this set
    /// and `alt` false.
    pub alt_graph: bool,
    pub caps_lock: bool,
    /// `None` when the local state is unknown (GPUI does not report it): the server's Num Lock
    /// is then left as this client last set it.
    pub num_lock: Option<bool>,
    pub scroll_lock: bool,
}

/// A key going down or up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyEvent {
    /// The physical key, named like `KeyboardEvent.code` (`KeyA`, `ShiftLeft`, `NumpadEnter`).
    pub code: String,
    /// The produced key value, like `KeyboardEvent.key`. RDP sends a character it holds as a
    /// Unicode event when the key is not a shortcut (see `typed_char`); VNC needs it for
    /// keysyms.
    pub key: String,
    /// True for keydown, false for keyup.
    pub pressed: bool,
    /// Auto-repeat of a held key.
    pub repeat: bool,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    pub fn down(code: &str, modifiers: Modifiers) -> Self {
        Self {
            code: code.to_owned(),
            pressed: true,
            modifiers,
            ..Self::default()
        }
    }

    pub fn up(code: &str, modifiers: Modifiers) -> Self {
        Self {
            code: code.to_owned(),
            pressed: false,
            modifiers,
            ..Self::default()
        }
    }
}

/// Mouse buttons in `MouseEvent.button` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// The "back" side button (X1).
    Back,
    /// The "forward" side button (X2).
    Forward,
}

impl MouseButton {
    /// From `MouseEvent.button` (0 left, 1 middle, 2 right, 3 back, 4 forward).
    pub fn from_dom(button: i16) -> Option<Self> {
        Some(match button {
            0 => Self::Left,
            1 => Self::Middle,
            2 => Self::Right,
            3 => Self::Back,
            4 => Self::Forward,
            _ => return None,
        })
    }
}

/// What a wheel delta counts, like `WheelEvent.deltaMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WheelUnit {
    #[default]
    Pixel,
    Line,
    Page,
}

/// One wheel event, with DOM sign conventions: `delta_y` grows toward the user, `delta_x` to
/// the right.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WheelDelta {
    pub delta_x: f64,
    pub delta_y: f64,
    pub unit: WheelUnit,
}

/// One device event for the server, the unit of an IronRDP input transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOp {
    KeyPressed(u16),
    KeyReleased(u16),
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Wheel rotation in `unit`s; positive is away from the user (vertical) or to the right.
    Wheel {
        vertical: bool,
        amount: i16,
        unit: WheelUnit,
    },
    /// The lock-key state. Windows releases every held key on one ([MS-RDPBCGR]
    /// 2.2.8.1.1.3.1.1.5).
    SynchronizeLocks {
        scroll_lock: bool,
        num_lock: bool,
        caps_lock: bool,
    },
    /// A character typed as a Unicode keyboard event (IronRDP's `UnicodeKeyPressed`).
    UnicodePressed(char),
    /// Auto-repeat of a character held down: another press without a release in between, as
    /// a keyboard repeats a key (IronRDP's input database would release it first).
    UnicodeRepeated(char),
    UnicodeReleased(char),
    /// Let go of every key and button the input database holds.
    ReleaseAll,
    /// The Pause key: the EXTENDED1 sequence (Ctrl 0x1D, then Num Lock 0x45, pressed and
    /// released) a keyboard sends for it, as mstsc and FreeRDP do. It has no key-up of its own.
    Pause,
}

/// Modifier flags on every key event, with the scancodes that produce them.
#[derive(Clone, Copy)]
enum ModifierFlag {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

const SHIFT_CODES: [u16; 2] = [0x2a, 0x36];

const MODIFIERS: [(ModifierFlag, [u16; 2]); 4] = [
    (ModifierFlag::Shift, SHIFT_CODES),
    (ModifierFlag::Ctrl, [0x1d, 0xe01d]),
    (ModifierFlag::Alt, [0x38, 0xe038]),
    (ModifierFlag::Meta, [0xe05b, 0xe05c]),
];

fn is_modifier_code(scancode: u16) -> bool {
    MODIFIERS.iter().any(|(_, codes)| codes.contains(&scancode))
}

fn is_lock_code(code: &str) -> bool {
    matches!(code, "ScrollLock" | "NumLock" | "CapsLock")
}

/// Keys whose meaning depends on the keyboard layout (the DOM's "writing system keys").
fn is_writing_system_code(code: &str) -> bool {
    code.starts_with("Key")
        || code.starts_with("Digit")
        || matches!(
            code,
            "Minus"
                | "Equal"
                | "BracketLeft"
                | "BracketRight"
                | "Backslash"
                | "Semicolon"
                | "Quote"
                | "Backquote"
                | "Comma"
                | "Period"
                | "Slash"
                | "IntlBackslash"
                | "IntlRo"
                | "IntlYen"
        )
}

/// The character a key event types when it goes to the server as a Unicode event rather than
/// a scancode.
///
/// The desktop view knows the character a key typed, but not reliably the physical key: it
/// names the US key that types the same character, which on any other local layout is the
/// wrong key (German `z` arrived as the server's `y`, `/` as `_`, AltGr+Q as `2`), and
/// characters no US key types (`ö`, `ß`, `é`) had no key at all. So a key goes as Unicode
/// when it is a layout-dependent key (letters, digits, punctuation) or has no scancode, its
/// `key` is a single printable character other than space, and it is not a shortcut: no Ctrl,
/// Alt or Meta, unless AltGr (`alt_graph`) made the character. Everything else keeps its
/// scancode: shortcuts (Ctrl+C, Alt+F, Win+R, which applications match by key), named keys
/// (Enter, Tab, Backspace, Escape, arrows, F-keys, Space), the keypad and modifiers.
fn typed_char(event: &KeyEvent) -> Option<char> {
    let mut chars = event.key.chars();
    let (Some(typed), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if typed.is_control() || typed.is_whitespace() {
        return None;
    }
    let flags = event.modifiers;
    if flags.meta || ((flags.ctrl || flags.alt) && !flags.alt_graph) {
        return None;
    }
    let layout_key =
        is_writing_system_code(&event.code) || scancode_for_code(&event.code).is_none();
    layout_key.then_some(typed)
}

/// The keyboard state one RDP session keeps between events.
#[derive(Debug, Default)]
pub struct RdpKeyboard {
    /// Modifiers the server currently sees held down.
    held_modifiers: BTreeSet<u16>,
    /// Lock-key state as last reported to the server (scroll, num, caps).
    lock_state: Option<(bool, bool, bool)>,
    /// Other keys pressed as scancodes and not yet released.
    held_keys: BTreeSet<u16>,
    /// Keys pressed as Unicode characters and not yet released: the key's `code` and the
    /// character it went down with, so its release lets go of that character whatever the
    /// modifiers are by then.
    held_text: Vec<(String, char)>,
}

/// The Num Lock state a synchronize event carries when the local state is unknown and the
/// server has not been told one yet. A synchronize event sets every lock, so some value has to
/// go out; with Num Lock on, the keypad keys this client sends as keypad scancodes (the
/// operators and the decimal key) type what they type locally, where off would turn the
/// decimal key into Delete.
const ASSUMED_NUM_LOCK: bool = true;

impl RdpKeyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The device events for one key event, or `None` for keys RDP has no scancode for, so
    /// the caller may let them be.
    pub fn key(&mut self, event: &KeyEvent) -> Option<Vec<InputOp>> {
        // Pause has a scancode sequence of its own; with Ctrl held the key is Break (0xE046),
        // which the table has.
        if event.code == "Pause" && !event.modifiers.ctrl {
            return Some(self.pause(event));
        }
        if !event.pressed
            && let Some(typed) = self.release_text(event)
        {
            return Some(vec![InputOp::UnicodeReleased(typed)]);
        }
        if let Some(typed) = typed_char(event) {
            return Some(self.text(event, typed));
        }
        let scancode = scancode_for_code(&event.code)?;
        let modifier = is_modifier_code(scancode);
        let mut ops = Vec::new();
        if event.pressed {
            // IronRDP sends a repeat of a held key as release + press, which would toggle a
            // held Caps Lock over and over; modifiers have nothing to repeat.
            if event.repeat && (modifier || is_lock_code(&event.code)) {
                return Some(ops);
            }
            // Pressing Num Lock toggles the server's lock; when the local state is unknown,
            // remember that so the next synchronize event does not undo it.
            if event.code == "NumLock"
                && event.modifiers.num_lock.is_none()
                && let Some(state) = &mut self.lock_state
            {
                state.1 = !state.1;
            }
            // A lock key toggles the server's lock itself; syncing first could undo that.
            if !is_lock_code(&event.code) && self.sync_lock_keys(event, &mut ops) {
                // A synchronize event also lets go of every key on the server
                // ([MS-RDPBCGR] 2.2.8.1.1.3.1.1.5), so held modifiers must go down again.
                self.forget_held();
            }
            if modifier {
                self.held_modifiers.insert(scancode);
            } else {
                self.reconcile_modifiers(event, &mut ops);
                self.held_keys.insert(scancode);
            }
            ops.push(InputOp::KeyPressed(scancode));
        } else {
            if modifier {
                self.held_modifiers.remove(&scancode);
            } else {
                self.held_keys.remove(&scancode);
            }
            ops.push(InputOp::KeyReleased(scancode));
        }
        Some(ops)
    }

    /// The character a held Unicode key went down with, when `event` releases it (the
    /// caller names the released key by the `code` and `key` it went down with).
    fn release_text(&mut self, event: &KeyEvent) -> Option<char> {
        let mut chars = event.key.chars();
        let (Some(typed), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let index = self
            .held_text
            .iter()
            .position(|(code, held)| *code == event.code && *held == typed)?;
        self.held_text.remove(index);
        Some(typed)
    }

    /// A typed character: pressed on the key's press and released on its release (see
    /// `release_text`), so the server sees the key held for as long as it is; auto-repeat
    /// presses it again without a release, as a keyboard does. Ctrl, Alt and Meta held on
    /// the server are let go first: Windows would deliver the character with Alt down as a
    /// menu shortcut. A release that finds no held character sends nothing, unless the key
    /// went down as a scancode (Ctrl+A with Ctrl let go first).
    fn text(&mut self, event: &KeyEvent, typed: char) -> Vec<InputOp> {
        let mut ops = Vec::new();
        if !event.pressed {
            if let Some(scancode) = scancode_for_code(&event.code)
                && self.held_keys.remove(&scancode)
            {
                ops.push(InputOp::KeyReleased(scancode));
            }
            return ops;
        }
        let held = (event.code.clone(), typed);
        if event.repeat && self.held_text.contains(&held) {
            return vec![InputOp::UnicodeRepeated(typed)];
        }
        if self.sync_lock_keys(event, &mut ops) {
            self.forget_held();
        }
        let shortcut_modifiers: Vec<u16> = self
            .held_modifiers
            .iter()
            .copied()
            .filter(|code| !SHIFT_CODES.contains(code))
            .collect();
        for code in shortcut_modifiers {
            self.held_modifiers.remove(&code);
            ops.push(InputOp::KeyReleased(code));
        }
        if !self.held_text.contains(&held) {
            self.held_text.push(held);
        }
        ops.push(InputOp::UnicodePressed(typed));
        ops
    }

    /// After a synchronize event the server holds no keys.
    fn forget_held(&mut self) {
        self.held_modifiers.clear();
        self.held_keys.clear();
        self.held_text.clear();
    }

    /// Pause goes out whole on the press; auto-repeat and the release send nothing.
    fn pause(&mut self, event: &KeyEvent) -> Vec<InputOp> {
        let mut ops = Vec::new();
        if !event.pressed || event.repeat {
            return ops;
        }
        if self.sync_lock_keys(event, &mut ops) {
            self.forget_held();
        }
        self.reconcile_modifiers(event, &mut ops);
        ops.push(InputOp::Pause);
        ops
    }

    /// Bring the server's modifiers in line with the event's flags. A modifier pressed before
    /// the desktop had focus (Shift held while clicking into it) never produced a keydown here,
    /// and one released elsewhere never produced its keyup; either would otherwise change what
    /// the key types.
    fn reconcile_modifiers(&mut self, event: &KeyEvent, ops: &mut Vec<InputOp>) {
        let flags = event.modifiers;
        // Chrome on Linux reports AltGr as AltRight with the AltGraph state but altKey false;
        // letting go of it would turn AltGr+Q (@ on a German layout) into a plain q.
        let alt_graph = flags.alt_graph;
        for (flag, codes) in MODIFIERS {
            let held: Vec<u16> = codes
                .iter()
                .copied()
                .filter(|code| self.held_modifiers.contains(code))
                .collect();
            let down = match flag {
                ModifierFlag::Shift => flags.shift,
                ModifierFlag::Ctrl => flags.ctrl,
                ModifierFlag::Alt => flags.alt || alt_graph,
                ModifierFlag::Meta => flags.meta,
            };
            if down && held.is_empty() {
                let code = if matches!(flag, ModifierFlag::Alt) && !flags.alt {
                    codes[1]
                } else {
                    codes[0]
                };
                self.held_modifiers.insert(code);
                ops.push(InputOp::KeyPressed(code));
            } else if !down {
                for code in held {
                    self.held_modifiers.remove(&code);
                    ops.push(InputOp::KeyReleased(code));
                }
            }
        }
    }

    /// Tell the server the lock-key state when it differs from what it was last told (first
    /// key, or a lock toggled while focus was elsewhere). Returns whether a synchronize event
    /// went out. Windows releases every key on one, so sending it before each key dropped Shift
    /// from every letter typed. An unknown Num Lock state never triggers one and keeps the
    /// value the server was last given.
    fn sync_lock_keys(&mut self, event: &KeyEvent, ops: &mut Vec<InputOp>) -> bool {
        let num_lock = event.modifiers.num_lock.unwrap_or_else(|| {
            self.lock_state
                .map_or(ASSUMED_NUM_LOCK, |(_, num_lock, _)| num_lock)
        });
        let state = (
            event.modifiers.scroll_lock,
            num_lock,
            event.modifiers.caps_lock,
        );
        if self.lock_state == Some(state) {
            return false;
        }
        self.lock_state = Some(state);
        ops.push(InputOp::SynchronizeLocks {
            scroll_lock: state.0,
            num_lock: state.1,
            caps_lock: state.2,
        });
        true
    }

    /// Let go of everything held down, e.g. when focus leaves mid-chord.
    pub fn release_all(&mut self) -> Vec<InputOp> {
        self.forget_held();
        vec![InputOp::ReleaseAll]
    }
}

/// `Math.round`: halves round toward positive infinity.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn clamp_i16(value: f64) -> i16 {
    value.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// The wheel rotations for one wheel event.
pub fn wheel_ops(delta: WheelDelta) -> Vec<InputOp> {
    let mut ops = Vec::new();
    // RDP counts vertical rotation positive away from the user, while DOM deltaY grows toward
    // them. Horizontally both count to the right.
    let vertical = clamp_i16(js_round(-delta.delta_y));
    let horizontal = clamp_i16(js_round(delta.delta_x));
    if vertical != 0 {
        ops.push(InputOp::Wheel {
            vertical: true,
            amount: vertical,
            unit: delta.unit,
        });
    }
    if horizontal != 0 {
        ops.push(InputOp::Wheel {
            vertical: false,
            amount: horizontal,
            unit: delta.unit,
        });
    }
    ops
}

/// The chord the toolbar's "Send Ctrl+Alt+Del" button sends.
pub fn ctrl_alt_del_ops() -> Vec<InputOp> {
    let keys = [scancode::CONTROL_LEFT, scancode::ALT_LEFT, scancode::DELETE];
    keys.iter()
        .map(|&key| InputOp::KeyPressed(key))
        .chain(keys.iter().rev().map(|&key| InputOp::KeyReleased(key)))
        .collect()
}

/// The toolbar's "Send the Windows key" button.
pub fn windows_key_ops() -> Vec<InputOp> {
    vec![
        InputOp::KeyPressed(scancode::META_LEFT),
        InputOp::KeyReleased(scancode::META_LEFT),
    ]
}

/// Where the desktop is drawn inside the view, in view pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ViewRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// The box that fits the remote desktop into the pane without distorting it, centred
/// (`fitRect` in `RemoteDesktopViewImpl.tsx`; noVNC's `scaleViewport` does the same).
pub fn fit_rect(
    viewport_width: f64,
    viewport_height: f64,
    desktop_width: f64,
    desktop_height: f64,
) -> ViewRect {
    if desktop_width <= 0.0
        || desktop_height <= 0.0
        || viewport_width <= 0.0
        || viewport_height <= 0.0
    {
        return ViewRect {
            left: 0.0,
            top: 0.0,
            width: viewport_width,
            height: viewport_height,
        };
    }
    let scale = (viewport_width / desktop_width).min(viewport_height / desktop_height);
    // `Math.round`: halves toward positive infinity.
    let width = (desktop_width * scale + 0.5).floor();
    let height = (desktop_height * scale + 0.5).floor();
    ViewRect {
        left: ((viewport_width - width) / 2.0).floor(),
        top: ((viewport_height - height) / 2.0).floor(),
        width,
        height,
    }
}

/// Map a pointer position in view pixels onto the remote desktop, clamped to its edges.
pub fn desktop_point(
    view: ViewRect,
    desktop_width: u16,
    desktop_height: u16,
    x: f64,
    y: f64,
) -> (u16, u16) {
    let width = f64::from(desktop_width);
    let height = f64::from(desktop_height);
    let desktop_x = (x - view.left) / view.width.max(1.0) * width;
    let desktop_y = (y - view.top) / view.height.max(1.0) * height;
    let clamp = |value: f64, size: f64| js_round(value).clamp(0.0, (size - 1.0).max(0.0)) as u16;
    (clamp(desktop_x, width), clamp(desktop_y, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ops as the TypeScript test's stand-ins print them.
    fn describe(ops: &[InputOp]) -> Vec<String> {
        ops.iter()
            .map(|op| match *op {
                InputOp::KeyPressed(code) => format!("down {code:x}"),
                InputOp::KeyReleased(code) => format!("up {code:x}"),
                InputOp::SynchronizeLocks {
                    scroll_lock,
                    num_lock,
                    caps_lock,
                } => format!(
                    "sync {}{}{}",
                    u8::from(scroll_lock),
                    u8::from(num_lock),
                    u8::from(caps_lock)
                ),
                InputOp::Wheel {
                    vertical, amount, ..
                } => format!("wheel {} {amount}", if vertical { 'v' } else { 'h' }),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[derive(Default, Clone, Copy)]
    struct KeyState {
        shift: bool,
        alt: bool,
        alt_graph: bool,
        caps: bool,
        repeat: bool,
    }

    fn key(pressed: bool, code: &str, state: KeyState) -> KeyEvent {
        KeyEvent {
            code: code.to_owned(),
            key: String::new(),
            pressed,
            repeat: state.repeat,
            modifiers: Modifiers {
                shift: state.shift,
                alt: state.alt,
                alt_graph: state.alt_graph,
                caps_lock: state.caps,
                ..Modifiers::default()
            },
        }
    }

    fn down(code: &str, state: KeyState) -> KeyEvent {
        key(true, code, state)
    }

    fn up(code: &str, state: KeyState) -> KeyEvent {
        key(false, code, state)
    }

    struct Recorder {
        keyboard: RdpKeyboard,
        sent: Vec<String>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                keyboard: RdpKeyboard::new(),
                sent: Vec::new(),
            }
        }

        fn key(&mut self, event: KeyEvent) -> bool {
            match self.keyboard.key(&event) {
                Some(ops) => {
                    self.sent.extend(describe(&ops));
                    true
                }
                None => false,
            }
        }
    }

    const SHIFT: KeyState = KeyState {
        shift: true,
        alt: false,
        alt_graph: false,
        caps: false,
        repeat: false,
    };

    #[test]
    fn keeps_shift_held_while_typing() {
        let mut rdp = Recorder::new();
        rdp.key(down("ShiftLeft", SHIFT));
        rdp.key(down("KeyA", SHIFT));
        rdp.key(up("KeyA", SHIFT));
        rdp.key(down("KeyB", SHIFT));
        // A synchronize event releases every key on Windows; only the first key may send one.
        assert_eq!(
            rdp.sent,
            ["sync 010", "down 2a", "down 1e", "up 1e", "down 30"]
        );
    }

    #[test]
    fn presses_held_modifiers_again_after_syncing_a_lock_toggled_elsewhere() {
        let mut rdp = Recorder::new();
        rdp.key(down("ShiftLeft", SHIFT));
        rdp.sent.clear();
        rdp.key(down(
            "KeyA",
            KeyState {
                caps: true,
                ..SHIFT
            },
        ));
        assert_eq!(rdp.sent, ["sync 011", "down 2a", "down 1e"]);
    }

    #[test]
    fn leaves_num_lock_as_last_set_when_the_local_state_is_unknown() {
        let mut rdp = Recorder::new();
        // Unknown on the first key: the assumed state goes out once.
        rdp.key(down("KeyA", KeyState::default()));
        // Num Lock pressed here toggles the server's lock, and the client follows it.
        rdp.key(down("NumLock", KeyState::default()));
        rdp.key(up("NumLock", KeyState::default()));
        rdp.key(down("KeyB", KeyState::default()));
        // A Caps Lock change syncs again, keeping the toggled Num Lock.
        rdp.key(down(
            "KeyC",
            KeyState {
                caps: true,
                ..KeyState::default()
            },
        ));
        assert_eq!(
            rdp.sent,
            [
                "sync 010",
                "down 1e",
                "down e045",
                "up e045",
                "down 30",
                "sync 001",
                "down 2e"
            ]
        );
    }

    #[test]
    fn syncs_a_known_num_lock_state() {
        let mut rdp = Recorder::new();
        let mut event = down("KeyA", KeyState::default());
        event.modifiers.num_lock = Some(false);
        rdp.key(event.clone());
        rdp.key(down("KeyB", KeyState::default()));
        event.modifiers.num_lock = Some(true);
        rdp.key(event);
        assert_eq!(
            rdp.sent,
            ["sync 000", "down 1e", "down 30", "sync 010", "down 1e"]
        );
    }

    #[test]
    fn lets_a_lock_key_toggle_the_server_without_syncing_first() {
        let mut rdp = Recorder::new();
        rdp.key(down("KeyA", KeyState::default()));
        rdp.sent.clear();
        rdp.key(down(
            "CapsLock",
            KeyState {
                caps: true,
                ..KeyState::default()
            },
        ));
        assert_eq!(rdp.sent, ["down 3a"]);
    }

    #[test]
    fn ignores_auto_repeat_on_lock_keys_and_modifiers_but_not_on_other_keys() {
        let mut rdp = Recorder::new();
        rdp.key(down("KeyA", KeyState::default()));
        rdp.sent.clear();
        // IronRDP would send each repeat as release + press: another Caps Lock toggle.
        assert!(rdp.key(down(
            "CapsLock",
            KeyState {
                caps: true,
                repeat: true,
                ..KeyState::default()
            }
        )));
        rdp.key(down("ShiftLeft", SHIFT));
        rdp.key(down(
            "ShiftLeft",
            KeyState {
                repeat: true,
                ..SHIFT
            },
        ));
        rdp.key(down(
            "KeyB",
            KeyState {
                repeat: true,
                ..SHIFT
            },
        ));
        assert_eq!(rdp.sent, ["down 2a", "down 30"]);
    }

    #[test]
    fn keeps_altgr_held_when_the_browser_reports_altgraph_instead_of_alt_key() {
        let mut rdp = Recorder::new();
        rdp.key(down("KeyA", KeyState::default()));
        rdp.sent.clear();
        // Chrome on Linux: AltRight with getModifierState('AltGraph') and altKey false.
        let alt_graph = KeyState {
            alt_graph: true,
            ..KeyState::default()
        };
        rdp.key(down("AltRight", alt_graph));
        rdp.key(down("KeyQ", alt_graph));
        rdp.key(up("AltRight", KeyState::default()));
        rdp.key(down("KeyQ", KeyState::default()));
        assert_eq!(rdp.sent, ["down e038", "down 10", "up e038", "down 10"]);
    }

    #[test]
    fn presses_altgr_itself_when_it_was_held_before_the_desktop_had_focus() {
        let mut rdp = Recorder::new();
        rdp.key(down(
            "KeyQ",
            KeyState {
                alt_graph: true,
                ..KeyState::default()
            },
        ));
        rdp.key(down(
            "KeyW",
            KeyState {
                alt: true,
                ..KeyState::default()
            },
        ));
        assert_eq!(rdp.sent, ["sync 010", "down e038", "down 10", "down 11"]);
    }

    #[test]
    fn sends_pause_as_its_extended1_sequence_and_ctrl_pause_as_break() {
        let mut rdp = Recorder::new();
        rdp.key(down("Pause", KeyState::default()));
        rdp.key(down(
            "Pause",
            KeyState {
                repeat: true,
                ..KeyState::default()
            },
        ));
        assert!(rdp.key(up("Pause", KeyState::default())));
        assert_eq!(rdp.sent, ["sync 010", "Pause"]);
        rdp.sent.clear();
        let mut ctrl = down("ControlLeft", KeyState::default());
        ctrl.modifiers.ctrl = true;
        rdp.key(ctrl.clone());
        let mut brk = down("Pause", KeyState::default());
        brk.modifiers.ctrl = true;
        rdp.key(brk.clone());
        brk.pressed = false;
        rdp.key(brk);
        assert_eq!(rdp.sent, ["down 1d", "down e046", "up e046"]);
    }

    fn typing(pressed: bool, code: &str, key: &str, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            code: code.to_owned(),
            key: key.to_owned(),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn types_characters_as_unicode_whatever_the_local_layout() {
        let mut rdp = Recorder::new();
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        // German layout: the view names the US key that types the same character.
        rdp.key(typing(true, "KeyZ", "z", none));
        rdp.key(typing(false, "KeyZ", "z", none));
        rdp.key(typing(true, "ShiftLeft", "Shift", shift));
        // Shift+7 types `/`; Shift stays held on the server.
        rdp.key(typing(true, "Slash", "/", shift));
        rdp.key(typing(false, "ShiftLeft", "Shift", none));
        // The key comes up after Shift: the view names it as it went down.
        rdp.key(typing(false, "Slash", "/", none));
        // No US key types these: they used to be dropped.
        rdp.key(typing(true, "", "ö", none));
        assert!(rdp.key(typing(false, "", "ö", none)));
        assert_eq!(
            rdp.sent,
            [
                "sync 010",
                "UnicodePressed('z')",
                "UnicodeReleased('z')",
                "down 2a",
                "UnicodePressed('/')",
                "up 2a",
                "UnicodeReleased('/')",
                "UnicodePressed('ö')",
                "UnicodeReleased('ö')",
            ]
        );
    }

    /// A held key is held on the server too: one press, repeated presses while it repeats,
    /// one release when it comes up, even when a modifier went down in between.
    #[test]
    fn holds_a_typed_character_until_its_key_comes_up() {
        let mut rdp = Recorder::new();
        let none = Modifiers::default();
        rdp.key(typing(true, "KeyW", "w", none));
        for _ in 0..2 {
            let mut repeat = typing(true, "KeyW", "w", none);
            repeat.repeat = true;
            rdp.key(repeat);
        }
        // Ctrl goes down while W is held, and W comes up first.
        let ctrl = Modifiers { ctrl: true, ..none };
        rdp.key(typing(true, "ControlLeft", "Control", ctrl));
        rdp.key(typing(false, "KeyW", "w", ctrl));
        // Its second release (nothing held any more) sends nothing.
        rdp.key(typing(false, "KeyW", "w", none));
        assert_eq!(
            rdp.sent,
            [
                "sync 010",
                "UnicodePressed('w')",
                "UnicodeRepeated('w')",
                "UnicodeRepeated('w')",
                "down 1d",
                "UnicodeReleased('w')",
            ]
        );
        // Letting go of everything forgets the character as well.
        rdp.sent.clear();
        rdp.key(typing(true, "KeyQ", "q", none));
        rdp.keyboard.release_all();
        rdp.key(typing(false, "KeyQ", "q", none));
        assert_eq!(rdp.sent, ["up 1d", "UnicodePressed('q')"]);
    }

    #[test]
    fn types_altgr_characters_without_ctrl_or_alt_held_on_the_server() {
        let mut rdp = Recorder::new();
        // Windows reports AltGr as Ctrl+Alt; the view marks the character as AltGr's.
        let ctrl_alt = Modifiers {
            ctrl: true,
            alt: true,
            ..Modifiers::default()
        };
        rdp.key(typing(true, "ControlLeft", "Control", ctrl_alt));
        rdp.key(typing(true, "AltLeft", "Alt", ctrl_alt));
        rdp.sent.clear();
        let alt_graph = Modifiers {
            alt_graph: true,
            ..ctrl_alt
        };
        rdp.key(typing(true, "KeyQ", "@", alt_graph));
        rdp.key(typing(false, "KeyQ", "@", alt_graph));
        // Letting go of the modifiers afterwards is harmless (the input database knows they
        // are up).
        rdp.key(typing(false, "AltLeft", "Alt", Modifiers::default()));
        assert_eq!(
            rdp.sent,
            [
                "up 1d",
                "up 38",
                "UnicodePressed('@')",
                "UnicodeReleased('@')",
                "up 38"
            ]
        );
    }

    #[test]
    fn keeps_scancodes_for_shortcuts_and_named_keys() {
        let mut rdp = Recorder::new();
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, ..none };
        rdp.key(typing(true, "KeyA", "a", none));
        rdp.key(typing(false, "KeyA", "a", none));
        rdp.sent.clear();
        // Ctrl+A goes as the key; let go of Ctrl first and the key still comes back up.
        rdp.key(typing(true, "KeyA", "a", ctrl));
        rdp.key(typing(false, "KeyA", "a", none));
        assert_eq!(rdp.sent, ["down 1d", "down 1e", "up 1e"]);
        // Alt+F is a menu shortcut, Meta+R a Windows one.
        rdp.key(typing(true, "KeyF", "f", Modifiers { alt: true, ..none }));
        rdp.key(typing(true, "KeyR", "r", Modifiers { meta: true, ..none }));
        assert_eq!(
            &rdp.sent[3..],
            [
                "up 1d",
                "down 38",
                "down 21",
                "up 38",
                "down e05b",
                "down 13"
            ]
        );
        rdp.sent.clear();
        for (code, key) in [
            ("Space", " "),
            ("Enter", "Enter"),
            ("NumpadAdd", "+"),
            ("Tab", "Tab"),
        ] {
            rdp.key(typing(true, code, key, none));
        }
        assert_eq!(
            rdp.sent,
            [
                // Meta+R let go of Alt already; Space lets go of Meta.
                "up e05b", "down 39", "down 1c", "down 4e", "down f"
            ]
        );
        // Keys with no character and no scancode stay the caller's.
        assert!(!rdp.key(typing(true, "", "Unidentified", none)));
    }

    #[test]
    fn reports_keys_without_a_scancode() {
        let mut rdp = Recorder::new();
        assert!(!rdp.key(down("Unidentified", KeyState::default())));
        assert!(rdp.sent.is_empty());
    }

    #[test]
    fn release_all_forgets_held_modifiers() {
        let mut rdp = Recorder::new();
        rdp.key(down("ShiftLeft", SHIFT));
        assert_eq!(rdp.keyboard.release_all(), [InputOp::ReleaseAll]);
        rdp.sent.clear();
        // Shift is still down locally: the next key presses it again.
        rdp.key(down("KeyA", SHIFT));
        assert_eq!(rdp.sent, ["down 2a", "down 1e"]);
    }

    fn wheel(delta_x: f64, delta_y: f64) -> WheelDelta {
        WheelDelta {
            delta_x,
            delta_y,
            unit: WheelUnit::Pixel,
        }
    }

    #[test]
    fn scrolls_up_for_a_negative_delta_y_and_right_for_a_positive_delta_x() {
        let mut sent = describe(&wheel_ops(wheel(0.0, 100.0)));
        sent.extend(describe(&wheel_ops(wheel(40.0, 0.0))));
        assert_eq!(sent, ["wheel v -100", "wheel h 40"]);
    }

    #[test]
    fn sends_both_axes_of_a_diagonal_trackpad_scroll() {
        assert_eq!(
            describe(&wheel_ops(wheel(-12.4, -30.6))),
            ["wheel v 31", "wheel h -12"]
        );
    }

    #[test]
    fn toolbar_chords() {
        assert_eq!(
            describe(&ctrl_alt_del_ops()),
            [
                "down 1d",
                "down 38",
                "down e053",
                "up e053",
                "up 38",
                "up 1d"
            ]
        );
        assert_eq!(describe(&windows_key_ops()), ["down e05b", "up e05b"]);
    }

    #[test]
    fn maps_view_pixels_onto_the_desktop() {
        let view = ViewRect {
            left: 10.0,
            top: 20.0,
            width: 640.0,
            height: 400.0,
        };
        assert_eq!(desktop_point(view, 1280, 800, 10.0, 20.0), (0, 0));
        assert_eq!(desktop_point(view, 1280, 800, 330.0, 220.0), (640, 400));
        // Outside the picture (a drag that left it) clamps to the edge.
        assert_eq!(desktop_point(view, 1280, 800, 9000.0, -5.0), (1279, 0));
    }

    #[test]
    fn fits_the_desktop_into_the_pane() {
        assert_eq!(
            fit_rect(1000.0, 500.0, 1920.0, 1080.0),
            ViewRect {
                left: 55.0,
                top: 0.0,
                width: 889.0,
                height: 500.0
            }
        );
        assert_eq!(
            fit_rect(300.0, 200.0, 0.0, 0.0),
            ViewRect {
                left: 0.0,
                top: 0.0,
                width: 300.0,
                height: 200.0
            }
        );
    }
}
