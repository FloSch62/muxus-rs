//! Folder preferences: collapse state, colours and icons, manual sibling order
//! and folders that hold no host yet. The pure logic of
//! `client/src/components/sidebar/useFolderPrefs.ts`, over the four prefs
//! fields it reads (`sidebarCollapsedFolders`, `sidebarFolderStyles`,
//! `sidebarFolderOrder`, `sidebarEmptyFolders`).

use std::collections::BTreeSet;

use indexmap::IndexMap;

use crate::hosts::tree::{
    folder_key, is_descendant_path, is_same_path, normalize_group_path, rename_folder_path,
};
use crate::model::FolderStyle;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderPrefs {
    /// `sidebarCollapsedFolders`: collapsed folder keys, in the order they were stored.
    pub collapsed: Vec<String>,
    /// `sidebarFolderStyles`, by folder key.
    pub styles: IndexMap<String, FolderStyle>,
    /// `sidebarFolderOrder`: sibling folder keys under a parent key (`TREE_ROOT_KEY` for the top level).
    pub order: IndexMap<String, Vec<String>>,
    /// `sidebarEmptyFolders`: folder paths that exist without a host.
    pub empty_folders: Vec<String>,
}

impl FolderPrefs {
    pub fn with_styles(styles: IndexMap<String, FolderStyle>) -> Self {
        FolderPrefs {
            styles,
            ..Default::default()
        }
    }

    /// A folder absent from the collapsed set is expanded, so new ones open.
    pub fn is_expanded(&self, key: &str) -> bool {
        !self.collapsed.iter().any(|entry| entry == key)
    }

    pub fn collapsed_keys(&self) -> BTreeSet<String> {
        self.collapsed.iter().cloned().collect()
    }

    pub fn set_collapsed(&mut self, key: &str, collapsed: bool) {
        let has = self.collapsed.iter().any(|entry| entry == key);
        if has == collapsed {
            return;
        }
        if collapsed {
            self.collapsed.push(key.to_string());
        } else {
            self.collapsed.retain(|entry| entry != key);
        }
    }

