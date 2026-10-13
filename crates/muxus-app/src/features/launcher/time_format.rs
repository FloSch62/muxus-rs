//! Locale date and time labels (`client/src/time-format.ts` and the history
//! dialog's `formatDate`).
//!
//! The TypeScript formats with `Intl.DateTimeFormat` for the user's locale.
//! These are the en-US forms Chromium produces (the documentation
//! screenshots' locale); other locales are not reproduced yet.

use std::cell::RefCell;
use std::collections::HashMap;

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// ICU 72 puts U+202F (narrow no-break space) before AM/PM, but V8 keeps a
/// plain space there for web compatibility, and so does the client.
const MERIDIEM_SPACE: char = ' ';

/// Each distinct timestamp is rendered once: lists that show them re-render
/// on every keystroke, selection and refetch.
const CACHE_LIMIT: usize = 500;

thread_local! {
    static TIMESTAMP_LABELS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn clock(hour: u32, minute: u32, second: Option<u32>) -> String {
    let (h, meridiem) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    match second {
        Some(second) => format!("{h}:{minute:02}:{second:02}{MERIDIEM_SPACE}{meridiem}"),
        None => format!("{h}:{minute:02}{MERIDIEM_SPACE}{meridiem}"),
    }
}

/// `dateStyle: 'medium', timeStyle: 'short'`: `Jul 25, 2026, 8:20 PM`.
pub fn medium_date_short_time<Tz: TimeZone>(at: &DateTime<Tz>) -> String {
    format!(
        "{} {}, {}, {}",
        MONTHS[at.month0() as usize],
        at.day(),
        at.year(),
        clock(at.hour(), at.minute(), None)
    )
}

/// `dateStyle: 'short', timeStyle: 'medium'`: `7/25/26, 8:54:05 PM`.
pub fn short_date_medium_time<Tz: TimeZone>(at: &DateTime<Tz>) -> String {
    format!(
        "{}/{}/{:02}, {}",
        at.month(),
        at.day(),
        at.year().rem_euclid(100),
        clock(at.hour(), at.minute(), Some(at.second()))
    )
}

fn parse(value: &str) -> Option<DateTime<Local>> {
    muxus_core::js::parse_timestamp(value).map(|at| at.with_timezone(&Local))
}

/// `formatTimestamp`: the local date and time of an ISO timestamp, medium
/// date and short time. An unparsable value renders as `Invalid Date`, as
/// `Intl` does for `new Date('garbage')`.
pub fn format_timestamp(value: &str) -> String {
    if let Some(label) = TIMESTAMP_LABELS.with(|cache| cache.borrow().get(value).cloned()) {
        return label;
    }
    let label = parse(value)
        .map(|at| medium_date_short_time(&at))
        .unwrap_or_else(|| "Invalid Date".to_owned());
    TIMESTAMP_LABELS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(value.to_owned(), label.clone());
    });
    label
}

/// The history dialog's `formatDate`: short date and medium time; a value
/// that does not parse is shown as it is.
pub fn format_history_date(value: &str) -> String {
    parse(value)
        .map(|at| short_date_medium_time(&at))
        .unwrap_or_else(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    #[test]
    fn formats_medium_dates_with_short_times() {
        assert_eq!(
            medium_date_short_time(&at("2026-07-25T20:20:00+00:00")),
            "Jul 25, 2026, 8:20\u{20}PM"
        );
        assert_eq!(
            medium_date_short_time(&at("2026-08-03T00:05:59+00:00")),
            "Aug 3, 2026, 12:05\u{20}AM"
        );
        assert_eq!(
            medium_date_short_time(&at("2026-12-31T12:00:00+00:00")),
            "Dec 31, 2026, 12:00\u{20}PM"
        );
    }

    #[test]
    fn formats_short_dates_with_medium_times() {
        assert_eq!(
            short_date_medium_time(&at("2026-07-25T20:54:05+00:00")),
            "7/25/26, 8:54:05\u{20}PM"
        );
        assert_eq!(
            short_date_medium_time(&at("2005-01-02T09:03:04+00:00")),
            "1/2/05, 9:03:04\u{20}AM"
        );
    }

    #[test]
    fn keeps_values_that_do_not_parse() {
        assert_eq!(format_history_date("yesterday"), "yesterday");
        assert_eq!(format_timestamp("yesterday"), "Invalid Date");
    }
}
