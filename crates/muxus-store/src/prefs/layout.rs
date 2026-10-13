//! Small presentation preferences and their bounds: status bar items,
//! sidebar and file browser widths, interface zoom, pane dimming and the
//! terminal font stack (from `client/src/host-stats.ts`,
//! `client/src/sidebar-width.ts`, `client/src/sftp-panel-width.ts`,
//! `client/src/interface-zoom.ts` and `client/src/state/prefs.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use muxus_core::js;

/// Status bar items, in the order the bar shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusBarItem {
    Hostname,
    Os,
    Cpu,
    Memory,
    Disk,
    Network,
    Uptime,
    Users,
}

pub const STATUS_BAR_ITEMS: [StatusBarItem; 8] = [
    StatusBarItem::Hostname,
    StatusBarItem::Os,
    StatusBarItem::Cpu,
    StatusBarItem::Memory,
    StatusBarItem::Disk,
    StatusBarItem::Network,
    StatusBarItem::Uptime,
    StatusBarItem::Users,
];

impl StatusBarItem {
    pub fn id(self) -> &'static str {
        match self {
            StatusBarItem::Hostname => "hostname",
            StatusBarItem::Os => "os",
            StatusBarItem::Cpu => "cpu",
            StatusBarItem::Memory => "memory",
            StatusBarItem::Disk => "disk",
            StatusBarItem::Network => "network",
            StatusBarItem::Uptime => "uptime",
            StatusBarItem::Users => "users",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        STATUS_BAR_ITEMS.into_iter().find(|item| item.id() == value)
    }

    /// `STATUS_BAR_ITEM_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            StatusBarItem::Hostname => "Host name",
            StatusBarItem::Os => "Operating system",
            StatusBarItem::Cpu => "CPU usage",
            StatusBarItem::Memory => "Memory usage",
            StatusBarItem::Disk => "Disk usage",
            StatusBarItem::Network => "Network traffic",
            StatusBarItem::Uptime => "Uptime",
            StatusBarItem::Users => "Logged-in users",
        }
    }
}

/// `isStatusBarItemList`: known items, each at most once.
pub fn is_status_bar_item_list(value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    items.iter().all(|item| {
        item.as_str()
            .and_then(StatusBarItem::parse)
            .is_some_and(|item| seen.insert(item))
    })
}

/// Turn an item on or off, keeping the bar's order.
pub fn with_status_bar_item(
    items: &[StatusBarItem],
    item: StatusBarItem,
    shown: bool,
) -> Vec<StatusBarItem> {
    STATUS_BAR_ITEMS
        .into_iter()
        .filter(|entry| {
            if *entry == item {
                shown
            } else {
                items.contains(entry)
            }
        })
        .collect()
}

/// The search box is what sets this floor: it has to hold `user@host`
/// without eliding the placeholder that says so, and still leave room for Add host.
pub const DEFAULT_SIDEBAR_WIDTH: f64 = 288.0;
/// Host names still fit here — long addresses fall back to the row hover card.
pub const MIN_SIDEBAR_WIDTH: f64 = 200.0;
pub const MAX_SIDEBAR_WIDTH: f64 = 520.0;
pub const MIN_MAIN_WORKSPACE_WIDTH: f64 = 320.0;

/// What `DEFAULT_SIDEBAR_WIDTH` was before preferences version 6.
pub const PREVIOUS_DEFAULT_SIDEBAR_WIDTH: f64 = 248.0;

pub fn max_sidebar_width(container_width: f64) -> f64 {
    MIN_SIDEBAR_WIDTH.max(
        MAX_SIDEBAR_WIDTH
            .min(container_width * 0.45)
            .min(container_width - MIN_MAIN_WORKSPACE_WIDTH)
            .floor(),
    )
}

pub fn clamp_sidebar_width(width: f64, container_width: f64) -> f64 {
    MIN_SIDEBAR_WIDTH.max(max_sidebar_width(container_width).min(js_round(width)))
}

pub const DEFAULT_SFTP_PANEL_WIDTH: f64 = 380.0;
pub const MIN_SFTP_PANEL_WIDTH: f64 = 280.0;
pub const MIN_SFTP_WORKSPACE_WIDTH: f64 = 240.0;

/// Keep the browser useful without letting it take more than 70% of its pane
/// or squeeze the terminal/editor below its practical minimum.
pub fn max_sftp_panel_width(container_width: f64) -> f64 {
    MIN_SFTP_PANEL_WIDTH.max(
        (container_width * 0.7)
            .min(container_width - MIN_SFTP_WORKSPACE_WIDTH)
            .floor(),
    )
}

pub fn clamp_sftp_panel_width(width: f64, container_width: f64) -> f64 {
    MIN_SFTP_PANEL_WIDTH.max(max_sftp_panel_width(container_width).min(js_round(width)))
}

