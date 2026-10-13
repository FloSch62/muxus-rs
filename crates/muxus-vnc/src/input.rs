// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/. Adapted from noVNC (Copyright The noVNC authors).

//! Keyboard and pointer input the way noVNC sends it.
//!
//! Port of noVNC's `core/input/{util,keyboard}.js` and the input half of `core/rfb.js`
//! (MPL-2.0, see the parity notes): DOM key values become X11 keysyms, physical codes become XT
//! scancodes for servers with the QEMU extended key event, and the keyboard remembers which
//! keysym each held key went down with so the release matches it.

use std::time::{Duration, Instant};

use crate::keysym_tables::{
    self, CODEPOINT_KEYSYMS, XK_ALT_L, XK_ALT_R, XK_CAPS_LOCK, XK_CONTROL_L, XK_EISU_TOGGLE,
    XK_HIRAGANA, XK_ISO_LEVEL3_SHIFT, XK_KATAKANA, XK_META_L, XK_META_R, XK_MODE_SWITCH,
    XK_NUM_LOCK, XK_ROMAJI, XK_SUPER_L, XK_SUPER_R, XK_ZENKAKU_HANKAKU,
};

/// Modifier and lock state at the time of a key event, as `KeyboardEvent` reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub alt_graph: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
}

/// A key going down or up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyEvent {
    /// The physical key, named like `KeyboardEvent.code` (`KeyA`, `ShiftLeft`); empty when the
    /// platform cannot tell (virtual keyboards).
    pub code: String,
    /// The produced key value, like `KeyboardEvent.key` (`a`, `A`, `@`, `Enter`, `Shift`).
    pub key: String,
    /// True for keydown, false for keyup.
    pub pressed: bool,
    /// Auto-repeat of a held key.
    pub repeat: bool,
    pub modifiers: Modifiers,
}

/// Mouse buttons in `MouseEvent.button` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// The "back" side button.
    Back,
    /// The "forward" side button.
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

    /// The bit in an RFB pointer event's button mask.
    pub fn rfb_mask(self) -> u16 {
        match self {
            Self::Left => 1 << 0,
            Self::Middle => 1 << 1,
            Self::Right => 1 << 2,
            Self::Back => 1 << 7,
            Self::Forward => 1 << 8,
        }
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

/// One wheel event, with DOM sign conventions: `delta_y` grows toward the user.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WheelDelta {
    pub delta_x: f64,
    pub delta_y: f64,
    pub unit: WheelUnit,
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
    // noVNC truncates (`absX` goes through `toSigned32bit`).
    let clamp = |value: f64, size: f64| value.floor().clamp(0.0, (size - 1.0).max(0.0)) as u16;
    (clamp(desktop_x, width), clamp(desktop_y, height))
}

/// The platform quirks noVNC works around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// `getKeycode`: the physical key, with Firefox's old names fixed.
fn key_code(event: &KeyEvent) -> &str {
    match event.code.as_str() {
        "" => "Unidentified",
        "OSLeft" => "MetaLeft",
        "OSRight" => "MetaRight",
        code => code,
    }
}

/// `getKey`: the key value, with legacy and platform names fixed.
fn key_value(event: &KeyEvent) -> &str {
    let key = event.key.as_str();
    if !key.is_empty() && key != "Unidentified" {
        return match key {
            "OS" => "Meta",
            "LaunchMyComputer" => "LaunchApplication1",
            "LaunchCalculator" => "LaunchApplication2",
            "UIKeyInputUpArrow" => "ArrowUp",
            "UIKeyInputDownArrow" => "ArrowDown",
            "UIKeyInputLeftArrow" => "ArrowLeft",
            "UIKeyInputRightArrow" => "ArrowRight",
            "UIKeyInputEscape" => "Escape",
            // Broken behaviour in Chrome.
            "\u{0}" if event.code == "NumpadDecimal" => "Delete",
            key => key,
        };
    }
    // Try to deduce it based on the physical key.
    keysym_tables::fixed_key(key_code(event)).unwrap_or("Unidentified")
}

/// `KeyboardEvent.location` for a physical key: 1 left, 2 right, 3 numpad, 0 standard.
fn key_location(code: &str) -> usize {
    if code.starts_with("Numpad") {
        3
    } else if code.ends_with("Left") && code != "ArrowLeft" {
        1
    } else if code.ends_with("Right") && code != "ArrowRight" {
        2
    } else {
        0
    }
}

