//! Ranking for the quick launcher (port of `client/src/quick-launcher.ts`).
//!
//! Every whitespace-separated query token has to appear somewhere in an
//! item's label, detail or keywords; exact and prefix hits on the label then
//! rank ahead of metadata-only hits. Items keep their normalized text next to
//! them (`searchTextCache` in the TypeScript), since normalizing costs more
//! than matching and the catalog is rebuilt far less often than it is queried.

use std::cell::OnceCell;

use muxus_core::js::{is_js_whitespace, js_trim};

/// What a launcher result opens (`QuickLauncherKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuickLauncherKind {
    Tab,
    Editor,
    Host,
    QuickConnect,
    Workspace,
    Command,
    Tunnel,
    History,
    LocalShell,
    Action,
    Keymap,
}

impl QuickLauncherKind {
    /// The kind as the TypeScript spells it (`'quick-connect'`, `'local-shell'`).
    pub fn as_str(self) -> &'static str {
        match self {
            QuickLauncherKind::Tab => "tab",
            QuickLauncherKind::Editor => "editor",
            QuickLauncherKind::Host => "host",
            QuickLauncherKind::QuickConnect => "quick-connect",
            QuickLauncherKind::Workspace => "workspace",
            QuickLauncherKind::Command => "command",
            QuickLauncherKind::Tunnel => "tunnel",
            QuickLauncherKind::History => "history",
            QuickLauncherKind::LocalShell => "local-shell",
            QuickLauncherKind::Action => "action",
            QuickLauncherKind::Keymap => "keymap",
        }
    }
}

/// The searchable part of a launcher result (`QuickLauncherItem`).
#[derive(Clone, Debug, Default)]
pub struct QuickLauncherItem {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub keywords: Vec<String>,
    /// Higher values win when the query matches equally well.
    pub priority: i32,
    /// Initial suggestions stay deliberately small and task-oriented.
    pub show_when_empty: bool,
    search: OnceCell<SearchText>,
}

impl PartialEq for QuickLauncherItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.label == other.label
            && self.detail == other.detail
            && self.keywords == other.keywords
            && self.priority == other.priority
            && self.show_when_empty == other.show_when_empty
    }
}

impl QuickLauncherItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: detail.into(),
            ..Self::default()
        }
    }

    pub fn keywords<I, S>(mut self, keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn show_when_empty(mut self, show: bool) -> Self {
        self.show_when_empty = show;
        self
    }

    fn search_text(&self) -> &SearchText {
        self.search.get_or_init(|| {
            let label = normalize(&self.label);
            let words = split_words(&label);
            let mut all = vec![self.label.as_str(), self.detail.as_str()];
            all.extend(self.keywords.iter().map(String::as_str));
            SearchText {
                searchable: normalize(&all.join(" ")),
                words,
                label,
            }
        })
    }
}

/// Anything carrying a [`QuickLauncherItem`] (the generic return type of
/// `selectQuickLauncherItems` preserves each caller's action payload).
pub trait LauncherItem {
    fn item(&self) -> &QuickLauncherItem;
}

impl LauncherItem for QuickLauncherItem {
    fn item(&self) -> &QuickLauncherItem {
        self
    }
}

#[derive(Clone, Debug, Default)]
struct SearchText {
    label: String,
    /// `label.split(/\s+/)`: empty words stay, as in the TypeScript.
    words: Vec<String>,
    searchable: String,
}

/// The launcher's result cap (`RESULT_LIMIT`).
pub const RESULT_LIMIT: usize = 40;

/// Match every whitespace-separated token, then rank exact and prefix matches
/// ahead of metadata-only matches. Equal scores keep the catalog order.
pub fn select_quick_launcher_items<'a, T: LauncherItem>(
    items: &'a [T],
    query: &str,
    limit: usize,
) -> Vec<&'a T> {
    let normalized = normalize(query);
    let tokens: Vec<&str> = normalized
        .split(is_js_whitespace)
        .filter(|token| !token.is_empty())
        .collect();
    // One pass: scoring and filtering together, keeping only what survives.
    let mut matches: Vec<(&T, usize, Option<i32>)> = Vec::new();
    for (index, entry) in items.iter().enumerate() {
        let item = entry.item();
        if normalized.is_empty() {
            if item.show_when_empty {
                matches.push((entry, index, None));
            }
            continue;
        }
        if let Some(score) = score_item(item, &normalized, &tokens) {
            matches.push((entry, index, Some(score)));
        }
    }
    matches.sort_by(|left, right| {
        let left_score = left.2.unwrap_or(left.0.item().priority);
        let right_score = right.2.unwrap_or(right.0.item().priority);
        right_score
            .cmp(&left_score)
            .then_with(|| left.1.cmp(&right.1))
    });
    matches.truncate(limit);
    matches.into_iter().map(|(entry, _, _)| entry).collect()
}