/// `Math.round`: halves round up, toward positive infinity.
pub fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// Window scale steps offered in Settings.
pub const INTERFACE_ZOOM_STEPS: [f64; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
pub const MIN_INTERFACE_ZOOM: f64 = 0.5;
pub const MAX_INTERFACE_ZOOM: f64 = 2.0;

pub fn clamp_interface_zoom(zoom: f64) -> f64 {
    if !zoom.is_finite() {
        return 1.0;
    }
    zoom.clamp(MIN_INTERFACE_ZOOM, MAX_INTERFACE_ZOOM)
}

pub fn interface_zoom_label(zoom: f64) -> String {
    format!("{}%", js_round(clamp_interface_zoom(zoom) * 100.0))
}

pub const DEFAULT_INACTIVE_PANE_DIM_STRENGTH: f64 = 0.15;
pub const MIN_INACTIVE_PANE_DIM_STRENGTH: f64 = 0.1;
pub const MAX_INACTIVE_PANE_DIM_STRENGTH: f64 = 0.6;

/// Keep hand-edited or older persisted values from making a pane illegible.
pub fn clamp_inactive_pane_dim_strength(value: f64) -> f64 {
    if !value.is_finite() {
        return DEFAULT_INACTIVE_PANE_DIM_STRENGTH;
    }
    value.clamp(
        MIN_INACTIVE_PANE_DIM_STRENGTH,
        MAX_INACTIVE_PANE_DIM_STRENGTH,
    )
}

/// Presentation-only opacity for one pane; terminal buffers remain untouched.
pub fn pane_focus_opacity(emphasized: bool, dim_inactive_panes: bool, dim_strength: f64) -> f64 {
    if emphasized || !dim_inactive_panes {
        1.0
    } else {
        1.0 - clamp_inactive_pane_dim_strength(dim_strength)
    }
}

/// Bundled icon-only fallback covering Nerd Font and Powerline glyphs.
pub const TERMINAL_SYMBOL_FONT: &str = "\"Pure Nerd Font\"";

/// Fallback stack appended after the user's chosen family.
pub const MONO_FONT_FALLBACK: &str = "\"JetBrains Mono\", \"Pure Nerd Font\", \"Noto Sans Mono\", \"DejaVu Sans Mono\", \"Liberation Mono\", monospace";

/// CSS-style font-family stack for the terminal given the `fontFamily` preference.
pub fn terminal_font_stack(family: &str) -> String {
    let trimmed = js::js_trim(family);
    if trimmed.is_empty() {
        return MONO_FONT_FALLBACK.to_owned();
    }
    let quoted = if trimmed.contains(' ') && !trimmed.starts_with(['"', '\'']) {
        format!("\"{trimmed}\"")
    } else {
        trimmed.to_owned()
    };
    if quoted == "\"JetBrains Mono\"" || quoted == "monospace" {
        MONO_FONT_FALLBACK.to_owned()
    } else {
        format!("{quoted}, {MONO_FONT_FALLBACK}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_zoom_stays_in_a_usable_range() {
        assert_eq!(clamp_interface_zoom(1.0), 1.0);
        assert_eq!(clamp_interface_zoom(0.1), MIN_INTERFACE_ZOOM);
        assert_eq!(clamp_interface_zoom(9.0), MAX_INTERFACE_ZOOM);
        assert_eq!(clamp_interface_zoom(f64::NAN), 1.0);
        assert!(INTERFACE_ZOOM_STEPS.contains(&1.0));
        assert!(
            INTERFACE_ZOOM_STEPS
                .iter()
                .all(|step| clamp_interface_zoom(*step) == *step)
        );
        assert_eq!(interface_zoom_label(1.25), "125%");
    }

    #[test]
    fn widths_clamp_to_their_containers() {
        assert_eq!(max_sidebar_width(1000.0), 450.0);
        assert_eq!(clamp_sidebar_width(100.0, 1000.0), MIN_SIDEBAR_WIDTH);
        assert_eq!(clamp_sidebar_width(300.4, 1000.0), 300.0);
        assert_eq!(max_sftp_panel_width(1000.0), 700.0);
        assert_eq!(clamp_sftp_panel_width(900.0, 1000.0), 700.0);
    }

    #[test]
    fn status_bar_items_keep_bar_order() {
        assert_eq!(
            with_status_bar_item(&[StatusBarItem::Disk], StatusBarItem::Cpu, true),
            vec![StatusBarItem::Cpu, StatusBarItem::Disk]
        );
        assert_eq!(
            with_status_bar_item(&[StatusBarItem::Disk], StatusBarItem::Disk, false),
            vec![]
        );
    }
}
