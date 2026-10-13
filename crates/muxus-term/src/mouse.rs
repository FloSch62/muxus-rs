//! Mouse reporting to terminal programs: the protocols (X10, VT200, button-event, any-event) and
//! encodings (default, SGR, SGR-pixels) xterm.js implements, adapted from its
//! `MouseStateService` and `MouseService._triggerMouseEvent` (MIT).

/// Which mouse events the program asked for (DECSET 9 / 1000 / 1002 / 1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseProtocol {
    #[default]
    None,
    X10,
    Vt200,
    Drag,
    Any,
}

/// How reports are encoded (DECSET 1006 / 1016; the default X11 form otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Sgr,
    SgrPixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    None = 3,
    Wheel = 4,
    Aux1 = 8,
    Aux2 = 9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button released (or wheel down).
    Up = 0,
    /// Button pressed (or wheel up).
    Down = 1,
    /// Horizontal wheel left.
    Left = 2,
    /// Horizontal wheel right.
    Right = 3,
    Move = 32,
}

/// One pointer event in cell coordinates (0-based) and pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReport {
    pub col: usize,
    pub row: usize,
    pub x: usize,
    pub y: usize,
    pub button: MouseButton,
    pub action: MouseAction,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

fn event_code(e: &MouseReport, sgr: bool) -> u32 {
    let mut code =
        (if e.ctrl { 16 } else { 0 }) | (if e.shift { 4 } else { 0 }) | (if e.alt { 8 } else { 0 });
    if e.button == MouseButton::Wheel {
        code |= 64;
        code |= e.action as u32;
    } else {
        let button = e.button as u32;
        code |= button & 3;
        if button & 4 != 0 {
            code |= 64;
        }
        if button & 8 != 0 {
            code |= 128;
        }
        if e.action == MouseAction::Move {
            code |= MouseAction::Move as u32;
        } else if e.action == MouseAction::Up && !sgr {
            // Only SGR can report which button was released; the others report NONE.
            code |= MouseButton::None as u32;
        }
    }
    code
}

fn encode(e: &MouseReport, encoding: MouseEncoding) -> Vec<u8> {
    match encoding {
        MouseEncoding::Default => {
            let params = [
                event_code(e, false) + 32,
                e.col as u32 + 32,
                e.row as u32 + 32,
            ];
            // Suppress the report beyond the addressable range (vte and konsole do the same).
            if params.iter().any(|p| *p > 255) {
                return Vec::new();
            }
            let mut out = b"\x1b[M".to_vec();
            out.extend(params.iter().map(|p| *p as u8));
            out
        }
        MouseEncoding::Sgr | MouseEncoding::SgrPixels => {
            let release = e.action == MouseAction::Up && e.button != MouseButton::Wheel;
            let (a, b) = if encoding == MouseEncoding::Sgr {
                (e.col, e.row)
            } else {
                (e.x, e.y)
            };
            format!(
                "\x1b[<{};{a};{b}{}",
                event_code(e, true),
                if release { 'm' } else { 'M' }
            )
            .into_bytes()
        }
    }
}

fn restrict(protocol: MouseProtocol, e: &mut MouseReport) -> bool {
    match protocol {
        MouseProtocol::None => false,
        MouseProtocol::X10 => {
            // No wheel, no move, no up, no modifiers.
            if e.button == MouseButton::Wheel || e.action != MouseAction::Down {
                return false;
            }
            e.ctrl = false;
            e.alt = false;
            e.shift = false;
            true
        }
        MouseProtocol::Vt200 => e.action != MouseAction::Move,
        MouseProtocol::Drag => !(e.action == MouseAction::Move && e.button == MouseButton::None),
        MouseProtocol::Any => true,
    }
}

/// Encodes pointer events for the program, debouncing motion within one cell.
#[derive(Debug, Default, Clone)]
pub struct MouseReporter {
    last: Option<MouseReport>,
}

