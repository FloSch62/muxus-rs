//! Keyboard, pointer and wheel input, translated the way the TypeScript client translates DOM
//! events (physical key codes → scancodes, view pixels → desktop pixels).
//!
//! Port of the input half of `client/src/remote-desktop/rdp-client.ts`. The output is a list of
//! [`InputOp`]s, the same device events the TypeScript code hands to IronRDP's input
//! transactions; the session applies them through IronRDP's input database.

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
    pub num_lock: bool,
    pub scroll_lock: bool,
}

/// A key going down or up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyEvent {
    /// The physical key, named like `KeyboardEvent.code` (`KeyA`, `ShiftLeft`, `NumpadEnter`).
    pub code: String,
    /// The produced key value, like `KeyboardEvent.key`. RDP ignores it (the remote layout
    /// decides what a key means); VNC needs it for keysyms.
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
    /// Let go of every key and button the input database holds.
    ReleaseAll,
}

/// Modifier flags on every key event, with the scancodes that produce them.
#[derive(Clone, Copy)]
enum ModifierFlag {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

const MODIFIERS: [(ModifierFlag, [u16; 2]); 4] = [
    (ModifierFlag::Shift, [0x2a, 0x36]),
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

/// The keyboard state one RDP session keeps between events.
#[derive(Debug, Default)]
pub struct RdpKeyboard {
    /// Modifiers the server currently sees held down.
    held_modifiers: BTreeSet<u16>,
    /// Lock-key state as last reported to the server (scroll, num, caps).
    lock_state: Option<(bool, bool, bool)>,
}

impl RdpKeyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The device events for one key event, or `None` for keys RDP has no scancode for, so
    /// the caller may let them be.
    pub fn key(&mut self, event: &KeyEvent) -> Option<Vec<InputOp>> {
        let scancode = scancode_for_code(&event.code)?;
        let modifier = is_modifier_code(scancode);
        let mut ops = Vec::new();
        if event.pressed {
            // IronRDP sends a repeat of a held key as release + press, which would toggle a
            // held Caps Lock over and over; modifiers have nothing to repeat.
            if event.repeat && (modifier || is_lock_code(&event.code)) {
                return Some(ops);
            }
            // A lock key toggles the server's lock itself; syncing first could undo that.
            if !is_lock_code(&event.code) && self.sync_lock_keys(event, &mut ops) {
                // A synchronize event also lets go of every key on the server
                // ([MS-RDPBCGR] 2.2.8.1.1.3.1.1.5), so held modifiers must go down again.
                self.held_modifiers.clear();
            }
            if modifier {
                self.held_modifiers.insert(scancode);
            } else {
                self.reconcile_modifiers(event, &mut ops);
            }
            ops.push(InputOp::KeyPressed(scancode));
        } else {
            if modifier {
                self.held_modifiers.remove(&scancode);
            }
            ops.push(InputOp::KeyReleased(scancode));
        }
        Some(ops)
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
    /// from every letter typed.
    fn sync_lock_keys(&mut self, event: &KeyEvent, ops: &mut Vec<InputOp>) -> bool {
        let state = (
            event.modifiers.scroll_lock,
            event.modifiers.num_lock,
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
        self.held_modifiers.clear();
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
            ["sync 000", "down 2a", "down 1e", "up 1e", "down 30"]
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
        assert_eq!(rdp.sent, ["sync 001", "down 2a", "down 1e"]);
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
        assert_eq!(rdp.sent, ["sync 000", "down e038", "down 10", "down 11"]);
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