fn score_item(item: &QuickLauncherItem, query: &str, tokens: &[&str]) -> Option<i32> {
    if query.is_empty() {
        return None;
    }
    let text = item.search_text();
    if !tokens.iter().all(|token| text.searchable.contains(token)) {
        return None;
    }
    let label = text.label.as_str();
    let relevance = if label == query {
        1_000
    } else if label.starts_with(query) {
        800
    } else if text.words.iter().any(|word| word.starts_with(query)) {
        650
    } else if label.contains(query) {
        500
    } else if tokens.iter().all(|token| label.contains(token)) {
        350
    } else {
        100
    };
    Some(relevance + item.priority)
}

/// `value.trim().toLocaleLowerCase()`.
pub fn normalize(value: &str) -> String {
    js_trim(value).to_lowercase()
}

fn split_words(label: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_space = false;
    for c in label.chars() {
        if is_js_whitespace(c) {
            if !in_space {
                words.push(std::mem::take(&mut current));
            }
            in_space = true;
        } else {
            in_space = false;
            current.push(c);
        }
    }
    words.push(current);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, label: &str) -> QuickLauncherItem {
        QuickLauncherItem::new(id, label, "")
    }

    fn ids(results: Vec<&QuickLauncherItem>) -> Vec<&str> {
        results.into_iter().map(|item| item.id.as_str()).collect()
    }

    #[test]
    fn shows_only_intentional_initial_suggestions_ordered_by_priority() {
        let items = [
            item("hidden", "Hidden").priority(100),
            item("recent", "Recent").priority(20).show_when_empty(true),
            item("live", "Live").priority(200).show_when_empty(true),
        ];
        assert_eq!(
            ids(select_quick_launcher_items(&items, "", RESULT_LIMIT)),
            ["live", "recent"]
        );
    }

    #[test]
    fn matches_every_query_token_across_labels_details_and_keywords() {
        let items = [
            QuickLauncherItem::new(
                "prod",
                "Core router",
                "Production · admin@router.example.test",
            )
            .keywords(["ssh"]),
            QuickLauncherItem::new("lab", "Lab router", "Lab · operator@router.example.test")
                .keywords(["ssh"]),
        ];
        assert_eq!(
            ids(select_quick_launcher_items(
                &items,
                "prod ssh",
                RESULT_LIMIT
            )),
            ["prod"]
        );
    }

    #[test]
    fn ranks_exact_and_prefix_label_matches_above_metadata_only_matches() {
        let items = [
            QuickLauncherItem::new("metadata", "Production database", "Alias: core").priority(100),
            item("prefix", "Core router"),
            item("exact", "core"),
        ];
        assert_eq!(
            ids(select_quick_launcher_items(&items, "core", RESULT_LIMIT)),
            ["exact", "prefix", "metadata"]
        );
    }

    #[test]
    fn preserves_stable_order_for_equally_ranked_results_and_applies_the_limit() {
        let items = [
            item("one", "One match"),
            item("two", "Two match"),
            item("three", "Three match"),
        ];
        assert_eq!(
            ids(select_quick_launcher_items(&items, "match", 2)),
            ["one", "two"]
        );
    }

    #[test]
    fn ranks_word_prefixes_between_label_prefixes_and_substrings() {
        let items = [
            item("inside", "bweb"),
            item("word", "my web"),
            item("prefix", "web-01"),
        ];
        assert_eq!(
            ids(select_quick_launcher_items(&items, "web", RESULT_LIMIT)),
            ["prefix", "word", "inside"]
        );
    }
}