    /// Collapse or expand several folders in one write (`setCollapsedKeys` in SessionSidebar).
    pub fn set_collapsed_keys<'a>(
        &mut self,
        keys: impl IntoIterator<Item = &'a str>,
        collapsed: bool,
    ) {
        for key in keys {
            self.set_collapsed(key, collapsed);
        }
    }

    pub fn toggle_folder(&mut self, key: &str) {
        let collapsed = self.is_expanded(key);
        self.set_collapsed(key, collapsed);
    }

    pub fn folder_style(&self, key: &str) -> Option<&FolderStyle> {
        self.styles.get(key)
    }

    pub fn folder_color(&self, key: &str) -> Option<String> {
        self.styles.get(key).and_then(|style| style.color.clone())
    }

    pub fn folder_icon(&self, key: &str) -> Option<&str> {
        self.styles.get(key).and_then(|style| style.icon.as_deref())
    }

    /// A style with neither colour nor icon removes the entry.
    pub fn set_folder_style(&mut self, key: &str, style: Option<FolderStyle>) {
        match style {
            Some(style) if style.color.is_some() || style.icon.is_some() => {
                self.styles.insert(key.to_string(), style);
            }
            _ => {
                self.styles.shift_remove(key);
            }
        }
    }

    /// Manual sibling order under a parent key, or `None` for alphabetical.
    pub fn folder_order(&self, parent_key: &str) -> Option<Vec<String>> {
        self.order.get(parent_key).cloned()
    }

    pub fn set_folder_order(&mut self, parent_key: &str, keys: &[String]) {
        self.order.insert(parent_key.to_string(), keys.to_vec());
    }

    pub fn add_empty_folder(&mut self, path: &str) {
        let normalized = normalize_group_path(Some(path));
        if normalized.is_empty()
            || self
                .empty_folders
                .iter()
                .any(|entry| is_same_path(entry, &normalized))
        {
            return;
        }
        self.empty_folders.push(normalized);
    }

    pub fn remove_empty_folder(&mut self, path: &str) {
        self.empty_folders
            .retain(|entry| !is_same_path(entry, path) && !is_descendant_path(entry, path));
    }

    /// Carry collapse state, colours, order and empty markers across a folder
    /// rename. Rewrites the folder itself and everything under it; running this
    /// before the host writes keeps a renamed folder's colour and expansion in
    /// place while the host list reflows underneath it.
    pub fn rename_folder_prefs(&mut self, from: &str, to: &str) {
        let target = normalize_group_path(Some(to));
        // A folder key is its lowercased path, so descendants can be rewritten in
        // key space. The trailing separator keeps this segment-safe: the key for
        // "Production" does not start with "folder:prod/".
        let from_key = folder_key(from);
        let to_key = folder_key(&target);
        let remap = |key: &str| -> String {
            if key == from_key {
                to_key.clone()
            } else if let Some(rest) = key.strip_prefix(&format!("{from_key}/")) {
                format!("{to_key}/{rest}")
            } else {
                key.to_string()
            }
        };
        let mut seen = BTreeSet::new();
        self.collapsed = self
            .collapsed
            .iter()
            .map(|key| remap(key))
            .filter(|key| seen.insert(key.clone()))
            .collect();
        self.styles = std::mem::take(&mut self.styles)
            .into_iter()
            .map(|(key, style): (String, FolderStyle)| (remap(&key), style))
            .collect();
        // Both sides of the order map hold folder keys: the parent it is stored
        // under, and every sibling listed inside it.
        self.order = std::mem::take(&mut self.order)
            .into_iter()
            .map(|(parent, keys): (String, Vec<String>)| {
                (remap(&parent), keys.iter().map(|key| remap(key)).collect())
            })
            .collect();
        let mut seen = BTreeSet::new();
        self.empty_folders = self
            .empty_folders
            .iter()
            .map(|path| rename_folder_path(path, from, &target).unwrap_or_else(|| path.clone()))
            .filter(|path| seen.insert(path.clone()))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(color: &str) -> FolderStyle {
        FolderStyle {
            color: Some(color.into()),
            icon: None,
        }
    }

    #[test]
    fn expands_folders_unless_collapsed() {
        let mut prefs = FolderPrefs::default();
        assert!(prefs.is_expanded("folder:a"));
        prefs.toggle_folder("folder:a");
        assert!(!prefs.is_expanded("folder:a"));
        prefs.set_collapsed("folder:a", true);
        assert_eq!(prefs.collapsed, ["folder:a"]);
        prefs.toggle_folder("folder:a");
        assert!(prefs.is_expanded("folder:a"));
    }

    #[test]
    fn drops_a_style_with_nothing_in_it() {
        let mut prefs = FolderPrefs::default();
        prefs.set_folder_style("folder:a", Some(style("#fff")));
        assert_eq!(prefs.folder_color("folder:a").as_deref(), Some("#fff"));
        prefs.set_folder_style("folder:a", Some(FolderStyle::default()));
        assert_eq!(prefs.folder_style("folder:a"), None);
    }

    #[test]
    fn remembers_empty_folders_once_and_forgets_their_subtree() {
        let mut prefs = FolderPrefs::default();
        prefs.add_empty_folder(" Staging / Blue ");
        prefs.add_empty_folder("staging/blue");
        prefs.add_empty_folder("Lab");
        assert_eq!(prefs.empty_folders, ["Staging/Blue", "Lab"]);
        prefs.remove_empty_folder("Staging");
        assert_eq!(prefs.empty_folders, ["Lab"]);
    }

    #[test]
    fn carries_everything_across_a_rename_segment_safely() {
        let mut prefs = FolderPrefs {
            collapsed: vec![
                "folder:prod".into(),
                "folder:prod/eu".into(),
                "folder:production".into(),
            ],
            styles: [
                ("folder:prod/eu".to_string(), style("#f00")),
                ("folder:production".to_string(), style("#0f0")),
            ]
            .into_iter()
            .collect(),
            order: [(
                "folder:prod".to_string(),
                vec!["folder:prod/eu".to_string(), "folder:prod/us".to_string()],
            )]
            .into_iter()
            .collect(),
            empty_folders: vec!["Prod/Empty".into(), "Production".into()],
        };
        prefs.rename_folder_prefs("Prod", "Live");
        assert_eq!(
            prefs.collapsed,
            ["folder:live", "folder:live/eu", "folder:production"]
        );
        assert_eq!(
            prefs.folder_color("folder:live/eu").as_deref(),
            Some("#f00")
        );
        assert_eq!(
            prefs.folder_color("folder:production").as_deref(),
            Some("#0f0")
        );
        assert_eq!(
            prefs.folder_order("folder:live"),
            Some(vec![
                "folder:live/eu".to_string(),
                "folder:live/us".to_string()
            ])
        );
        assert_eq!(prefs.empty_folders, ["Live/Empty", "Production"]);
    }
}
