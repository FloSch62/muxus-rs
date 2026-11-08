//! Program status on tabs: `components/ProgramStatusBadge.tsx` and the wiring of
//! `attachProgramStatus` in `TerminalViewImpl.tsx`.
//!
//! The terminal reports what programs say with OSC 7501 ([`ProgramStatusSignal`]); the window
//! keeps the records per tab (`muxus_app::state::program_status`) and marks the tab: a ring while
//! a program works, a question mark while it waits for the user, and a check or cross until the
//! finished tab is looked at. The text comes from the program and is drawn as plain text.

use std::collections::HashSet;
use std::time::Duration;

use muxus_term_view::muxus_term::program_status::{
    ProgramState, ProgramStatusSignal, program_status_label,
};
use muxus_ui::gpui_kit::{
    Animation, AnimationExt, AnyElement, Context, ElementId, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, ease_in_out, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::{Placement, Tooltip};

use crate::ui::shell::AppShell;

/// `muxus-pulse`: 1.2 s, down to 0.35 opacity halfway.
const PULSE: Duration = Duration::from_millis(1200);

fn pulse_opacity(delta: f32) -> f32 {
    let half = if delta < 0.5 {
        delta * 2.
    } else {
        (1. - delta) * 2.
    };
    1. - 0.65 * ease_in_out(half)
}

impl AppShell {
    /// What a tab's terminal reported (`attachProgramStatus`'s handlers).
    pub(crate) fn on_program_status(
        &mut self,
        tab_id: &str,
        signal: &ProgramStatusSignal,
        cx: &mut Context<Self>,
    ) {
        let changed = match signal {
            ProgramStatusSignal::Report(report) => {
                // The user is looking at this tab right now.
                let watched = self.window_active && self.tabs.active_id.as_deref() == Some(tab_id);
                self.program_status.apply(tab_id, report, watched)
            }
            // A new prompt means the foreground program ended.
            ProgramStatusSignal::Prompt => self.program_status.drop_transient(tab_id),
            // RIS, a full terminal reset, clears every record.
            ProgramStatusSignal::Reset => self.program_status.reset(tab_id),
        };
        if changed {
            cx.notify();
        }
    }

    /// Before each frame: activating a tab, or returning to the window with it active, counts
    /// as looking at it; closed tabs drop their records.
    pub(crate) fn sync_program_status(&mut self) {
        if self.program_status.is_empty() {
            return;
        }
        let open: HashSet<&str> = self.tabs.tabs.iter().map(|tab| tab.id.as_str()).collect();
        self.program_status.retain_tabs(&open);
        if self.window_active
            && let Some(active) = self.tabs.active_id.as_deref()
        {
            self.program_status.mark_seen(active);
        }
    }

    /// `ProgramStatusBadge`: the mark a tab shows, with what the program reported on hover.
    pub(crate) fn program_status_badge(&self, tab_id: &str, theme: &Theme) -> Option<AnyElement> {
        if !self.prefs.show_program_status {
            return None;
        }
        let record = self.program_status.summary(tab_id)?;
        let p = theme.palette;
        let mark = match record.state {
            ProgramState::Working => {
                let ring = CircularProgress::new(
                    ElementId::Name(format!("program-status-ring-{tab_id}").into()),
                    12.,
                    p.info.main,
                )
                .thickness(6.);
                let ring = match record.progress {
                    Some(value) => ring.determinate(f32::from(value)),
                    None => ring,
                };
                div().mx(px(1.)).child(ring).into_any_element()
            }
            ProgramState::Blocked => icon("HelpRounded", 14., p.warning.main)
                .with_animation(
                    ElementId::Name(format!("program-status-pulse-{tab_id}").into()),
                    Animation::new(PULSE).repeat(),
                    |mark, delta| mark.opacity(pulse_opacity(delta)),
                )
                .into_any_element(),
            ProgramState::Done => {
                icon("CheckCircleRounded", 14., p.success.main).into_any_element()
            }
            ProgramState::Error => icon("CancelRounded", 14., p.error.main).into_any_element(),
            ProgramState::Idle => return None,
        };
        let label = program_status_label(record.state, record.kind, record.progress);
        let source = [record.app.as_deref(), record.title.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(": ");
        let lines: Vec<SharedString> = [Some(source).filter(|s| !s.is_empty()), record.msg]
            .into_iter()
            .flatten()
            .map(SharedString::from)
            .collect();
        Some(
            div()
                .id(ElementId::Name(format!("program-status-{tab_id}").into()))
                .flex()
                .flex_none()
                .items_center()
                .child(mark)
                .tooltip(Tooltip::card(label, lines, None, Placement::Bottom))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_dims_to_a_third_halfway_and_comes_back() {
        assert_eq!(pulse_opacity(0.), 1.);
        assert!((pulse_opacity(0.5) - 0.35).abs() < 1e-6);
        assert!((pulse_opacity(1.) - 1.).abs() < 1e-6);
    }
}
