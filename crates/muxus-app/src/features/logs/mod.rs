//! The app log viewer (`components/LogViewerDialog.tsx`) over the backend's
//! log buffer (`Backend::app_logs`, `clear_app_logs`).
//!
//! Session logging policy drafts (`session-logging-policy.ts`) are ported in
//! `features::settings::draft` and the host editor's `logic::session_logging`;
//! their fields are drawn by those screens.

pub mod model;
pub mod view;
