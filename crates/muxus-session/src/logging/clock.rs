//! Wall-clock time for recorded events, replaceable in tests.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use muxus_core::js::iso_timestamp;

/// Where recorders and log files read the time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// `new Date().toISOString()`.
    fn now_iso(&self) -> String {
        iso_timestamp(self.now())
    }
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that only moves when told to (tests).
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<DateTime<Utc>>>);

impl ManualClock {
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(Arc::new(Mutex::new(at)))
    }

    /// Set from an ISO-8601 timestamp; panics on a malformed one.
    pub fn at(iso: &str) -> Self {
        Self::new(
            DateTime::parse_from_rfc3339(iso)
                .expect("valid timestamp")
                .with_timezone(&Utc),
        )
    }

    pub fn set(&self, at: DateTime<Utc>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = at;
    }

    pub fn set_iso(&self, iso: &str) {
        self.set(
            DateTime::parse_from_rfc3339(iso)
                .expect("valid timestamp")
                .with_timezone(&Utc),
        );
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
