//! The live telemetry view's helpers (`LiveView.tsx`): the prefix every
//! subscribed path shares, sorting, ages, sparklines and the CSV export.

use std::cmp::Ordering;

use muxus_core::collate::locale_compare;
use muxus_core::gnmi_path::{GnmiPath, strip_module_prefix, try_parse_gnmi_path};
use muxus_core::js::js_trim;
use muxus_mgmt::workbench::live_table::LiveRow;

use super::format::{iso_ms, js_number_string};
use super::values::value_text;

/// Height of a live row, in px.
pub const ROW_HEIGHT: f32 = 28.;
/// How long a changed state value stays highlighted.
pub const FLASH_MS: u64 = 1200;
/// Width and height of a sparkline, in px.
pub const SPARK_WIDTH: f32 = 84.;
pub const SPARK_HEIGHT: f32 = 18.;

/// Elements every subscribed path shares exactly (no wildcards), shown once above the table.
pub fn common_prefix(paths: &[String]) -> Option<GnmiPath> {
    let parsed: Vec<GnmiPath> = paths
        .iter()
        .filter_map(|path| try_parse_gnmi_path(path))
        .collect();
    let first = parsed.first()?;
    let mut length = 0;
    while length < first.elems.len() {
        let elem = &first.elems[length];
        let wildcard = elem
            .keys
            .iter()
            .flat_map(|keys| keys.iter())
            .any(|(_, value)| value == "*");
        if wildcard {
            break;
        }
        let keys_of = |keys: &Option<muxus_core::gnmi_path::GnmiKeys>| {
            keys.as_ref().map(|keys| keys.0.clone()).unwrap_or_default()
        };
        let same = parsed.iter().all(|path| {
            path.elems.get(length).is_some_and(|other| {
                strip_module_prefix(&other.name) == strip_module_prefix(&elem.name)
                    && keys_of(&other.keys) == keys_of(&elem.keys)
            })
        });
        if !same {
            break;
        }
        length += 1;
    }
    // Keep at least the leaf visible: never fold a whole single-leaf path away.
    if parsed.len() == 1 && length == first.elems.len() {
        length = length.saturating_sub(1);
    }
    Some(GnmiPath {
        origin: None,
        elems: first.elems[..length].to_vec(),
    })
}