impl MouseReporter {
    /// The bytes for one event, or `None` when the protocol does not report it. `cols` and `rows`
    /// bound the grid.
    pub fn report(
        &mut self,
        protocol: MouseProtocol,
        encoding: MouseEncoding,
        mut e: MouseReport,
        cols: usize,
        rows: usize,
    ) -> Option<Vec<u8>> {
        if e.col >= cols || e.row >= rows {
            return None;
        }
        // Filter nonsense combinations of button and action.
        if e.button == MouseButton::Wheel && e.action == MouseAction::Move {
            return None;
        }
        if e.button == MouseButton::None && e.action != MouseAction::Move {
            return None;
        }
        if e.button != MouseButton::Wheel
            && matches!(e.action, MouseAction::Left | MouseAction::Right)
        {
            return None;
        }
        // Report 1-based coordinates.
        e.col += 1;
        e.row += 1;
        if e.action == MouseAction::Move
            && let Some(last) = &self.last
        {
            let same = if encoding == MouseEncoding::SgrPixels {
                last.x == e.x && last.y == e.y
            } else {
                last.col == e.col && last.row == e.row
            };
            if same
                && last.button == e.button
                && last.action == e.action
                && last.ctrl == e.ctrl
                && last.alt == e.alt
                && last.shift == e.shift
            {
                return None;
            }
        }
        if !restrict(protocol, &mut e) {
            return None;
        }
        let report = encode(&e, encoding);
        self.last = Some(e);
        (!report.is_empty()).then_some(report)
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Whether a protocol wants any events at all (the terminal then hands clicks to the program
/// unless Shift forces a selection).
pub fn protocol_active(protocol: MouseProtocol) -> bool {
    protocol != MouseProtocol::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(button: MouseButton, action: MouseAction) -> MouseReport {
        MouseReport {
            col: 4,
            row: 2,
            x: 40,
            y: 37,
            button,
            action,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn encodes_like_xterm() {
        let mut reporter = MouseReporter::default();
        let down = reporter.report(
            MouseProtocol::Vt200,
            MouseEncoding::Default,
            event(MouseButton::Left, MouseAction::Down),
            80,
            24,
        );
        assert_eq!(down, Some(vec![0x1b, b'[', b'M', 32, 37, 35]));
        let up = reporter.report(
            MouseProtocol::Vt200,
            MouseEncoding::Default,
            event(MouseButton::Left, MouseAction::Up),
            80,
            24,
        );
        assert_eq!(up, Some(vec![0x1b, b'[', b'M', 35, 37, 35]));
        let sgr_up = reporter.report(
            MouseProtocol::Vt200,
            MouseEncoding::Sgr,
            event(MouseButton::Right, MouseAction::Up),
            80,
            24,
        );
        assert_eq!(sgr_up.as_deref(), Some(&b"\x1b[<2;5;3m"[..]));
        let wheel = reporter.report(
            MouseProtocol::Vt200,
            MouseEncoding::Sgr,
            event(MouseButton::Wheel, MouseAction::Down),
            80,
            24,
        );
        assert_eq!(wheel.as_deref(), Some(&b"\x1b[<65;5;3M"[..]));
        let pixels = reporter.report(
            MouseProtocol::Any,
            MouseEncoding::SgrPixels,
            event(MouseButton::Left, MouseAction::Down),
            80,
            24,
        );
        assert_eq!(pixels.as_deref(), Some(&b"\x1b[<0;40;37M"[..]));
    }

    #[test]
    fn applies_protocol_restrictions() {
        let mut reporter = MouseReporter::default();
        let moved = event(MouseButton::None, MouseAction::Move);
        assert_eq!(
            reporter.report(MouseProtocol::Vt200, MouseEncoding::Sgr, moved, 80, 24),
            None
        );
        assert_eq!(
            reporter.report(MouseProtocol::Drag, MouseEncoding::Sgr, moved, 80, 24),
            None
        );
        assert!(
            reporter
                .report(MouseProtocol::Any, MouseEncoding::Sgr, moved, 80, 24)
                .is_some()
        );
        // Motion within the same cell is reported once.
        assert_eq!(
            reporter.report(MouseProtocol::Any, MouseEncoding::Sgr, moved, 80, 24),
            None
        );
        let mut shifted = event(MouseButton::Left, MouseAction::Down);
        shifted.shift = true;
        assert_eq!(
            reporter
                .report(MouseProtocol::X10, MouseEncoding::Sgr, shifted, 80, 24)
                .as_deref(),
            Some(&b"\x1b[<0;5;3M"[..])
        );
        assert_eq!(
            reporter.report(
                MouseProtocol::X10,
                MouseEncoding::Sgr,
                event(MouseButton::Left, MouseAction::Up),
                80,
                24
            ),
            None
        );
        assert_eq!(
            reporter.report(
                MouseProtocol::None,
                MouseEncoding::Sgr,
                event(MouseButton::Left, MouseAction::Down),
                80,
                24
            ),
            None
        );
        assert_eq!(
            reporter.report(
                MouseProtocol::Any,
                MouseEncoding::Sgr,
                event(MouseButton::Left, MouseAction::Down),
                4,
                24
            ),
            None
        );
    }

    #[test]
    fn suppresses_default_reports_beyond_the_addressable_range() {
        let mut reporter = MouseReporter::default();
        let far = MouseReport {
            col: 300,
            ..event(MouseButton::Left, MouseAction::Down)
        };
        assert_eq!(
            reporter.report(MouseProtocol::Vt200, MouseEncoding::Default, far, 400, 24),
            None
        );
    }
}
