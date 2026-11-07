//! Text preparation and unified-diff export for the compare dialog
//! (`client/src/text-compare.ts`). The view draws its own diff; the export is
//! computed here so it works on the prepared text alone.

use std::sync::LazyLock;

use fancy_regex::Regex;
use muxus_core::js::{is_js_whitespace, js_trim, js_trim_end};

/// How both sides are prepared before they are compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CompareOptions {
    /// Drop a timestamp at the start of each line, such as
    /// `[2026-10-14T09:15:00.123Z]` or `*Oct 14 09:15:00.123:`.
    pub ignore_timestamps: bool,
    /// Collapse runs of spaces and tabs after the indentation and drop trailing ones.
    pub ignore_whitespace: bool,
}

const TIME: &str = r"\d{1,2}:\d{2}:\d{2}(?:[.,]\d+)?";

/// A colon and a hex group after the time make it an address, such as
/// `00:11:22:33:44:55` or `10:20:30::1`. Cisco marks an unsynchronized clock
/// with `*` or `.` and ends the stamp with a colon.
static LEADING_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    let stamp = [
        format!(r"\d{{4}}[-/]\d{{2}}[-/]\d{{2}}[T ]{TIME}(?:Z|[+-]\d{{2}}:?\d{{2}})?"),
        format!(
            r"(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) +\d{{1,2}} +(?:\d{{4}} +)?{TIME}"
        ),
        TIME.to_string(),
    ]
    .join("|");
    let address_group = r"(?::[\dA-Fa-f]{1,4}\b|::)";
    Regex::new(&format!(
        r"^[*.]?(?:\[(?:{stamp})\]|(?:{stamp})(?!{address_group})):?[ \t]*"
    ))
    .unwrap()
});

/// `/(?<=\S)[ \t]+/g` → one space: runs of spaces and tabs after a visible
/// character; the indentation stays.
fn collapse_inner_whitespace(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut previous_visible = false;
    while let Some(c) = chars.next() {
        if (c == ' ' || c == '\t') && previous_visible {
            while matches!(chars.peek(), Some(' ' | '\t')) {
                chars.next();
            }
            out.push(' ');
            previous_visible = false;
            continue;
        }
        previous_visible = !is_js_whitespace(c);
        out.push(c);
    }
    out
}

