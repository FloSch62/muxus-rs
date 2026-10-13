//! Command-line launches: parsing the desktop executable's arguments and
//! resolving what they name. The ssh:// / telnet:// link grammar itself is
//! `muxus_core::connection_links`.

mod command_line;
mod host_tree;
mod resolve;

pub use command_line::{
    ConnectOptions, can_handle_command_line_launch, link_launch, parse_command_line_launch,
    parse_command_line_launch_data, parse_connect_target,
};
pub use host_tree::ManagedHost;
pub use resolve::{
    CommandLineTargetKind, ConnectionLinkLaunch, HOST_CATALOG_ERROR, LAUNCH_FAILED, LaunchAction,
    LaunchCatalogs, ResolvedHostFolder, TargetResolution, WorkspaceLaunchOutcome,
    folder_launch_message, link_label, needs_host_catalog, plan_command_line_launch,
    resolution_error, resolve_command_line_folder, resolve_command_line_host,
    resolve_command_line_workspace, resolve_connection_link, workspace_message,
};
