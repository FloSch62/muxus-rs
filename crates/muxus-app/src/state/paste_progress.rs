//! A paced paste the backend is typing into a terminal (port of `PasteProgress`,
//! `pasteProgressLabel` and `pasteProgressPercent` in `client/src/state/paste-progress.ts`). The
//! progress shows on the tab and over its terminal, both with a way to cancel the paste.

use muxus_core::protocol::PasteProgressState;

/// Where a running paste is, over every paste queued behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteProgress {
    /// The line being sent, from 1, of every queued paste's lines.
    pub line: u64,
    pub lines: u64,
    /// UTF-16 code units sent and queued.
    pub sent: u64,
    pub total: u64,
}

impl PasteProgress {
    /// What a `paste-progress` message leaves to show: progress while the paste runs, nothing
    /// once it is done, cancelled or stopped.
    pub fn from_report(
        state: PasteProgressState,
        line: u64,
        lines: u64,
        sent: u64,
        total: u64,
    ) -> Option<Self> {
        (state == PasteProgressState::Running).then_some(Self {
            line,
            lines,
            sent,
            total,
        })
    }

    /// `pasteProgressLabel`.
    pub fn label(&self) -> String {
        format!("Pasting line {} of {}", self.line, self.lines)
    }

    /// `pasteProgressPercent`: 0–100.
    pub fn percent(&self) -> f32 {
        if self.total > 0 {
            (self.sent as f32 / self.total as f32 * 100.).min(100.)
        } else {
            0.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(line: u64, lines: u64, sent: u64, total: u64) -> PasteProgress {
        PasteProgress {
            line,
            lines,
            sent,
            total,
        }
    }

    #[test]
    fn labels_the_line_being_sent() {
        assert_eq!(progress(2, 5, 10, 40).label(), "Pasting line 2 of 5");
    }

    #[test]
    fn measures_what_was_sent_of_everything_queued() {
        assert_eq!(progress(2, 5, 10, 40).percent(), 25.);
        assert_eq!(progress(5, 5, 50, 40).percent(), 100.);
        assert_eq!(progress(0, 0, 0, 0).percent(), 0.);
    }

    #[test]
    fn shows_progress_only_while_the_paste_runs() {
        assert_eq!(
            PasteProgress::from_report(PasteProgressState::Running, 1, 3, 4, 12),
            Some(progress(1, 3, 4, 12))
        );
        for state in [
            PasteProgressState::Done,
            PasteProgressState::Cancelled,
            PasteProgressState::Stopped,
        ] {
            assert_eq!(PasteProgress::from_report(state, 3, 3, 12, 12), None);
        }
    }
}