/// `keysyms.lookup`: the keysym for a Unicode codepoint.
pub fn codepoint_keysym(codepoint: u32) -> u32 {
    // Latin-1 is one-to-one mapping.
    if (0x20..=0xff).contains(&codepoint) {
        return codepoint;
    }
    if let Ok(index) = CODEPOINT_KEYSYMS.binary_search_by_key(&codepoint, |&(code, _)| code) {
        return CODEPOINT_KEYSYMS[index].1;
    }
    // General mapping as final fallback.
    0x0100_0000 | codepoint
}

/// `getKeysym`: the most reliable keysym for a key event.
pub fn keysym_for(event: &KeyEvent, platform: Platform) -> Option<u32> {
    let key = key_value(event);
    if key == "Unidentified" {
        return None;
    }
    let code = key_code(event);
    // First look up special keys.
    if let Some(keysyms) = keysym_tables::dom_key_keysyms(key) {
        let mut location = key_location(code);
        // Safari screws up location for the right cmd key.
        if key == "Meta" && location == 0 {
            location = 2;
        }
        // And for Clear.
        if key == "Clear" && location == 3 && code == "NumLock" {
            location = 0;
        }
        // The original Meta key now gets confused with the Windows key.
        if key == "Meta" {
            if code == "AltLeft" {
                return Some(XK_META_L);
            } else if code == "AltRight" {
                return Some(XK_META_R);
            }
        }
        // macOS has Clear instead of NumLock, but the remote system is probably not macOS, so
        // lying here is probably best.
        if key == "Clear" && code == "NumLock" {
            return Some(XK_NUM_LOCK);
        }
        // Windows sends alternating symbols for some keys when using a Japanese layout. We
        // have no way of synchronising with the IM running on the remote system, so we send
        // some combined keysym instead and hope for the best.
        if platform == Platform::Windows {
            match key {
                "Zenkaku" | "Hankaku" => return Some(XK_ZENKAKU_HANKAKU),
                "Romaji" | "KanaMode" => return Some(XK_ROMAJI),
                _ => {}
            }
        }
        return Some(keysyms[location]);
    }
    // Now we need to look at the Unicode symbol instead.
    let mut units = key.encode_utf16();
    let (Some(unit), None) = (units.next(), units.next()) else {
        return None;
    };
    (unit != 0).then(|| codepoint_keysym(u32::from(unit)))
}

/// One key message for the server, before it is encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMessage {
    /// 0 is NoSymbol (only with a scancode).
    pub keysym: u32,
    /// The physical key, for the XT scancode of a QEMU extended key event.
    pub code: String,
    pub down: bool,
}

/// noVNC's window for merging Windows' fake Ctrl+Alt into one AltGr.
const ALT_GR_WINDOW: Duration = Duration::from_millis(50);
/// How long a lone left Ctrl may wait for the AltRight that would make it AltGr.
pub const ALT_GR_TIMEOUT: Duration = Duration::from_millis(100);

/// The keyboard state of one VNC session.
#[derive(Debug)]
pub struct VncKeyboard {
    platform: Platform,
    /// Keys held down, with the keysym each went down with, in press order.
    down: Vec<(String, Option<u32>)>,
    /// Windows AltGr detection: when the deferred left Ctrl went down.
    alt_gr_armed: Option<Instant>,
    /// The server's Caps/Num Lock as its LED state last said; `None` while unknown.
    remote_caps_lock: Option<bool>,
    remote_num_lock: Option<bool>,
}

