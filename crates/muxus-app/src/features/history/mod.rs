//! Session history (`client/src/components/SessionHistoryDialog.tsx`,
//! `session-history-matches.ts`).

pub mod format;
pub mod matches;
pub mod view;

pub use view::{HistoryEvent, SessionHistoryDialog, open_history};
