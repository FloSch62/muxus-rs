//! Multi-selection in the file browser (port of `client/src/sftp-selection.ts`), kept free of
//! the UI so it can be tested. It works like a file manager: a click selects one entry,
//! Ctrl-click (Command-click on macOS) adds or removes one, and Shift-click selects the run from
//! the entry clicked last. As in the host tree, the run is re-derived from what was selected
//! around that anchor, so moving the Shift-click back shrinks it again.

use std::collections::HashSet;

use muxus_core::api::{SftpEntry, SftpEntryType};

/// The entry a Shift-click runs from, and what was selected besides the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SftpAnchor {
    pub name: String,
    pub base: HashSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SftpSelection {
    pub names: HashSet<String>,
    pub anchor: Option<SftpAnchor>,
}

impl SftpSelection {
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Just `name`, anchored there.
    fn only(name: &str) -> Self {
        Self {
            names: HashSet::from([name.to_string()]),
            anchor: Some(SftpAnchor {
                name: name.to_string(),
                base: HashSet::new(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpSelectGesture {
    Only,
    Toggle,
    Extend,
}

/// What a click with these modifiers does to the selection.
pub fn sftp_select_gesture(ctrl: bool, meta: bool, shift: bool, is_mac: bool) -> SftpSelectGesture {
    if shift {
        return SftpSelectGesture::Extend;
    }
    // Ctrl-click is the secondary click on macOS.
    if if is_mac { meta } else { ctrl } {
        return SftpSelectGesture::Toggle;
    }
    SftpSelectGesture::Only
}

/// Apply a click on `name`; `order` is the listing as shown.
pub fn select_sftp_entry(
    selection: &SftpSelection,
    name: &str,
    gesture: SftpSelectGesture,
    order: &[String],
) -> SftpSelection {
    match gesture {
        SftpSelectGesture::Toggle => {
            let mut names = selection.names.clone();
            if !names.remove(name) {
                names.insert(name.to_string());
            }
            SftpSelection {
                anchor: Some(SftpAnchor {
                    name: name.to_string(),
                    base: names.clone(),
                }),
                names,
            }
        }
        SftpSelectGesture::Extend if selection.anchor.is_some() => {
            let anchor = selection.anchor.clone().expect("an anchor");
            let to = order.iter().position(|entry| entry == name);
            // An anchor no longer listed, such as a file deleted since, starts the run at the
            // click.
            let from = order.iter().position(|entry| *entry == anchor.name).or(to);
            let mut names = anchor.base.clone();
            if let (Some(from), Some(to)) = (from, to) {
                names.extend(order[from.min(to)..=from.max(to)].iter().cloned());
            }
            SftpSelection {
                names,
                anchor: Some(anchor),
            }
        }
        _ => SftpSelection::only(name),
    }
}

/// A right-click keeps a selection it lands in, and otherwise selects just that entry.
pub fn sftp_context_selection(selection: &SftpSelection, name: &str) -> SftpSelection {
    if selection.contains(name) {
        selection.clone()
    } else {
        SftpSelection::only(name)
    }
}

/// Every listed entry, for Ctrl/Cmd+A.
pub fn select_all_sftp_entries(order: &[String]) -> SftpSelection {
    SftpSelection {
        names: order.iter().cloned().collect(),
        anchor: None,
    }
}

/// The selected entries in listing order; names gone since the last refresh drop out.
pub fn selected_sftp_entries<'a>(
    entries: &'a [SftpEntry],
    selection: &SftpSelection,
) -> Vec<&'a SftpEntry> {
    entries
        .iter()
        .filter(|entry| selection.contains(&entry.name))
        .collect()
}

/// "3 files and 1 folder": what a bulk action is about to touch.
pub fn sftp_entry_count(entries: &[&SftpEntry]) -> String {
    let folders = entries
        .iter()
        .filter(|entry| entry.kind == SftpEntryType::Dir)
        .count();
    let others = entries.len() - folders;
    let plural = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    let mut parts = Vec::new();
    if others > 0 {
        parts.push(plural(others, "file", "files"));
    }
    if folders > 0 {
        parts.push(plural(folders, "folder", "folders"));
    }
    parts.join(" and ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use SftpSelectGesture::{Extend, Only, Toggle};

    fn order() -> Vec<String> {
        ["logs", "a.txt", "b.txt", "c.txt", "d.txt"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn names(selection: &SftpSelection) -> Vec<String> {
        let mut names: Vec<String> = selection.names.iter().cloned().collect();
        names.sort();
        names
    }

    fn empty() -> SftpSelection {
        SftpSelection::default()
    }

    #[test]
    fn toggles_with_ctrl_on_windows_and_linux_and_with_command_on_macos() {
        assert_eq!(sftp_select_gesture(true, false, false, false), Toggle);
        assert_eq!(sftp_select_gesture(false, true, false, true), Toggle);
        // Ctrl-click is the secondary click on macOS, and Command is not a selection key
        // elsewhere.
        assert_eq!(sftp_select_gesture(true, false, false, true), Only);
        assert_eq!(sftp_select_gesture(false, true, false, false), Only);
    }

    #[test]
    fn extends_with_shift_and_selects_one_entry_otherwise() {
        assert_eq!(sftp_select_gesture(false, false, true, false), Extend);
        assert_eq!(sftp_select_gesture(true, false, true, false), Extend);
        assert_eq!(sftp_select_gesture(false, false, false, true), Only);
    }

    #[test]
    fn selects_only_the_clicked_entry_on_a_plain_click() {
        let toggled = select_sftp_entry(&empty(), "a.txt", Toggle, &order());
        assert_eq!(
            names(&select_sftp_entry(&toggled, "c.txt", Only, &order())),
            ["c.txt"]
        );
    }

    #[test]
    fn adds_and_removes_entries_with_ctrl_or_cmd_click() {
        let one = select_sftp_entry(&empty(), "a.txt", Only, &order());
        let two = select_sftp_entry(&one, "c.txt", Toggle, &order());
        assert_eq!(names(&two), ["a.txt", "c.txt"]);
        assert_eq!(
            names(&select_sftp_entry(&two, "a.txt", Toggle, &order())),
            ["c.txt"]
        );
    }

    #[test]
    fn selects_the_run_from_the_last_click_with_shift_in_either_direction() {
        let anchored = select_sftp_entry(&empty(), "b.txt", Only, &order());
        assert_eq!(
            names(&select_sftp_entry(&anchored, "d.txt", Extend, &order())),
            ["b.txt", "c.txt", "d.txt"]
        );
        assert_eq!(
            names(&select_sftp_entry(&anchored, "logs", Extend, &order())),
            ["a.txt", "b.txt", "logs"]
        );
    }

    #[test]
    fn shrinks_the_run_again_when_the_shift_click_moves_back() {
        let anchored = select_sftp_entry(&empty(), "a.txt", Only, &order());
        let long = select_sftp_entry(&anchored, "d.txt", Extend, &order());
        assert_eq!(
            names(&select_sftp_entry(&long, "b.txt", Extend, &order())),
            ["a.txt", "b.txt"]
        );
    }

    #[test]
    fn keeps_what_was_ctrl_or_cmd_selected_before_the_run() {
        let first = select_sftp_entry(&empty(), "logs", Only, &order());
        let anchored = select_sftp_entry(&first, "c.txt", Toggle, &order());
        assert_eq!(
            names(&select_sftp_entry(&anchored, "d.txt", Extend, &order())),
            ["c.txt", "d.txt", "logs"]
        );
    }

    #[test]
    fn treats_shift_click_without_an_anchor_or_on_an_anchor_gone_since_as_a_fresh_start() {
        assert_eq!(
            names(&select_sftp_entry(&empty(), "c.txt", Extend, &order())),
            ["c.txt"]
        );
        let mut with_gone = order();
        with_gone.push("gone.txt".into());
        let anchored = select_sftp_entry(&empty(), "gone.txt", Only, &with_gone);
        assert_eq!(
            names(&select_sftp_entry(&anchored, "c.txt", Extend, &order())),
            ["c.txt"]
        );
    }

    #[test]
    fn keeps_a_selection_the_right_click_lands_in_and_replaces_one_it_misses() {
        let selection = select_sftp_entry(
            &select_sftp_entry(&empty(), "a.txt", Only, &order()),
            "b.txt",
            Toggle,
            &order(),
        );
        assert_eq!(sftp_context_selection(&selection, "b.txt"), selection);
        assert_eq!(
            names(&sftp_context_selection(&selection, "d.txt")),
            ["d.txt"]
        );
    }

    fn entry(name: &str, kind: SftpEntryType) -> SftpEntry {
        SftpEntry {
            name: name.into(),
            kind,
            size: None,
            mtime_ms: None,
            mode: None,
            owner: None,
            group: None,
            download_ticket: None,
        }
    }

    fn entries() -> Vec<SftpEntry> {
        vec![
            entry("logs", SftpEntryType::Dir),
            entry("a.txt", SftpEntryType::File),
            entry("b.txt", SftpEntryType::File),
        ]
    }

    #[test]
    fn selects_everything_with_ctrl_or_cmd_a_and_lists_entries_in_listing_order() {
        let entries = entries();
        let order: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        let all = select_all_sftp_entries(&order);
        let selected: Vec<&str> = selected_sftp_entries(&entries, &all)
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(selected, ["logs", "a.txt", "b.txt"]);
    }

    #[test]
    fn drops_names_that_are_no_longer_listed() {
        let entries = entries();
        let selection = SftpSelection {
            names: HashSet::from(["b.txt".to_string(), "deleted.txt".to_string()]),
            anchor: None,
        };
        let selected = selected_sftp_entries(&entries, &selection);
        assert_eq!(selected, [&entry("b.txt", SftpEntryType::File)]);
    }

    #[test]
    fn counts_files_and_folders_for_confirmations() {
        let entries = entries();
        let all: Vec<&SftpEntry> = entries.iter().collect();
        assert_eq!(sftp_entry_count(&all), "2 files and 1 folder");
        assert_eq!(sftp_entry_count(&all[1..2]), "1 file");
        let logs = entry("logs", SftpEntryType::Dir);
        let tmp = entry("tmp", SftpEntryType::Dir);
        assert_eq!(sftp_entry_count(&[&logs, &tmp]), "2 folders");
    }
}
