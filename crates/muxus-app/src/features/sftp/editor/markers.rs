//! Problem markers as Monaco keeps them for one file (`IMarker`: range, message, severity,
//! source, code) and the marker navigation F8 and Shift+F8 walk
//! (`markerNavigationService.ts`'s `MarkerList`).

use std::ops::Range;

use muxus_ui::gpui_kit::SharedString;

use super::json::Problem;
pub use super::json::Severity;

/// One problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// Byte range in the text (empty for a position).
    pub range: Range<usize>,
    pub message: SharedString,
    pub severity: Severity,
    /// Who reported it (`json`, `css`), shown faded after the message.
    pub source: Option<&'static str>,
    /// The checker's code for it, shown in parentheses after the source.
    pub code: Option<SharedString>,
}

impl Marker {
    /// What Monaco's problem widget shows after the message: `source(code)`.
    pub fn details(&self) -> Option<String> {
        if self.source.is_none() && self.code.is_none() {
            return None;
        }
        let mut details = self.source.unwrap_or("").to_string();
        if let Some(code) = &self.code {
            details.push('(');
            details.push_str(code);
            details.push(')');
        }
        Some(details)
    }
}

/// `vscode-json-languageservice`'s `ErrorCode` for each of its syntax messages.
fn json_code(message: &str) -> u32 {
    match message {
        "Unexpected end of comment." => 257,
        "Unexpected end of string." => 258,
        "Unexpected end of number." => 259,
        "Invalid unicode sequence in string." => 260,
        "Invalid escape character in string." => 261,
        "Invalid characters in string. Control characters must be escaped." => 262,
        "Property expected" => 513,
        "Expected comma" => 514,
        "Colon expected" => 515,
        "Value expected" => 516,
        "Expected comma or closing bracket" => 517,
        "Expected comma or closing brace" => 518,
        "Trailing comma" => 519,
        "Duplicate object key" => 520,
        "Comments are not permitted in JSON." => 521,
        "Property keys must be doublequoted" => 528,
        _ => 0,
    }
}

impl From<Problem> for Marker {
    /// The JSON worker's markers: the parser's carry the language as their source, the
    /// comment check's carry none.
    fn from(problem: Problem) -> Self {
        let code = json_code(problem.message);
        Marker {
            range: problem.range,
            message: problem.message.into(),
            severity: problem.severity,
            source: (code != 521).then_some("json"),
            code: Some(code.to_string().into()),
        }
    }
}

/// The markers in the order the navigation visits them: errors before warnings
/// (`problems.sortOrder` is not `position` in the standalone editor), then by position.
pub fn sort_markers(markers: &mut [Marker]) {
    markers.sort_by_key(|marker| {
        (
            marker.severity != Severity::Error,
            marker.range.start,
            marker.range.end,
        )
    });
}

/// The navigation over a file's markers (`MarkerList` for one resource).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerNavigation {
    /// The marker shown, as an index into the sorted markers.
    index: Option<usize>,
}

impl MarkerNavigation {
    /// The marker shown, if any.
    pub fn selected(&self) -> Option<usize> {
        self.index
    }

    /// Forget the marker shown: the next step starts from the cursor again.
    pub fn reset(&mut self) {
        self.index = None;
    }

    /// Step to the next (`forward`) or previous marker in `markers` (sorted with
    /// [`sort_markers`]), the first step starting from the cursor at `offset`. `widen` gives an
    /// empty range the extent of the word there. Returns the marker's index.
    pub fn step(
        &mut self,
        markers: &[Marker],
        forward: bool,
        offset: usize,
        widen: impl Fn(usize) -> Option<Range<usize>>,
    ) -> Option<usize> {
        let count = markers.len();
        if count == 0 {
            self.index = None;
            return None;
        }
        let next = match self.index {
            Some(index) if index < count => {
                if forward {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                }
            }
            _ => Self::initial(markers, forward, offset, widen),
        };
        self.index = Some(next);
        self.index
    }

