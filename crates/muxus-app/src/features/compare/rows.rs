//! The diff the compare dialog draws (Monaco's diff editor in
//! `CodeEditor.tsx`'s `DiffViewer`): side by side, or inline with removed
//! lines above added ones; changed lines paired with the characters that
//! differ marked; unchanged stretches folded away (3 lines of context, at
//! least 5 lines hidden) until clicked; `ignoreTrimWhitespace` treats lines
//! that differ only at their ends as unchanged.
//!
//! Large texts stay responsive: the diff is computed off the UI thread with a
//! deadline, and the view only lays out the rows on screen.

use std::collections::HashSet;
use std::ops::Range;
use std::time::{Duration, Instant};

use muxus_core::js::js_trim;
use similar::{Algorithm, ChangeTag, DiffOp, TextDiff};

/// Unchanged lines kept around a change (`contextLineCount`).
const CONTEXT: usize = 3;
/// Fewer unchanged lines than this stay visible (`minimumLineCount`).
const MINIMUM_HIDDEN: usize = 5;
/// After this long the line diff settles for a coarser result.
const DEADLINE: Duration = Duration::from_secs(2);
/// Paired lines longer than this are marked whole instead of by character.
const MAX_MARKED_LINE: usize = 2_000;

/// One line of one side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number.
    pub number: usize,
    pub text: String,
    /// Byte ranges of `text` that changed.
    pub marks: Vec<Range<usize>>,
}

/// A row of the side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideRow {
    /// The same line on both sides (up to whitespace at its ends when that is ignored).
    Same {
        left: usize,
        right: usize,
        text: String,
        /// The right side's text when it differs from the left's.
        right_text: Option<String>,
    },
    /// A changed line; a missing side is filler.
    Changed {
        left: Option<Line>,
        right: Option<Line>,
    },
}

/// A row of the inline view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineRow {
    Same {
        left: usize,
        right: usize,
        text: String,
    },
    Removed(Line),
    Added(Line),
}

impl SideRow {
    fn same(&self) -> bool {
        matches!(self, SideRow::Same { .. })
    }
}

impl InlineRow {
    fn same(&self) -> bool {
        matches!(self, InlineRow::Same { .. })
    }
}

/// Both views of one diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffModel {
    pub side: Vec<SideRow>,
    pub inline: Vec<InlineRow>,
    /// Unchanged stretches folded away, as row ranges of `side` / `inline`.
    pub side_folds: Vec<Range<usize>>,
    pub inline_folds: Vec<Range<usize>>,
}

/// What the view lays out: a row, or a folded stretch (clicked to unfold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visible {
    Row(usize),
    Fold { fold: usize, count: usize },
}

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

#[allow(clippy::single_range_in_vec_init)]
fn whole(text: &str) -> Vec<Range<usize>> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![0..text.len()]
    }
}

/// Character ranges that differ between two paired lines.
fn inner_marks(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if left.len() > MAX_MARKED_LINE || right.len() > MAX_MARKED_LINE {
        return (whole(left), whole(right));
    }
    let diff = TextDiff::from_chars(left, right);
    let (mut old, mut new) = (Vec::new(), Vec::new());
    let (mut old_at, mut new_at) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_at += len;
                new_at += len;
            }
            ChangeTag::Delete => {
                push_range(&mut old, old_at..old_at + len);
                old_at += len;
            }
            ChangeTag::Insert => {
                push_range(&mut new, new_at..new_at + len);
                new_at += len;
            }
        }
    }
    (old, new)
}