/// How long ago, in whole seconds, minutes or hours.
pub fn ago(ms: u64, now: u64) -> String {
    let seconds = ((now as f64 - ms as f64) / 1000.).round().max(0.) as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Path,
    Rate,
    Updated,
    Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Sort {
    /// A click on a column header: the same column flips, another starts
    /// descending (path ascending).
    pub fn clicked(self, key: SortKey) -> Sort {
        if self.key == key {
            Sort {
                key,
                descending: !self.descending,
            }
        } else {
            Sort {
                key,
                descending: key != SortKey::Path,
            }
        }
    }
}

/// `localeCompare(b, undefined, { numeric: true })`: digit runs compare as numbers.
pub fn numeric_compare(a: &str, b: &str) -> Ordering {
    fn chunks(text: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut digits: Option<bool> = None;
        for (index, c) in text.char_indices() {
            let is_digit = c.is_ascii_digit();
            if digits.is_some_and(|current| current != is_digit) {
                out.push((digits.unwrap_or(false), &text[start..index]));
                start = index;
            }
            digits = Some(is_digit);
        }
        if start < text.len() {
            out.push((digits.unwrap_or(false), &text[start..]));
        }
        out
    }
    let (left, right) = (chunks(a), chunks(b));
    for ((left_digits, left), (right_digits, right)) in left.iter().zip(&right) {
        let order = if *left_digits && *right_digits {
            let (l, r) = (left.trim_start_matches('0'), right.trim_start_matches('0'));
            l.len().cmp(&r.len()).then_with(|| l.cmp(r))
        } else {
            locale_compare(left, right)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len()
        .cmp(&right.len())
        .then_with(|| locale_compare(a, b))
}

/// The rows to show: filtered by path or value, then sorted.
pub fn sorted_rows<'a>(
    rows: impl Iterator<Item = &'a LiveRow>,
    filter: &str,
    sort: Sort,
) -> Vec<&'a LiveRow> {
    let needle = js_trim(filter).to_lowercase();
    let mut list: Vec<&LiveRow> = rows
        .filter(|row| {
            needle.is_empty()
                || row.path.to_lowercase().contains(&needle)
                || value_text(Some(&row.value))
                    .to_lowercase()
                    .contains(&needle)
        })
        .collect();
    let direction = |order: Ordering| {
        if sort.descending {
            order.reverse()
        } else {
            order
        }
    };
    list.sort_by(|a, b| {
        direction(match sort.key {
            SortKey::Rate => a
                .rate
                .unwrap_or(-1.)
                .partial_cmp(&b.rate.unwrap_or(-1.))
                .unwrap_or(Ordering::Equal),
            SortKey::Updated => a.received_at.cmp(&b.received_at),
            SortKey::Updates => a.updates.cmp(&b.updates),
            SortKey::Path => numeric_compare(&a.path, &b.path),
        })
    });
    list
}

fn csv_cell(cell: &str) -> String {
    if cell.contains(['"', ',', '\n']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_owned()
    }
}

/// The table as CSV, in the order it is shown.
pub fn live_csv(rows: &[&LiveRow]) -> String {
    let mut lines = vec!["path,value,rate_per_second,updated,updates".to_owned()];
    for row in rows {
        let cells = [
            row.path.clone(),
            value_text(Some(&row.value)),
            row.rate.map(js_number_string).unwrap_or_default(),
            iso_ms(row.updated_at),
            row.updates.to_string(),
        ];
        lines.push(
            cells
                .iter()
                .map(|cell| csv_cell(cell))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\n")
}

/// Points of a sparkline in its 84×18 box; empty below two samples.
pub fn sparkline(points: &[f64]) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let min = points.iter().copied().fold(f64::INFINITY, f64::min);
    let max = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = if max - min == 0. { 1. } else { max - min };
    let step = f64::from(SPARK_WIDTH) / (points.len() - 1) as f64;
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let x = (index as f64 * step * 10.).round() / 10.;
            let y = ((17. - ((point - min) / span) * 15.) * 10.).round() / 10.;
            (x as f32, y as f32)
        })
        .collect()
}

/// The status word above the table.
pub fn status_label(streaming: bool, paused: bool, synced: bool) -> &'static str {
    if !streaming {
        "Stopped"
    } else if paused {
        "Paused"
    } else if synced {
        "Live"
    } else {
        "Receiving initial state …"
    }
}

