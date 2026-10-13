//! Session logging: the recorder that feeds session history and plain-text
//! log files, the text normalizer both use, and the app log buffer
//! (`server/src/session-logging`, `server/src/logging`).

pub mod app_log;
pub mod clock;
pub mod level;
pub mod log_file;
pub mod memory;
pub mod normalizer;
pub mod recorder;
pub mod subscriber;
pub(crate) mod utf8;

pub use clock::{Clock, ManualClock, SystemClock};
pub use log_file::{
    SessionLogFile, default_session_log_directory, session_log_directory, session_log_file_path,
    unused_session_log_file_path,
};
pub use memory::{MemoryHistory, MemorySession};
pub use normalizer::TerminalTextNormalizer;
pub use recorder::{
    FailureSubscription, HistoryEvent, HistorySink, PartLimits, RecorderConfig, SessionIdentity,
    SessionLoggingPatch, SessionLoggingState, SessionRecorder, session_profile_identity,
};
