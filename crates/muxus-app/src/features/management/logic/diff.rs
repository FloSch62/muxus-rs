//! What a change does, line by line (`ReviewDialog.tsx` `lineDelta`, and the
//! side-by-side diff `CodeEditor.tsx`'s `DiffViewer` shows with Monaco:
//! changed lines paired across the two sides, character ranges marked
//! inside them, and long unchanged stretches folded away).

use std::ops::Range;

use similar::{ChangeTag, DiffOp, TextDiff};

/// Lines added and removed, for the summary chips.
pub fn line_delta(original: &str, modified: &str) -> (usize, usize) {
    let before: Vec<&str> = if original.is_empty() {
        Vec::new()
    } else {
        original.split('\n').collect()
    };
    let after: Vec<&str> = if modified.is_empty() {
        Vec::new()
    } else {
        modified.split('\n').collect()
    };
    // Longest common subsequence on lines; configs are small enough for O(n·m) in the common case.
    if before.len() * after.len() > 4_000_000 {
        let mut counts = std::collections::HashMap::<&str, usize>::new();
        for line in &before {
            *counts.entry(line).or_default() += 1;
        }
        let mut common = 0;
        for line in &after {
            if let Some(count) = counts.get_mut(line)
                && *count > 0
            {
                common += 1;
                *count -= 1;
            }
        }
        return (after.len() - common, before.len() - common);
    }
    let mut previous = vec![0u32; after.len() + 1];
    let mut current = vec![0u32; after.len() + 1];
    for line in &before {
        for j in 1..=after.len() {
            current[j] = if *line == after[j - 1] {
                previous[j - 1] + 1
            } else {
                previous[j].max(current[j - 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let common = previous[after.len()] as usize;
    (after.len() - common, before.len() - common)
}

/// One side of a diff row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSide {
    /// 1-based line number.
    pub number: usize,
    pub text: String,
    /// Byte ranges of `text` that changed.
    pub marks: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    /// The same line on both sides.
    Same {
        left: usize,
        right: usize,
        text: String,
    },
    /// A changed line; a missing side is filler.
    Changed {
        left: Option<DiffSide>,
        right: Option<DiffSide>,
    },
    /// Unchanged lines folded away.
    Hidden { count: usize },
}

/// Unchanged lines kept around a change.
const CONTEXT: usize = 3;
/// Fewer unchanged lines than this stay visible.
const MINIMUM_HIDDEN: usize = 5;

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

/// Character ranges that differ between two paired lines.
fn inner_marks(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
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

/// The rows of a side-by-side diff.
pub fn diff_rows(original: &str, modified: &str) -> Vec<DiffRow> {
    let before = lines(original);
    let after = lines(modified);
    let diff = TextDiff::configure().diff_slices(&before, &after);
    let ops = diff.ops();
    let changed = ops.iter().any(|op| !matches!(op, DiffOp::Equal { .. }));
    let mut rows = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                let first = index == 0;
                let last = index == ops.len() - 1;
                let keep_head = if first { 0 } else { CONTEXT };
                let keep_tail = if last { 0 } else { CONTEXT };
                let hidden = len.saturating_sub(keep_head + keep_tail);
                let fold = changed && hidden >= MINIMUM_HIDDEN;
                for offset in 0..len {
                    if fold && offset >= keep_head && offset < len - keep_tail {
                        if offset == keep_head {
                            rows.push(DiffRow::Hidden { count: hidden });
                        }
                        continue;
                    }
                    rows.push(DiffRow::Same {
                        left: old_index + offset + 1,
                        right: new_index + offset + 1,
                        text: before[old_index + offset].to_owned(),
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for offset in 0..old_len {
                    let text = before[old_index + offset];
                    rows.push(DiffRow::Changed {
                        left: Some(DiffSide {
                            number: old_index + offset + 1,
                            text: text.to_owned(),
                            marks: whole(text),
                        }),
                        right: None,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for offset in 0..new_len {
                    let text = after[new_index + offset];
                    rows.push(DiffRow::Changed {
                        left: None,
                        right: Some(DiffSide {
                            number: new_index + offset + 1,
                            text: text.to_owned(),
                            marks: whole(text),
                        }),
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for offset in 0..old_len.max(new_len) {
                    let left = (offset < old_len).then(|| before[old_index + offset]);
                    let right = (offset < new_len).then(|| after[new_index + offset]);
                    let (left_marks, right_marks) = match (left, right) {
                        (Some(left), Some(right)) => inner_marks(left, right),
                        (Some(left), None) => (whole(left), Vec::new()),
                        (None, Some(right)) => (Vec::new(), whole(right)),
                        (None, None) => (Vec::new(), Vec::new()),
                    };
                    rows.push(DiffRow::Changed {
                        left: left.map(|text| DiffSide {
                            number: old_index + offset + 1,
                            text: text.to_owned(),
                            marks: left_marks,
                        }),
                        right: right.map(|text| DiffSide {
                            number: new_index + offset + 1,
                            text: text.to_owned(),
                            marks: right_marks,
                        }),
                    });
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_added_and_removed() {
        assert_eq!(line_delta("", ""), (0, 0));
        assert_eq!(line_delta("a\nb\nc", "a\nb\nc"), (0, 0));
        assert_eq!(line_delta("a\nb\nc", "a\nx\nc"), (1, 1));
        assert_eq!(line_delta("", "a\nb"), (2, 0));
        assert_eq!(line_delta("a\nb", ""), (0, 2));
        assert_eq!(line_delta("a\nb\nc", "c\na\nb"), (1, 1));
    }

    #[test]
    fn counts_large_inputs_by_multiset() {
        let before: Vec<String> = (0..2100).map(|n| format!("line {n}")).collect();
        let mut after = before.clone();
        after[5] = "changed".into();
        after.push("new".into());
        assert_eq!(line_delta(&before.join("\n"), &after.join("\n")), (2, 1));
    }

    #[test]
    fn pairs_changed_lines_and_marks_what_changed() {
        let rows = diff_rows("{\n  \"a\": 1\n}", "{\n  \"a\": 2,\n  \"b\": 3\n}");
        assert_eq!(
            rows[0],
            DiffRow::Same {
                left: 1,
                right: 1,
                text: "{".into()
            }
        );
        let DiffRow::Changed { left, right } = &rows[1] else {
            panic!("{rows:?}");
        };
        let (left, right) = (left.as_ref().unwrap(), right.as_ref().unwrap());
        assert_eq!(left.text, "  \"a\": 1");
        assert_eq!(&left.text[left.marks[0].clone()], "1");
        assert_eq!(
            right
                .marks
                .iter()
                .map(|range| &right.text[range.clone()])
                .collect::<String>(),
            "2,"
        );
        let DiffRow::Changed { left, right } = &rows[2] else {
            panic!("{rows:?}");
        };
        assert!(left.is_none());
        assert_eq!(right.as_ref().unwrap().number, 3);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn folds_long_unchanged_stretches() {
        let before: Vec<String> = (1..=30).map(|n| format!("line {n}")).collect();
        let mut after = before.clone();
        after[15] = "changed".into();
        let rows = diff_rows(&before.join("\n"), &after.join("\n"));
        // 15 lines before the change: 12 folded, 3 kept; 14 after: 3 kept, 11 folded.
        assert_eq!(rows[0], DiffRow::Hidden { count: 12 });
        assert!(matches!(rows[1], DiffRow::Same { left: 13, .. }));
        assert!(matches!(rows[4], DiffRow::Changed { .. }));
        assert!(matches!(rows[7], DiffRow::Same { left: 19, .. }));
        assert_eq!(rows[8], DiffRow::Hidden { count: 11 });
        assert_eq!(rows.len(), 9);
        // Identical texts show in full.
        assert_eq!(diff_rows("a\nb", "a\nb").len(), 2);
    }
}
