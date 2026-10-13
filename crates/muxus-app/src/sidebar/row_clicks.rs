//! Click rules for sidebar rows, kept free of UI so they can be tested.
//! Port of `client/src/components/sidebar/row-clicks.ts`.

/// `sidebarOpenGesture`: whether a single click opens a host, or selects it and
/// a double-click opens it.
pub use muxus_store::prefs::state::SidebarOpenGesture;

/// How a host row was activated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostActivation {
    /// Middle-click, Shift+Enter or Shift + double-click: open another session, never list the open ones.
    pub new_session: bool,
    /// The second click of a double-click.
    pub repeat: bool,
    /// The click count in a multi-click, 0 for Enter or Space. `None` for other gestures.
    pub clicks: Option<u32>,
    /// Ctrl/Cmd-click: add the host to the selection or take it out instead of connecting.
    pub toggle_selection: bool,
    /// Shift held: a click selects the range from the last host clicked; from the
    /// keyboard it extends a selection when there is one, else opens a new session.
    pub extend_selection: bool,
}

/// Whether a click on a fixed row (Local terminal, quick-connect) opens it.
///
/// `clicks` is the click count: 1 and 2 for the two clicks of a double-click,
/// higher for a triple-click, and 0 for the click a row synthesizes for Enter
/// or Space. The keyboard opens under either gesture.
pub fn click_opens_row(clicks: u32, gesture: SidebarOpenGesture) -> bool {
    gesture == SidebarOpenGesture::Click || clicks == 0 || clicks == 2
}

/// What a gesture on a host row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostClickOutcome {
    Open,
    Toggle,
    Extend,
    Select,
    Ignore,
}

/// Ctrl/Cmd-click and Shift-click select in either mode; with single-click
/// opening, every other click connects. With double-click opening, the first
/// click is file-manager selection (plain, Ctrl/Cmd or Shift) and the second
/// click opens; anything past a double-click is ignored. From the keyboard,
/// Shift extends a selection that exists and otherwise opens another session.
pub fn host_click_outcome(
    activation: HostActivation,
    gesture: SidebarOpenGesture,
    has_selection: bool,
) -> HostClickOutcome {
    let clicks = activation.clicks.unwrap_or(0);
    let selects_on_click = gesture == SidebarOpenGesture::DoubleClick && clicks > 0;
    if selects_on_click && clicks != 1 {
        return if clicks == 2 {
            HostClickOutcome::Open
        } else {
            HostClickOutcome::Ignore
        };
    }
    if activation.toggle_selection {
        return HostClickOutcome::Toggle;
    }
    if activation.extend_selection && (clicks > 0 || has_selection) {
        return HostClickOutcome::Extend;
    }
    if selects_on_click {
        HostClickOutcome::Select
    } else {
        HostClickOutcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::HostClickOutcome::*;
    use super::SidebarOpenGesture::*;
    use super::*;

    /// A plain left click; `clicks` is the click count.
    fn click(clicks: u32) -> HostActivation {
        HostActivation {
            new_session: false,
            repeat: clicks > 1,
            clicks: Some(clicks),
            ..Default::default()
        }
    }

    fn shift_click(clicks: u32) -> HostActivation {
        HostActivation {
            new_session: true,
            extend_selection: true,
            ..click(clicks)
        }
    }

    fn ctrl_click(clicks: u32) -> HostActivation {
        HostActivation {
            toggle_selection: true,
            ..click(clicks)
        }
    }

    const MIDDLE_CLICK: HostActivation = HostActivation {
        new_session: true,
        repeat: false,
        clicks: None,
        toggle_selection: false,
        extend_selection: false,
    };

    #[test]
    fn single_click_opens_on_every_click_as_before() {
        for clicks in 0..4 {
            assert!(click_opens_row(clicks, Click));
        }
    }

    #[test]
    fn single_click_connects_on_a_plain_click_and_on_the_second_click() {
        assert_eq!(host_click_outcome(click(1), Click, false), Open);
        assert_eq!(host_click_outcome(click(2), Click, false), Open);
    }

    #[test]
    fn single_click_keeps_ctrl_click_for_the_selection() {
        assert_eq!(host_click_outcome(ctrl_click(1), Click, false), Toggle);
    }

    #[test]
    fn single_click_selects_a_range_with_shift_click_even_with_nothing_selected() {
        assert_eq!(host_click_outcome(shift_click(1), Click, true), Extend);
        assert_eq!(host_click_outcome(shift_click(1), Click, false), Extend);
        assert_eq!(host_click_outcome(shift_click(2), Click, false), Extend);
    }

    #[test]
    fn single_click_keeps_shift_from_the_keyboard_and_middle_click_for_another_session() {
        assert_eq!(host_click_outcome(shift_click(0), Click, false), Open);
        assert_eq!(host_click_outcome(shift_click(0), Click, true), Extend);
        assert_eq!(host_click_outcome(MIDDLE_CLICK, Click, true), Open);
    }

    #[test]
    fn double_click_opens_on_the_second_click_and_on_enter_never_on_the_first() {
        assert!(!click_opens_row(1, DoubleClick));
        assert!(click_opens_row(2, DoubleClick));
        assert!(click_opens_row(0, DoubleClick));
    }

    #[test]
    fn double_click_does_not_open_again_on_the_third_click() {
        assert!(!click_opens_row(3, DoubleClick));
        assert_eq!(host_click_outcome(click(3), DoubleClick, true), Ignore);
    }

    #[test]
    fn double_click_selects_on_a_single_click_and_connects_on_the_second() {
        assert_eq!(host_click_outcome(click(1), DoubleClick, false), Select);
        assert_eq!(host_click_outcome(click(1), DoubleClick, true), Select);
        assert_eq!(host_click_outcome(click(2), DoubleClick, true), Open);
    }

    #[test]
    fn double_click_toggles_with_ctrl_and_extends_with_shift_even_with_nothing_selected() {
        assert_eq!(
            host_click_outcome(ctrl_click(1), DoubleClick, false),
            Toggle
        );
        assert_eq!(
            host_click_outcome(shift_click(1), DoubleClick, false),
            Extend
        );
        assert_eq!(
            host_click_outcome(shift_click(1), DoubleClick, true),
            Extend
        );
    }

    #[test]
    fn double_click_opens_a_modified_double_click() {
        assert_eq!(host_click_outcome(shift_click(2), DoubleClick, true), Open);
        assert_eq!(host_click_outcome(ctrl_click(2), DoubleClick, true), Open);
    }

    #[test]
    fn double_click_leaves_the_keyboard_and_middle_click_as_they_were() {
        assert_eq!(host_click_outcome(click(0), DoubleClick, true), Open);
        assert_eq!(
            host_click_outcome(shift_click(0), DoubleClick, true),
            Extend
        );
        assert_eq!(host_click_outcome(shift_click(0), DoubleClick, false), Open);
        assert_eq!(host_click_outcome(MIDDLE_CLICK, DoubleClick, true), Open);
        assert_eq!(
            host_click_outcome(
                HostActivation {
                    toggle_selection: true,
                    ..Default::default()
                },
                DoubleClick,
                false
            ),
            Toggle
        );
    }
}