impl VncKeyboard {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            down: Vec::new(),
            alt_gr_armed: None,
            remote_caps_lock: None,
            remote_num_lock: None,
        }
    }

    fn is_mac(&self) -> bool {
        self.platform == Platform::Mac
    }

    fn held(&self, code: &str) -> Option<Option<u32>> {
        self.down
            .iter()
            .find(|(held, _)| held == code)
            .map(|(_, keysym)| *keysym)
    }

    /// The server reported its lock LEDs (QEMU LED state pseudo-encoding).
    pub fn set_remote_locks(&mut self, caps_lock: bool, num_lock: bool) {
        self.remote_caps_lock = Some(caps_lock);
        self.remote_num_lock = Some(num_lock);
    }

    /// When a deferred left Ctrl must be sent if no AltRight arrives.
    pub fn alt_gr_deadline(&self) -> Option<Instant> {
        self.alt_gr_armed.map(|armed| armed + ALT_GR_TIMEOUT)
    }

    /// `_sendKeyEvent` followed by RFB's `_handleKeyEvent` (lock sync).
    fn send(
        &mut self,
        out: &mut Vec<KeyMessage>,
        keysym: Option<u32>,
        code: &str,
        down: bool,
        locks: Option<(Option<bool>, Option<bool>)>,
    ) {
        if down {
            match self.down.iter_mut().find(|(held, _)| held == code) {
                Some(entry) => entry.1 = keysym,
                None => self.down.push((code.to_owned(), keysym)),
            }
        } else {
            // Do we really think this key is down?
            let Some(index) = self.down.iter().position(|(held, _)| held == code) else {
                return;
            };
            self.down.remove(index);
        }
        let (num_lock, caps_lock) = locks.unwrap_or((None, None));
        // If the remote state of Caps Lock is known and differs from the local LED state, a
        // Caps Lock press goes first to bring it into sync. Pressing Caps Lock, or fixing it,
        // makes the remote state unknown until the server reports it again.
        if code == "CapsLock" && down {
            self.remote_caps_lock = None;
        }
        if let (Some(remote), Some(local)) = (self.remote_caps_lock, caps_lock)
            && remote != local
            && down
        {
            Self::press_release(out, XK_CAPS_LOCK, "CapsLock");
            self.remote_caps_lock = None;
        }
        // Num Lock works the same way.
        if code == "NumLock" && down {
            self.remote_num_lock = None;
        }
        if let (Some(remote), Some(local)) = (self.remote_num_lock, num_lock)
            && remote != local
            && down
        {
            Self::press_release(out, XK_NUM_LOCK, "NumLock");
            self.remote_num_lock = None;
        }
        out.push(KeyMessage {
            keysym: keysym.unwrap_or(0),
            code: code.to_owned(),
            down,
        });
    }

    fn press_release(out: &mut Vec<KeyMessage>, keysym: u32, code: &str) {
        for down in [true, false] {
            out.push(KeyMessage {
                keysym,
                code: code.to_owned(),
                down,
            });
        }
    }

    /// The messages for one key event at time `now`.
    pub fn key(&mut self, event: &KeyEvent, now: Instant) -> Vec<KeyMessage> {
        let mut out = Vec::new();
        if event.pressed {
            self.key_down(event, now, &mut out);
        } else {
            self.key_up(event, &mut out);
        }
        out
    }

    fn key_down(&mut self, event: &KeyEvent, now: Instant, out: &mut Vec<KeyMessage>) {
        let code = key_code(event).to_owned();
        let mut keysym = keysym_for(event, self.platform);
        let caps_lock = Some(event.modifiers.caps_lock);
        // NumLock state is not available on macOS: unknown rather than off.
        let num_lock = (!self.is_mac()).then_some(event.modifiers.num_lock);
        let locks = Some((num_lock, caps_lock));

        // Windows doesn't have a proper AltGr, but handles it using fake Ctrl+Alt. The remote
        // end might not be Windows, so merge those into a single AltGr when the two arrive
        // directly after each other.
        if let Some(armed) = self.alt_gr_armed.take() {
            if code == "AltRight" && now.duration_since(armed) < ALT_GR_WINDOW {
                keysym = Some(XK_ISO_LEVEL3_SHIFT);
            } else {
                self.send(out, Some(XK_CONTROL_L), "ControlLeft", true, locks);
            }
        }

        // Keys we cannot track: a virtual keyboard, so press and release right away.
        if code == "Unidentified" {
            if keysym.is_some() {
                self.send(out, keysym, &code, true, locks);
                self.send(out, keysym, &code, false, locks);
            }
            return;
        }

        // Alt behaves more like AltGraph on macOS, so shuffle the keys around a bit to make
        // things more sane for the remote server (RealVNC and TigerVNC do the same).
        if self.is_mac() {
            keysym = keysym.map(|keysym| match keysym {
                XK_SUPER_L => XK_ALT_L,
                XK_SUPER_R => XK_SUPER_L,
                XK_ALT_L => XK_MODE_SWITCH,
                XK_ALT_R => XK_ISO_LEVEL3_SHIFT,
                other => other,
            });
        }

        // An already pressed key must keep its keysym or the server gets confused.
        if let Some(held) = self.held(&code) {
            keysym = held;
        }

        // macOS doesn't send proper key releases if a key is pressed while Meta is held.
        if self.is_mac() && event.modifiers.meta && code != "MetaLeft" && code != "MetaRight" {
            self.send(out, keysym, &code, true, locks);
            self.send(out, keysym, &code, false, locks);
            return;
        }

        // macOS only reports Caps Lock state changes: pretend it was a quick press.
        if self.is_mac() && code == "CapsLock" {
            self.send(out, Some(XK_CAPS_LOCK), "CapsLock", true, locks);
            self.send(out, Some(XK_CAPS_LOCK), "CapsLock", false, locks);
            return;
        }

        // Windows doesn't send proper key releases for a bunch of Japanese IM keys.
        let japanese = [
            XK_ZENKAKU_HANKAKU,
            XK_EISU_TOGGLE,
            XK_KATAKANA,
            XK_HIRAGANA,
            XK_ROMAJI,
        ];
        if self.platform == Platform::Windows && keysym.is_some_and(|k| japanese.contains(&k)) {
            self.send(out, keysym, &code, true, locks);
            self.send(out, keysym, &code, false, locks);
            return;
        }

        // Possible start of an AltGr sequence (see above).
        if code == "ControlLeft"
            && self.platform == Platform::Windows
            && self.held("ControlLeft").is_none()
        {
            self.alt_gr_armed = Some(now);
            return;
        }

        self.send(out, keysym, &code, true, locks);
    }

    fn key_up(&mut self, event: &KeyEvent, out: &mut Vec<KeyMessage>) {
        let code = key_code(event).to_owned();
        // A release in the middle of an AltGr sequence aborts the detection.
        self.interrupt_alt_gr_into(out);

        if self.is_mac() && code == "CapsLock" {
            self.send(out, Some(XK_CAPS_LOCK), "CapsLock", true, None);
            self.send(out, Some(XK_CAPS_LOCK), "CapsLock", false, None);
            return;
        }

        let keysym = self.held(&code).flatten();
        self.send(out, keysym, &code, false, None);

        // Windows won't send a release for one Shift while the other is still pressed.
        if self.platform == Platform::Windows && (code == "ShiftLeft" || code == "ShiftRight") {
            for shift in ["ShiftRight", "ShiftLeft"] {
                if let Some(keysym) = self.held(shift) {
                    self.send(out, keysym, shift, false, None);
                }
            }
        }
    }

    fn interrupt_alt_gr_into(&mut self, out: &mut Vec<KeyMessage>) {
        if self.alt_gr_armed.take().is_some() {
            self.send(out, Some(XK_CONTROL_L), "ControlLeft", true, None);
        }
    }

    /// The AltGr window passed without an AltRight: the left Ctrl was a real one.
    pub fn interrupt_alt_gr(&mut self) -> Vec<KeyMessage> {
        let mut out = Vec::new();
        self.interrupt_alt_gr_into(&mut out);
        out
    }

    /// Release every key held down (focus left the desktop).
    pub fn release_all(&mut self) -> Vec<KeyMessage> {
        let mut out = Vec::new();
        // Prevent the control key being processed after losing focus.
        self.interrupt_alt_gr_into(&mut out);
        for (code, keysym) in self.down.clone() {
            self.send(&mut out, keysym, &code, false, None);
        }
        out
    }
}

