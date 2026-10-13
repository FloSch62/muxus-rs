//! Sidebar host tree logic, free of UI types: click rules, row windowing,
//! keyboard navigation, multi-selection, drag and drop, folder edits, folder
//! preferences and the per-row lookups (icons, hover card lines, live dots).
//! Ported from `client/src/components/sidebar/*.ts` and the pure parts of the
//! hooks next to them.

pub mod folder_icons;
pub mod folder_mutations;
pub mod folder_prefs;
pub mod host_details;
pub mod kind_icon;
pub mod live_counts;
pub mod row_clicks;
pub mod row_style;
pub mod tree_dnd;
pub mod tree_navigation;
pub mod tree_selection;
pub mod tree_window;