/// What the empty table says.
pub fn empty_message(streaming: bool, filter: &str) -> &'static str {
    if streaming {
        "Waiting for the first update …"
    } else if !filter.is_empty() {
        "Nothing matches the filter."
    } else {
        "No values were received."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::gnmi_path::format_gnmi_path;
    use serde_json::{Value, json};

    fn prefix(paths: &[&str]) -> Option<String> {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        common_prefix(&paths).map(|path| {
            if path.elems.is_empty() {
                String::new()
            } else {
                format_gnmi_path(&path)
            }
        })
    }

    #[test]
    fn folds_the_shared_prefix_but_not_wildcards_or_the_leaf() {
        assert_eq!(
            prefix(&[
                "/interface[name=e1]/statistics/in-octets",
                "/interface[name=e1]/statistics/out-octets"
            ]),
            Some("/interface[name=e1]/statistics".into())
        );
        assert_eq!(
            prefix(&["/interface[name=*]/statistics"]),
            Some(String::new())
        );
        assert_eq!(
            prefix(&["/system/information/version"]),
            Some("/system/information".into())
        );
        assert_eq!(prefix(&["/a:x/y", "/b:x/z"]), Some("/a:x".into()));
        assert_eq!(prefix(&["/a[k=1]/b", "/a[k=2]/b"]), Some(String::new()));
        assert_eq!(prefix(&["not a [path"]), None);
    }

    #[test]
    fn ages_in_whole_units() {
        assert_eq!(ago(10_000, 10_400), "0s");
        assert_eq!(ago(10_000, 10_600), "1s");
        assert_eq!(ago(0, 59_000), "59s");
        assert_eq!(ago(0, 61_000), "1m");
        assert_eq!(ago(0, 7_300_000), "2h");
        assert_eq!(ago(5_000, 1_000), "0s");
    }

    #[test]
    fn header_clicks_flip_or_start_descending() {
        let sort = Sort::default();
        assert_eq!(
            sort.clicked(SortKey::Path),
            Sort {
                key: SortKey::Path,
                descending: true
            }
        );
        assert_eq!(
            sort.clicked(SortKey::Rate),
            Sort {
                key: SortKey::Rate,
                descending: true
            }
        );
        let rate = sort.clicked(SortKey::Rate);
        assert_eq!(
            rate.clicked(SortKey::Rate),
            Sort {
                key: SortKey::Rate,
                descending: false
            }
        );
    }

    #[test]
    fn compares_digit_runs_as_numbers() {
        assert_eq!(
            numeric_compare("ethernet-1/10", "ethernet-1/9"),
            Ordering::Greater
        );
        assert_eq!(
            numeric_compare("ethernet-1/2", "ethernet-1/10"),
            Ordering::Less
        );
        assert_eq!(numeric_compare("a", "b"), Ordering::Less);
        assert_eq!(numeric_compare("x1", "x1"), Ordering::Equal);
        assert_eq!(numeric_compare("x", "x1"), Ordering::Less);
    }

    fn row(path: &str, value: Value, rate: Option<f64>, updates: u64) -> LiveRow {
        LiveRow {
            path: path.into(),
            value,
            updated_at: 0,
            received_at: updates,
            updates,
            numeric: None,
            rate,
            history: Vec::new(),
            counter: false,
            changed_at: None,
            deleted: false,
            sample_time: None,
        }
    }

    #[test]
    fn sorts_filters_and_exports() {
        let rows = [
            row("/i[name=e10]/c", json!("1"), Some(2.5), 3),
            row("/i[name=e9]/c", json!("a,\"b\""), None, 1),
            row("/i[name=e9]/d", json!("up"), Some(1.), 2),
        ];
        let by_path = sorted_rows(rows.iter(), "", Sort::default());
        assert_eq!(by_path[0].path, "/i[name=e9]/c");
        assert_eq!(by_path[2].path, "/i[name=e10]/c");
        let by_rate = sorted_rows(
            rows.iter(),
            "",
            Sort {
                key: SortKey::Rate,
                descending: true,
            },
        );
        assert_eq!(by_rate[0].rate, Some(2.5));
        assert_eq!(by_rate[2].rate, None);
        let filtered = sorted_rows(rows.iter(), " UP ", Sort::default());
        assert_eq!(filtered.len(), 1);
        let csv = live_csv(&by_path);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "path,value,rate_per_second,updated,updates");
        assert_eq!(
            lines[1],
            "/i[name=e9]/c,\"a,\"\"b\"\"\",,1970-01-01T00:00:00.000Z,1"
        );
        assert_eq!(lines[3], "/i[name=e10]/c,1,2.5,1970-01-01T00:00:00.000Z,3");
    }

    #[test]
    fn draws_sparklines_in_their_box() {
        assert!(sparkline(&[1.]).is_empty());
        assert_eq!(sparkline(&[0., 10.]), vec![(0., 17.), (84., 2.)]);
        assert_eq!(
            sparkline(&[5., 5., 5.]),
            vec![(0., 17.), (42., 17.), (84., 17.)]
        );
    }

    #[test]
    fn says_what_the_stream_is_doing() {
        assert_eq!(status_label(false, false, true), "Stopped");
        assert_eq!(status_label(true, true, true), "Paused");
        assert_eq!(status_label(true, false, true), "Live");
        assert_eq!(
            status_label(true, false, false),
            "Receiving initial state …"
        );
        assert_eq!(empty_message(true, "x"), "Waiting for the first update …");
        assert_eq!(empty_message(false, "x"), "Nothing matches the filter.");
        assert_eq!(empty_message(false, ""), "No values were received.");
    }
}