/// QEMU extended key events carry the scancode in RFB's own numbering: extended keys
/// (0xE0 prefix) set the high bit instead.
pub fn rfb_keycode(xt_scancode: u16) -> u32 {
    let upper = xt_scancode >> 8;
    let lower = xt_scancode & 0xff;
    if upper == 0xe0 && lower < 0x7f {
        u32::from(lower | 0x80)
    } else {
        u32::from(xt_scancode)
    }
}

/// Pixels of wheel travel per RFB wheel step.
const WHEEL_STEP: f64 = 50.0;
/// Assumed pixels for one line step.
const WHEEL_LINE_HEIGHT: f64 = 19.0;

/// noVNC's wheel: deltas accumulate until they make a whole step, which is sent as a press and
/// release of the wheel "button".
#[derive(Debug, Default)]
pub struct WheelAccumulator {
    x: f64,
    y: f64,
}

impl WheelAccumulator {
    /// The button masks to send for this delta, given the buttons currently held.
    pub fn steps(&mut self, delta: WheelDelta, held: u16) -> Vec<u16> {
        let (mut dx, mut dy) = (delta.delta_x, delta.delta_y);
        // Pixel units unless it's non-zero.
        if delta.unit != WheelUnit::Pixel {
            dx *= WHEEL_LINE_HEIGHT;
            dy *= WHEEL_LINE_HEIGHT;
        }
        self.x += dx;
        self.y += dy;
        let mut masks = Vec::new();
        if self.x.abs() >= WHEEL_STEP {
            let bit = if self.x < 0.0 { 1 << 5 } else { 1 << 6 };
            masks.extend([held | bit, held]);
            self.x = 0.0;
        }
        if self.y.abs() >= WHEEL_STEP {
            let bit = if self.y < 0.0 { 1 << 3 } else { 1 << 4 };
            masks.extend([held | bit, held]);
            self.y = 0.0;
        }
        masks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: &str, key: &str, pressed: bool) -> KeyEvent {
        KeyEvent {
            code: code.into(),
            key: key.into(),
            pressed,
            ..KeyEvent::default()
        }
    }

