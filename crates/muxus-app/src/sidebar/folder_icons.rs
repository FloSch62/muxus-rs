//! The icons a folder can wear. Port of `client/src/components/sidebar/folder-icons.ts`.
//!
//! Icons are named after the `@mui/icons-material` components the TypeScript
//! imports (`FolderOutlined` for `FolderOutlinedIcon`), so the UI can map each
//! name to its SVG asset.

/// One choice in the folder editor's icon picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderIcon {
    pub id: &'static str,
    pub label: &'static str,
    /// MUI icon component name, without the `Icon` suffix.
    pub icon: &'static str,
}

/// A deliberately short list: curated rather than exposing the whole icon set.
pub const FOLDER_ICONS: [FolderIcon; 11] = [
    FolderIcon {
        id: "folder",
        label: "Folder",
        icon: "FolderOutlined",
    },
    FolderIcon {
        id: "cloud",
        label: "Cloud",
        icon: "CloudOutlined",
    },
    FolderIcon {
        id: "server",
        label: "Servers",
        icon: "DnsOutlined",
    },
    FolderIcon {
        id: "storage",
        label: "Storage",
        icon: "StorageOutlined",
    },
    FolderIcon {
        id: "router",
        label: "Network",
        icon: "RouterOutlined",
    },
    FolderIcon {
        id: "lan",
        label: "LAN",
        icon: "LanOutlined",
    },
    FolderIcon {
        id: "hub",
        label: "Cluster",
        icon: "HubOutlined",
    },
    FolderIcon {
        id: "public",
        label: "Public",
        icon: "PublicOutlined",
    },
    FolderIcon {
        id: "lock",
        label: "Secure",
        icon: "LockOutlined",
    },
    FolderIcon {
        id: "lab",
        label: "Lab",
        icon: "ScienceOutlined",
    },
    FolderIcon {
        id: "site",
        label: "Site",
        icon: "WarehouseOutlined",
    },
];

pub fn is_folder_icon_id(id: &str) -> bool {
    FOLDER_ICONS.iter().any(|entry| entry.id == id)
}

/// The plain folder falls back to open/closed art; named icons do not. A folder that
/// follows a shared sessions link has its own art, unless the user picked an icon for it.
pub fn folder_icon_name(id: Option<&str>, expanded: bool, shared: bool) -> &'static str {
    match id {
        None | Some("folder") => {
            if shared {
                "FolderSharedOutlined"
            } else if expanded {
                "FolderOpenOutlined"
            } else {
                "FolderOutlined"
            }
        }
        Some(id) => FOLDER_ICONS
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.icon)
            .unwrap_or("FolderOutlined"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_plain_folder_and_keeps_named_icons() {
        assert_eq!(folder_icon_name(None, true, false), "FolderOpenOutlined");
        assert_eq!(
            folder_icon_name(Some("folder"), false, false),
            "FolderOutlined"
        );
        assert_eq!(
            folder_icon_name(Some("cloud"), true, false),
            "CloudOutlined"
        );
        assert_eq!(
            folder_icon_name(Some("unknown"), true, false),
            "FolderOutlined"
        );
        // A folder that follows a link says so, open or closed, unless it has its own icon.
        assert_eq!(folder_icon_name(None, true, true), "FolderSharedOutlined");
        assert_eq!(
            folder_icon_name(Some("folder"), false, true),
            "FolderSharedOutlined"
        );
        assert_eq!(
            folder_icon_name(Some("cloud"), false, true),
            "CloudOutlined"
        );
        assert!(is_folder_icon_id("lab"));
        assert!(!is_folder_icon_id("unknown"));
    }
}