    /// `_initIdx`: the first marker at or after the cursor (or the one containing it), the
    /// one before it going back, round to the other end past the last.
    fn initial(
        markers: &[Marker],
        forward: bool,
        offset: usize,
        widen: impl Fn(usize) -> Option<Range<usize>>,
    ) -> usize {
        let count = markers.len();
        let mut found = None;
        for (index, marker) in markers.iter().enumerate() {
            let mut range = marker.range.clone();
            if range.is_empty()
                && let Some(word) = widen(range.start)
            {
                range = word;
            }
            let contains = range.start <= offset && offset <= range.end;
            if contains || offset <= range.start {
                found = Some((index, !contains));
                break;
            }
        }
        match found {
            None => {
                if forward {
                    0
                } else {
                    count - 1
                }
            }
            Some((index, went_past)) if went_past && !forward => (index + count - 1) % count,
            Some((index, _)) => index,
        }
    }

    /// The cursor moved to `offset`: leaving the marker shown starts the next step from the
    /// cursor (the controller's cursor listener).
    pub fn follow_cursor(&mut self, markers: &[Marker], offset: usize) {
        if let Some(index) = self.index
            && !markers
                .get(index)
                .is_some_and(|marker| marker.range.start <= offset && offset <= marker.range.end)
        {
            self.index = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(range: Range<usize>, severity: Severity) -> Marker {
        Marker {
            range,
            message: "m".into(),
            severity,
            source: None,
            code: None,
        }
    }

    fn sorted(mut markers: Vec<Marker>) -> Vec<Marker> {
        sort_markers(&mut markers);
        markers
    }

    #[test]
    fn visits_errors_before_warnings() {
        let markers = sorted(vec![
            marker(30..31, Severity::Error),
            marker(5..6, Severity::Warning),
            marker(10..12, Severity::Error),
        ]);
        let starts: Vec<usize> = markers.iter().map(|m| m.range.start).collect();
        assert_eq!(starts, vec![10, 30, 5]);
    }

    #[test]
    fn starts_from_the_cursor_and_wraps_round() {
        let markers = sorted(vec![
            marker(10..12, Severity::Error),
            marker(30..31, Severity::Error),
            marker(50..55, Severity::Error),
        ]);
        let none = |_| None;
        let mut nav = MarkerNavigation::default();
        // From the top: the first.
        assert_eq!(nav.step(&markers, true, 0, none), Some(0));
        assert_eq!(nav.step(&markers, true, 0, none), Some(1));
        assert_eq!(nav.step(&markers, true, 0, none), Some(2));
        assert_eq!(nav.step(&markers, true, 0, none), Some(0));
        assert_eq!(nav.step(&markers, false, 0, none), Some(2));
        // From between two markers: forward finds the next, back the one before.
        let mut nav = MarkerNavigation::default();
        assert_eq!(nav.step(&markers, true, 20, none), Some(1));
        let mut nav = MarkerNavigation::default();
        assert_eq!(nav.step(&markers, false, 20, none), Some(0));
        // Inside a marker: that one, either way.
        let mut nav = MarkerNavigation::default();
        assert_eq!(nav.step(&markers, false, 52, none), Some(2));
        // Past the last: round to the first going forward, the last going back.
        let mut nav = MarkerNavigation::default();
        assert_eq!(nav.step(&markers, true, 90, none), Some(0));
        let mut nav = MarkerNavigation::default();
        assert_eq!(nav.step(&markers, false, 90, none), Some(2));
    }

    #[test]
    fn widens_an_empty_marker_to_its_word() {
        let markers = vec![marker(10..10, Severity::Error)];
        let mut nav = MarkerNavigation::default();
        // The cursor at 12 is inside the word 8..14 the marker sits in.
        assert_eq!(nav.step(&markers, false, 12, |_| Some(8..14)), Some(0));
    }

    #[test]
    fn leaving_the_marker_starts_over_from_the_cursor() {
        let markers = vec![
            marker(10..12, Severity::Error),
            marker(30..31, Severity::Error),
        ];
        let mut nav = MarkerNavigation::default();
        nav.step(&markers, true, 0, |_| None);
        nav.follow_cursor(&markers, 11);
        assert_eq!(nav.selected(), Some(0));
        nav.follow_cursor(&markers, 40);
        assert_eq!(nav.selected(), None);
    }

    #[test]
    fn json_markers_carry_their_code_and_source() {
        let marker: Marker = Problem {
            range: 3..4,
            message: "Expected comma",
            severity: Severity::Error,
        }
        .into();
        assert_eq!(marker.details().as_deref(), Some("json(514)"));
        let comment: Marker = Problem {
            range: 0..4,
            message: "Comments are not permitted in JSON.",
            severity: Severity::Error,
        }
        .into();
        assert_eq!(comment.details().as_deref(), Some("(521)"));
    }
}