    #[test]
    fn maps_key_values_to_keysyms() {
        let other = Platform::Other;
        assert_eq!(keysym_for(&event("KeyA", "a", true), other), Some(0x61));
        assert_eq!(keysym_for(&event("KeyA", "A", true), other), Some(0x41));
        assert_eq!(keysym_for(&event("Digit2", "@", true), other), Some(0x40));
        assert_eq!(
            keysym_for(&event("Enter", "Enter", true), other),
            Some(0xff0d)
        );
        assert_eq!(
            keysym_for(&event("NumpadEnter", "Enter", true), other),
            Some(0xff8d)
        );
        assert_eq!(
            keysym_for(&event("ShiftRight", "Shift", true), other),
            Some(0xffe2)
        );
        assert_eq!(
            keysym_for(&event("Numpad1", "1", true), other),
            Some(0xffb1)
        );
        assert_eq!(keysym_for(&event("Digit1", "1", true), other), Some(0x31));
        // Outside Latin-1: the keysymdef table, then the Unicode fallback.
        assert_eq!(keysym_for(&event("KeyE", "€", true), other), Some(0x20ac));
        assert_eq!(keysym_for(&event("KeyQ", "ā", true), other), Some(0x03e0));
        assert_eq!(keysym_for(&event("KeyQ", "ア", true), other), Some(0x04b1));
        assert_eq!(
            keysym_for(&event("KeyQ", "ǅ", true), other),
            Some(0x0100_01c5)
        );
        // No key value: the physical key decides for layout-independent keys.
        assert_eq!(keysym_for(&event("Escape", "", true), other), Some(0xff1b));
        assert_eq!(keysym_for(&event("KeyA", "", true), other), None);
        // The Windows key reported as Meta on Alt (Chrome bug) is Meta.
        assert_eq!(
            keysym_for(&event("AltLeft", "Meta", true), other),
            Some(0xffe7)
        );
        assert_eq!(
            keysym_for(&event("MetaLeft", "Meta", true), other),
            Some(0xffeb)
        );
        assert_eq!(
            keysym_for(&event("AltRight", "AltGraph", true), other),
            Some(0xfe03)
        );
    }

    fn sent(messages: &[KeyMessage]) -> Vec<String> {
        messages
            .iter()
            .map(|m| {
                format!(
                    "{} {:x} {}",
                    if m.down { "down" } else { "up" },
                    m.keysym,
                    m.code
                )
            })
            .collect()
    }

    #[test]
    fn releases_a_key_with_the_keysym_it_went_down_with() {
        let mut keyboard = VncKeyboard::new(Platform::Other);
        let now = Instant::now();
        let mut out = keyboard.key(&event("ShiftLeft", "Shift", true), now);
        out.extend(keyboard.key(&event("KeyA", "A", true), now));
        // Shift went up first: the layout now says "a", but the server saw "A" go down.
        out.extend(keyboard.key(&event("ShiftLeft", "Shift", false), now));
        out.extend(keyboard.key(&event("KeyA", "a", false), now));
        // A release of a key never seen going down is dropped.
        out.extend(keyboard.key(&event("KeyB", "b", false), now));
        assert_eq!(
            sent(&out),
            [
                "down ffe1 ShiftLeft",
                "down 41 KeyA",
                "up ffe1 ShiftLeft",
                "up 41 KeyA"
            ]
        );
    }