/// The text as both views and the export compare it: LF line endings, no
/// trailing newline.
pub fn prepare_compare_text(text: &str, options: CompareOptions) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim_end_matches('\n');
    trimmed
        .split('\n')
        .map(|line| {
            let stamped = if options.ignore_timestamps {
                match LEADING_TIMESTAMP.find(line) {
                    Ok(Some(found)) => &line[found.end()..],
                    _ => line,
                }
            } else {
                line
            };
            if options.ignore_whitespace {
                js_trim_end(&collapse_inner_whitespace(stamped)).to_string()
            } else {
                stamped.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Labels and options of [`unified_diff`].
#[derive(Clone, Debug, Default)]
pub struct UnifiedDiffOptions {
    pub original_label: String,
    pub modified_label: String,
    /// Lines that differ only in runs of whitespace or at their ends are unchanged.
    pub ignore_whitespace: bool,
    /// Unchanged lines shown around each change (3 when `None`).
    pub context: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Same,
    Removed,
    Added,
}

/// Past this many changed lines the rest is one replacement, which keeps the trace small.
const MAX_EDITS: usize = 2_000;

/// `diff -u` output for two texts, or `""` when they match. A missing final
/// newline is not reported.
pub fn unified_diff(original: &str, modified: &str, options: &UnifiedDiffOptions) -> String {
    let before = split_lines(original);
    let after = split_lines(modified);
    let key = line_key(options.ignore_whitespace);
    let before_keys: Vec<String> = before.iter().map(|line| key(line)).collect();
    let after_keys: Vec<String> = after.iter().map(|line| key(line)).collect();
    let ops = edit_script(&before_keys, &after_keys);
    let context = options.context.unwrap_or(3);
    let mut old_at = vec![0usize];
    let mut new_at = vec![0usize];
    for op in &ops {
        let (old, new) = (*old_at.last().unwrap(), *new_at.last().unwrap());
        old_at.push(old + usize::from(*op != Op::Added));
        new_at.push(new + usize::from(*op != Op::Removed));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        if ops[index] == Op::Same {
            index += 1;
            continue;
        }
        let start = index.saturating_sub(context);
        let mut end = index + 1;
        let mut next = end;
        while next < ops.len() {
            if ops[next] != Op::Same {
                end = next + 1;
            } else if next - end >= 2 * context {
                break;
            }
            next += 1;
        }
        let stop = ops.len().min(end + context);
        lines.push(format!(
            "@@ -{} +{} @@",
            hunk_range(old_at[start], old_at[stop] - old_at[start]),
            hunk_range(new_at[start], new_at[stop] - new_at[start])
        ));
        for line in start..stop {
            lines.push(match ops[line] {
                Op::Added => format!("+{}", after[new_at[line]]),
                Op::Removed => format!("-{}", before[old_at[line]]),
                Op::Same => format!(" {}", before[old_at[line]]),
            });
        }
        index = stop;
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "--- {}\n+++ {}\n{}\n",
        options.original_label,
        options.modified_label,
        lines.join("\n")
    )
}

/// Whether [`unified_diff`] would find no differences, without building the diff.
pub fn texts_match(original: &str, modified: &str, ignore_whitespace: bool) -> bool {
    if original == modified {
        return true;
    }
    let before = split_lines(original);
    let after = split_lines(modified);
    if before.len() != after.len() {
        return false;
    }
    let key = line_key(ignore_whitespace);
    before.iter().zip(&after).all(|(a, b)| key(a) == key(b))
}

/// `line.replace(/\s+/g, ' ').trim()` when ignoring whitespace.
fn line_key(ignore_whitespace: bool) -> impl Fn(&str) -> String {
    move |line: &str| {
        if !ignore_whitespace {
            return line.to_string();
        }
        let mut out = String::with_capacity(line.len());
        let mut in_space = false;
        for c in line.chars() {
            if is_js_whitespace(c) {
                if !in_space {
                    out.push(' ');
                }
                in_space = true;
            } else {
                out.push(c);
                in_space = false;
            }
        }
        js_trim(&out).to_string()
    }
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn hunk_range(start: usize, count: usize) -> String {
    if count == 1 {
        return (start + 1).to_string();
    }
    format!("{},{count}", if count == 0 { start } else { start + 1 })
}

fn edit_script(a: &[String], b: &[String]) -> Vec<Op> {
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let (mut end_a, mut end_b) = (a.len(), b.len());
    while end_a > head && end_b > head && a[end_a - 1] == b[end_b - 1] {
        end_a -= 1;
        end_b -= 1;
    }
    let mut ops = vec![Op::Same; head];
    ops.extend(myers(&a[head..end_a], &b[head..end_b]));
    ops.extend(std::iter::repeat_n(Op::Same, a.len() - end_a));
    ops
}

/// Myers' O(ND) shortest edit script. Within a change it removes lines before
/// adding them, as diff prints them.
fn myers(a: &[String], b: &[String]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let offset = n + m + 1;
    let mut v = vec![0isize; (2 * offset + 1) as usize];
    // trace[d] holds diagonals -d-1..d+1 as they were before step d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let at = |k: isize| (offset + k) as usize;
    for d in 0..=(n + m).min(MAX_EDITS as isize) {
        trace.push(v[at(-d - 1)..at(d + 2)].to_vec());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                return backtrack(&trace, n, m);
            }
            k += 2;
        }
    }
    let mut ops = vec![Op::Removed; n as usize];
    ops.extend(std::iter::repeat_n(Op::Added, m as usize));
    ops
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let snapshot = &trace[d as usize];
        let at = |k: isize| snapshot[(k + d + 1) as usize];
        let k = x - y;
        let previous = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = at(previous);
        let previous_y = previous_x - previous;
        while x > previous_x && y > previous_y {
            ops.push(Op::Same);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == previous_x {
                Op::Added
            } else {
                Op::Removed
            });
        }
        x = previous_x;
        y = previous_y;
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    //! Port of `tests/unit/client/text-compare.test.ts`.

    use super::*;

    const EXACT: CompareOptions = CompareOptions {
        ignore_timestamps: false,
        ignore_whitespace: false,
    };

    fn labels() -> UnifiedDiffOptions {
        UnifiedDiffOptions {
            original_label: "a".into(),
            modified_label: "b".into(),
            ..UnifiedDiffOptions::default()
        }
    }

    fn numbered(count: usize) -> Vec<String> {
        (1..=count).map(|n| n.to_string()).collect()
    }

    fn hunks(diff: &str) -> Vec<&str> {
        diff.split('\n')
            .filter(|line| line.starts_with("@@"))
            .collect()
    }

    #[test]
    fn normalizes_line_endings_and_drops_the_trailing_newlines() {
        assert_eq!(
            prepare_compare_text("one\r\ntwo\rthree\n\n", EXACT),
            "one\ntwo\nthree"
        );
    }

    #[test]
    fn drops_leading_timestamps_only_when_asked() {
        let text = [
            "[2026-10-14T09:15:00.123Z] show bgp summary",
            "2026-10-14 09:15:00,250 sshd started",
            "2026/10/14T09:15:00+02:00 rebooted",
            "*Oct 14 09:15:00.123: %LINK-3-UPDOWN: Interface Gi0/1, changed state to up",
            "Oct  4 2026 09:15:00 r1 mgd[1]: commit",
            "09:15:00.5 ping ok",
            "2026-10-14 09:15:00,250:DEBUG:ncclient:connected",
            "10.0.0.1 via 10.0.0.2, 00:01:02",
            "interface Gi0/1",
        ]
        .join("\n");
        assert_eq!(prepare_compare_text(&text, EXACT), text);
        let stripped = prepare_compare_text(
            &text,
            CompareOptions {
                ignore_timestamps: true,
                ..EXACT
            },
        );
        assert_eq!(
            stripped.split('\n').collect::<Vec<_>>(),
            [
                "show bgp summary",
                "sshd started",
                "rebooted",
                "%LINK-3-UPDOWN: Interface Gi0/1, changed state to up",
                "r1 mgd[1]: commit",
                "ping ok",
                "DEBUG:ncclient:connected",
                "10.0.0.1 via 10.0.0.2, 00:01:02",
                "interface Gi0/1",
            ]
        );
    }

    #[test]
    fn keeps_mac_and_ipv6_addresses_that_start_like_a_time() {
        let text = [
            "00:11:22:aa:bb:cc dev eth0 master br0",
            "00:11:22:AA:BB:CC dev eth1 master br0",
            "00:11:22:33:44:55 dev eth2 master br0",
            "2001:db8::1 dev eth0 metric 256",
            "10:20:30::1 via fe80::1 dev eth0",
        ]
        .join("\n");
        let options = CompareOptions {
            ignore_timestamps: true,
            ..EXACT
        };
        assert_eq!(prepare_compare_text(&text, options), text);
    }

    #[test]
    fn collapses_inner_whitespace_and_keeps_the_indentation() {
        let options = CompareOptions {
            ignore_whitespace: true,
            ..EXACT
        };
        assert_eq!(
            prepare_compare_text("  Gi0/1\t  up     up  \n neighbor  10.0.0.2 ", options),
            "  Gi0/1 up up\n neighbor 10.0.0.2"
        );
    }

    #[test]
    fn is_empty_when_the_texts_match() {
        assert_eq!(unified_diff("a\nb\n", "a\nb", &labels()), "");
        assert_eq!(unified_diff("", "", &labels()), "");
    }

    #[test]
    fn writes_the_hunks_diff_u_writes() {
        let pre = [
            "hostname r1",
            "interface Gi0/1",
            " description uplink",
            " ip address 10.0.0.1 255.255.255.0",
            " no shutdown",
            "interface Gi0/2",
            " shutdown",
            "router bgp 65000",
            " neighbor 10.0.0.2 remote-as 65001",
            " neighbor 10.0.0.3 remote-as 65002",
            "end",
        ]
        .join("\n");
        let post = pre.replace(
            " neighbor 10.0.0.3 remote-as 65002",
            " neighbor 10.0.0.4 remote-as 65004",
        );
        let options = UnifiedDiffOptions {
            original_label: "pre".into(),
            modified_label: "post".into(),
            ..UnifiedDiffOptions::default()
        };
        assert_eq!(
            unified_diff(&pre, &post, &options),
            [
                "--- pre",
                "+++ post",
                "@@ -7,5 +7,5 @@",
                "  shutdown",
                " router bgp 65000",
                "  neighbor 10.0.0.2 remote-as 65001",
                "- neighbor 10.0.0.3 remote-as 65002",
                "+ neighbor 10.0.0.4 remote-as 65004",
                " end",
                "",
            ]
            .join("\n")
        );
        assert_eq!(
            unified_diff("", "a\nb", &labels()),
            "--- a\n+++ b\n@@ -0,0 +1,2 @@\n+a\n+b\n"
        );
    }

    #[test]
    fn splits_changes_more_than_twice_the_context_apart_into_hunks() {
        let lines = numbered(20);
        let replace = |pairs: &[(&str, &str)]| -> String {
            lines
                .iter()
                .map(|line| {
                    pairs
                        .iter()
                        .find(|(from, _)| from == line)
                        .map_or(line.as_str(), |(_, to)| to)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let original = lines.join("\n");
        let apart = unified_diff(
            &original,
            &replace(&[("2", "two"), ("10", "ten")]),
            &labels(),
        );
        assert_eq!(hunks(&apart), ["@@ -1,5 +1,5 @@", "@@ -7,7 +7,7 @@"]);
        let near = unified_diff(
            &original,
            &replace(&[("2", "two"), ("9", "nine")]),
            &labels(),
        );
        assert_eq!(hunks(&near), ["@@ -1,12 +1,12 @@"]);
    }

    #[test]
    fn keeps_lines_that_differ_only_in_whitespace_when_asked_printing_the_original() {
        let before = "interface Gi0/1\n shutdown\nend";
        let after = "interface  Gi0/1 \n  shutdown\nexit";
        let ignoring = UnifiedDiffOptions {
            ignore_whitespace: true,
            ..labels()
        };
        assert_eq!(
            unified_diff(before, after, &ignoring),
            "--- a\n+++ b\n@@ -1,3 +1,3 @@\n interface Gi0/1\n  shutdown\n-end\n+exit\n"
        );
        assert!(
            unified_diff(before, after, &labels()).contains("-interface Gi0/1\n- shutdown\n-end\n")
        );
    }

    #[test]
    fn prints_removed_lines_before_added_ones_within_a_change() {
        assert_eq!(
            unified_diff("k\na\nb\nk\nc\nk", "k\nx\ny\nk\nz\nk", &labels()),
            "--- a\n+++ b\n@@ -1,6 +1,6 @@\n k\n-a\n-b\n+x\n+y\n k\n-c\n+z\n k\n"
        );
    }

    #[test]
    fn keeps_common_lines_up_to_2000_changed_lines_and_replaces_the_whole_middle_past_that() {
        let middle = |prefix: &str, count: usize| -> String {
            let lines: Vec<String> = numbered(count)
                .into_iter()
                .map(|line| format!("{prefix}{line}"))
                .collect();
            let mut out: Vec<String> = lines[..500].to_vec();
            out.push("keep".into());
            out.extend_from_slice(&lines[500..]);
            out.join("\n")
        };
        assert!(
            unified_diff(&middle("a", 1_000), &middle("b", 1_000), &labels()).contains("\n keep\n")
        );
        let past = unified_diff(&middle("a", 1_000), &middle("b", 1_001), &labels());
        assert!(past.contains("\n-keep\n"));
        assert!(past.contains("\n+keep\n"));
    }

    #[test]
    fn reports_a_very_different_middle_as_one_replacement() {
        let side = |prefix: &str| -> String {
            let mut lines = vec!["head".to_string()];
            lines.extend(numbered(2_500).into_iter().map(|n| format!("{prefix}{n}")));
            lines.push("tail".into());
            lines.join("\n")
        };
        let diff = unified_diff(&side("a"), &side("b"), &labels());
        let lines: Vec<&str> = diff.split('\n').collect();
        assert_eq!(lines[2], "@@ -1,2502 +1,2502 @@");
        assert!(lines[4..2_504].iter().all(|line| line.starts_with("-a")));
        assert!(
            lines[2_504..5_004]
                .iter()
                .all(|line| line.starts_with("+b"))
        );
    }

    #[test]
    fn texts_match_agrees_with_unified_diff() {
        let cases = [
            ("a\nb\n", "a\nb", false),
            ("a\nb", "a\nc", false),
            ("a\nb", "a", false),
            ("  a\tb ", "a b", true),
            ("  a\tb ", "a b", false),
            ("", "", false),
        ];
        for (before, after, ignore_whitespace) in cases {
            let options = UnifiedDiffOptions {
                ignore_whitespace,
                ..labels()
            };
            assert_eq!(
                texts_match(before, after, ignore_whitespace),
                unified_diff(before, after, &options).is_empty(),
                "{before:?} vs {after:?}"
            );
        }
    }
}
