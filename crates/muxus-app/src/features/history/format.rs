//! The small formatting helpers of `SessionHistoryDialog.tsx`: sizes, the
//! date filter bounds, search snippets and the match counter.

use muxus_core::api::{SNIPPET_MATCH_END, SNIPPET_MATCH_START, SessionLogSummary};

/// `formatBytes`: `512 B`, `1.5 KiB`, `3.2 MiB`.
pub fn format_bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    if value < 1024 * 1024 {
        return format!("{:.1} KiB", value as f64 / 1024.);
    }
    format!("{:.1} MiB", value as f64 / 1024. / 1024.)
}

/// `dateBoundary`: a `YYYY-MM-DD` filter as the ISO instant that starts (or,
/// for the end of a range, ends) that UTC day; `None` when empty or invalid.
pub fn date_boundary(value: &str, end_of_day: bool) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    // `new Date('2026-07-25T…Z')` only reads four-digit years.
    if !(0..=9999).contains(&chrono::Datelike::year(&date)) || value.len() != 10 {
        return None;
    }
    let suffix = if end_of_day {
        "T23:59:59.999Z"
    } else {
        "T00:00:00.000Z"
    };
    Some(format!("{value}{suffix}"))
}

/// One piece of a search snippet: plain text or a highlighted hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetPart {
    Text(String),
    Mark(String),
}

/// `snippetNodes`: the store's sentinel-marked ranges as marks. A start
/// without an end stays plain text, without the sentinel.
pub fn snippet_parts(snippet: &str) -> Vec<SnippetPart> {
    let mut pieces = snippet.split(SNIPPET_MATCH_START);
    let mut parts = vec![SnippetPart::Text(
        pieces.next().unwrap_or_default().to_owned(),
    )];
    for piece in pieces {
        match piece.find(SNIPPET_MATCH_END) {
            None => parts.push(SnippetPart::Text(piece.to_owned())),
            Some(end) => {
                parts.push(SnippetPart::Mark(piece[..end].to_owned()));
                parts.push(SnippetPart::Text(
                    piece[end + SNIPPET_MATCH_END.len_utf8()..].to_owned(),
                ));
            }
        }
    }
    parts.retain(|part| !matches!(part, SnippetPart::Text(text) if text.is_empty()));
    parts
}

/// Highlighting stops here so a one-letter query cannot flood the preview.
pub const MAX_PREVIEW_MATCHES: usize = 1_000;
/// The newest events the preview renders.
pub const MAX_PREVIEW_EVENTS: usize = 5_000;

/// `toLocaleString()` for a count in en-US: digit groups of three.
pub fn group_digits(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The caption above the preview while a search is active.
pub fn match_caption(matches: usize, active: usize) -> String {
    if matches == 0 {
        return "No exact match in the previewed output.".into();
    }
    let total = if matches >= MAX_PREVIEW_MATCHES {
        format!("{}+", group_digits(MAX_PREVIEW_MATCHES))
    } else {
        matches.to_string()
    };
    format!("Match {} of {total} in this preview", active + 1)
}

/// The secondary line of a history row, before the snippet: the recorded
/// title when the session was renamed, when it started, its size and how
/// many chunks matched.
pub fn list_secondary(session: &SessionLogSummary) -> String {
    let mut out = String::new();
    if session.label.is_some() {
        out.push_str(&session.title);
        out.push_str(" · ");
    }
    out.push_str(&super::super::launcher::time_format::format_history_date(
        &session.started_at,
    ));
    out.push_str(" · ");
    out.push_str(&format_bytes(session.raw_bytes));
    if let Some(count) = session.match_count.filter(|count| *count > 0) {
        out.push_str(&format!(
            " · {} {}",
            group_digits(count as usize),
            if count == 1 {
                "matching chunk"
            } else {
                "matching chunks"
            }
        ));
    }
    out
}

/// The subtitle under the selected session's name.
pub fn detail_subtitle(session: &SessionLogSummary) -> String {
    let mut out = String::new();
    if session.label.is_some() && session.title != session.host {
        out.push_str(&session.title);
        out.push_str(" · ");
    }
    out.push_str(&format!(
        "{} · {} · {} raw",
        session.host,
        super::super::launcher::time_format::format_history_date(&session.started_at),
        format_bytes(session.raw_bytes)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_like_the_dialog() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 + 200 * 1024), "3.2 MiB");
    }

    #[test]
    fn bounds_whole_utc_days() {
        assert_eq!(
            date_boundary("2026-07-25", false).as_deref(),
            Some("2026-07-25T00:00:00.000Z")
        );
        assert_eq!(
            date_boundary("2026-07-25", true).as_deref(),
            Some("2026-07-25T23:59:59.999Z")
        );
        assert_eq!(date_boundary("", false), None);
        assert_eq!(date_boundary("2026-02-30", false), None);
    }

    #[test]
    fn splits_snippets_at_their_sentinels() {
        assert_eq!(
            snippet_parts("a \u{1}hit\u{2} b \u{1}open"),
            [
                SnippetPart::Text("a ".into()),
                SnippetPart::Mark("hit".into()),
                SnippetPart::Text(" b ".into()),
                SnippetPart::Text("open".into()),
            ]
        );
    }

    #[test]
    fn counts_matches_with_a_cap() {
        assert_eq!(
            match_caption(0, 0),
            "No exact match in the previewed output."
        );
        assert_eq!(match_caption(3, 1), "Match 2 of 3 in this preview");
        assert_eq!(match_caption(1_000, 0), "Match 1 of 1,000+ in this preview");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