    #[test]
    fn repeats_keep_the_first_keysym_and_release_all_lets_go() {
        let mut keyboard = VncKeyboard::new(Platform::Other);
        let now = Instant::now();
        let mut out = keyboard.key(&event("KeyQ", "q", true), now);
        out.extend(keyboard.key(&event("KeyQ", "Q", true), now));
        out.extend(keyboard.key(&event("ControlLeft", "Control", true), now));
        out.extend(keyboard.release_all());
        assert_eq!(
            sent(&out),
            [
                "down 71 KeyQ",
                "down 71 KeyQ",
                "down ffe3 ControlLeft",
                "up 71 KeyQ",
                "up ffe3 ControlLeft"
            ]
        );
        assert!(keyboard.release_all().is_empty());
    }

    #[test]
    fn brings_the_remote_caps_lock_in_line_before_a_key() {
        let mut keyboard = VncKeyboard::new(Platform::Other);
        keyboard.set_remote_locks(false, true);
        let now = Instant::now();
        let mut shifted = event("KeyA", "A", true);
        shifted.modifiers.caps_lock = true;
        shifted.modifiers.num_lock = true;
        let out = keyboard.key(&shifted, now);
        assert_eq!(
            sent(&out),
            ["down ffe5 CapsLock", "up ffe5 CapsLock", "down 41 KeyA"]
        );
        // The fix is not repeated before the server reports its LEDs again.
        let mut next = event("KeyB", "B", true);
        next.modifiers.num_lock = true;
        let out = keyboard.key(&next, now);
        assert_eq!(sent(&out), ["down 42 KeyB"]);
    }

    #[test]
    fn merges_windows_fake_ctrl_alt_into_altgr() {
        let mut keyboard = VncKeyboard::new(Platform::Windows);
        let start = Instant::now();
        assert!(
            keyboard
                .key(&event("ControlLeft", "Control", true), start)
                .is_empty()
        );
        assert!(keyboard.alt_gr_deadline().is_some());
        let out = keyboard.key(
            &event("AltRight", "AltGraph", true),
            start + Duration::from_millis(10),
        );
        assert_eq!(sent(&out), ["down fe03 AltRight"]);

        // A lone left Ctrl goes down once the window has passed.
        let mut keyboard = VncKeyboard::new(Platform::Windows);
        assert!(
            keyboard
                .key(&event("ControlLeft", "Control", true), start)
                .is_empty()
        );
        assert_eq!(
            sent(&keyboard.interrupt_alt_gr()),
            ["down ffe3 ControlLeft"]
        );
        assert_eq!(
            sent(&keyboard.key(&event("KeyC", "c", true), start)),
            ["down 63 KeyC"]
        );
    }

    #[test]
    fn shuffles_mac_modifiers_like_tigervnc() {
        let mut keyboard = VncKeyboard::new(Platform::Mac);
        let now = Instant::now();
        let out = keyboard.key(&event("MetaLeft", "Meta", true), now);
        assert_eq!(sent(&out), ["down ffe9 MetaLeft"]);
        let out = keyboard.key(&event("AltLeft", "Alt", true), now);
        assert_eq!(sent(&out), ["down ff7e AltLeft"]);
    }

    #[test]
    fn converts_scancodes_for_qemu() {
        assert_eq!(rfb_keycode(0x1e), 0x1e);
        assert_eq!(rfb_keycode(0xe01d), 0x9d);
        assert_eq!(keysym_tables::xt_scancode("KeyA"), Some(0x1e));
        assert_eq!(keysym_tables::xt_scancode("ControlRight"), Some(0xe01d));
    }

    #[test]
    fn accumulates_wheel_travel_into_steps() {
        let mut wheel = WheelAccumulator::default();
        let small = WheelDelta {
            delta_x: 0.0,
            delta_y: 30.0,
            unit: WheelUnit::Pixel,
        };
        assert!(wheel.steps(small, 0).is_empty());
        assert_eq!(wheel.steps(small, 1), [1 | 16, 1]);
        let lines = WheelDelta {
            delta_x: -3.0,
            delta_y: -3.0,
            unit: WheelUnit::Line,
        };
        assert_eq!(wheel.steps(lines, 0), [32, 0, 8, 0]);
    }

    #[test]
    fn maps_view_pixels_onto_the_desktop() {
        let view = ViewRect {
            left: 0.0,
            top: 0.0,
            width: 400.0,
            height: 300.0,
        };
        assert_eq!(desktop_point(view, 800, 600, 100.6, 50.9), (201, 101));
        assert_eq!(desktop_point(view, 800, 600, -5.0, 900.0), (0, 599));
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