/// Unchanged stretches long enough to fold, given which rows are unchanged.
fn folds(same: &[bool], changed: bool) -> Vec<Range<usize>> {
    let mut folds = Vec::new();
    if !changed {
        return folds;
    }
    let mut start = 0;
    while start < same.len() {
        if !same[start] {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < same.len() && same[end] {
            end += 1;
        }
        let keep_head = if start == 0 { 0 } else { CONTEXT };
        let keep_tail = if end == same.len() { 0 } else { CONTEXT };
        let len = end - start;
        if len >= keep_head + keep_tail + MINIMUM_HIDDEN {
            folds.push(start + keep_head..end - keep_tail);
        }
        start = end;
    }
    folds
}

/// The diff of two prepared texts.
pub fn diff_model(original: &str, modified: &str, ignore_trim_whitespace: bool) -> DiffModel {
    let before = lines(original);
    let after = lines(modified);
    let key = |line: &&str| -> String {
        if ignore_trim_whitespace {
            js_trim(line).to_string()
        } else {
            (*line).to_string()
        }
    };
    let before_keys: Vec<String> = before.iter().map(key).collect();
    let after_keys: Vec<String> = after.iter().map(key).collect();
    let ops = similar::capture_diff_slices_deadline(
        Algorithm::Myers,
        &before_keys,
        &after_keys,
        Some(Instant::now() + DEADLINE),
    );
    let mut model = DiffModel::default();
    let line = |number: usize, text: &str, marks: Vec<Range<usize>>| Line {
        number,
        text: text.to_owned(),
        marks,
    };
    for op in &ops {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for offset in 0..len {
                    let (left, right) = (before[old_index + offset], after[new_index + offset]);
                    model.side.push(SideRow::Same {
                        left: old_index + offset + 1,
                        right: new_index + offset + 1,
                        text: left.to_owned(),
                        right_text: (left != right).then(|| right.to_owned()),
                    });
                    model.inline.push(InlineRow::Same {
                        left: old_index + offset + 1,
                        right: new_index + offset + 1,
                        text: right.to_owned(),
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for offset in 0..old_len {
                    let text = before[old_index + offset];
                    let removed = line(old_index + offset + 1, text, whole(text));
                    model.side.push(SideRow::Changed {
                        left: Some(removed.clone()),
                        right: None,
                    });
                    model.inline.push(InlineRow::Removed(removed));
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for offset in 0..new_len {
                    let text = after[new_index + offset];
                    let added = line(new_index + offset + 1, text, whole(text));
                    model.side.push(SideRow::Changed {
                        left: None,
                        right: Some(added.clone()),
                    });
                    model.inline.push(InlineRow::Added(added));
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let mut removed = Vec::with_capacity(old_len);
                let mut added = Vec::with_capacity(new_len);
                for offset in 0..old_len.max(new_len) {
                    let left = (offset < old_len).then(|| before[old_index + offset]);
                    let right = (offset < new_len).then(|| after[new_index + offset]);
                    let (left_marks, right_marks) = match (left, right) {
                        (Some(left), Some(right)) => inner_marks(left, right),
                        (Some(left), None) => (whole(left), Vec::new()),
                        (None, Some(right)) => (Vec::new(), whole(right)),
                        (None, None) => (Vec::new(), Vec::new()),
                    };
                    let left = left.map(|text| line(old_index + offset + 1, text, left_marks));
                    let right = right.map(|text| line(new_index + offset + 1, text, right_marks));
                    removed.extend(left.clone());
                    added.extend(right.clone());
                    model.side.push(SideRow::Changed { left, right });
                }
                model
                    .inline
                    .extend(removed.into_iter().map(InlineRow::Removed));
                model.inline.extend(added.into_iter().map(InlineRow::Added));
            }
        }
    }
    let changed = ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }));
    let side_same: Vec<bool> = model.side.iter().map(SideRow::same).collect();
    let inline_same: Vec<bool> = model.inline.iter().map(InlineRow::same).collect();
    model.side_folds = folds(&side_same, changed);
    model.inline_folds = folds(&inline_same, changed);
    model
}

/// The rows to lay out: every row, with each folded stretch not in
/// `expanded` replaced by one fold row.
pub fn visible_rows(
    total: usize,
    folds: &[Range<usize>],
    expanded: &HashSet<usize>,
) -> Vec<Visible> {
    let mut visible = Vec::with_capacity(total);
    let mut row = 0;
    for (index, fold) in folds.iter().enumerate() {
        if expanded.contains(&index) {
            continue;
        }
        visible.extend((row..fold.start).map(Visible::Row));
        visible.push(Visible::Fold {
            fold: index,
            count: fold.len(),
        });
        row = fold.end;
    }
    visible.extend((row..total).map(Visible::Row));
    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> Vec<String> {
        (1..=count).map(|n| format!("line {n}")).collect()
    }

    #[test]
    fn pairs_changed_lines_and_marks_the_characters_that_differ() {
        let model = diff_model("a\nshutdown\nc", "a\nno shutdown\nc", false);
        assert_eq!(model.side.len(), 3);
        let SideRow::Changed {
            left: Some(left),
            right: Some(right),
        } = &model.side[1]
        else {
            panic!("a changed row: {:?}", model.side[1]);
        };
        assert_eq!((left.number, right.number), (2, 2));
        assert!(left.marks.is_empty());
        assert_eq!(right.marks, vec![0..3usize]);
        // Inline: the removed line above the added one.
        assert!(matches!(model.inline[1], InlineRow::Removed(_)));
        assert!(matches!(model.inline[2], InlineRow::Added(_)));
        assert_eq!(model.inline.len(), 4);
    }

    #[test]
    fn ignores_whitespace_at_line_ends_when_asked() {
        let model = diff_model("  a\nb", "a  \nb", true);
        assert!(model.side.iter().all(SideRow::same));
        assert_eq!(
            model.side[0],
            SideRow::Same {
                left: 1,
                right: 1,
                text: "  a".into(),
                right_text: Some("a  ".into())
            }
        );
        assert!(!diff_model("  a\nb", "a  \nb", false).side[0].same());
    }

    #[test]
    fn folds_long_unchanged_stretches_around_changes() {
        let before = numbered(40);
        let mut after = before.clone();
        after[19] = "changed".into();
        let model = diff_model(&before.join("\n"), &after.join("\n"), false);
        // Rows 0..16 fold (3 lines kept above the change), and 23..40 after it.
        assert_eq!(model.side_folds, [0..16, 23..40]);
        // Inline has the removed and the added line: one row more.
        assert_eq!(model.inline_folds, [0..16, 24..41]);
        let visible = visible_rows(model.side.len(), &model.side_folds, &HashSet::new());
        assert_eq!(visible[0], Visible::Fold { fold: 0, count: 16 });
        assert_eq!(visible[1], Visible::Row(16));
        assert_eq!(visible.len(), 1 + 7 + 1);
        let expanded: HashSet<usize> = [0].into();
        assert_eq!(
            visible_rows(model.side.len(), &model.side_folds, &expanded).len(),
            16 + 7 + 1
        );
    }

    #[test]
    fn keeps_short_stretches_and_identical_texts_unfolded() {
        let text = numbered(30).join("\n");
        assert!(diff_model(&text, &text, false).side_folds.is_empty());
        let model = diff_model("a\nb\nc\nd", "a\nb\nx\nd", false);
        assert!(model.side_folds.is_empty());
    }

    #[test]
    fn handles_empty_sides() {
        let model = diff_model("", "a\nb", false);
        assert_eq!(model.side.len(), 2);
        assert!(
            model
                .inline
                .iter()
                .all(|row| matches!(row, InlineRow::Added(_)))
        );
        assert_eq!(diff_model("", "", false), DiffModel::default());
    }
}
