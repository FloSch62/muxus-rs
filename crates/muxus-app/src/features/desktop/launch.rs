//! Turning a command-line launch into what the app does (`CommandLineLaunchHandler.tsx`): load
//! the catalogs the request needs from the backend, resolve it with
//! `muxus_import::launch::plan_command_line_launch`, and hand back an owned plan the window
//! performs, with the toast to show.

use muxus_core::api::{CommandLineLaunch, ManagedHostRef, WorkspaceSummary};
use muxus_core::protocol::SessionProfile;
use muxus_import::launch::{
    HOST_CATALOG_ERROR, LaunchAction, LaunchCatalogs, needs_host_catalog, plan_command_line_launch,
};
use muxus_session::Backend;

/// A host from either catalog, as the sidebar names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchHost {
    pub host: ManagedHostRef,
    pub name: String,
    pub color: Option<String>,
}

/// What a launch request does once resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedAction {
    /// Dial `[user@]host[:port]` like `ssh <target>`, so ssh_config still supplies keys and
    /// jumps (`connectTarget`).
    ConnectTarget { target: String },
    /// Open a session profile in a new tab (`openSessionProfile`): links.
    OpenProfile {
        profile: Box<SessionProfile>,
        title: String,
        color: Option<String>,
    },
    /// Connect a saved or ssh_config host the usual way (`connectManagedHost`).
    ConnectHost { host: LaunchHost },
    /// Open every host of a folder in tabs (`launchManagedHostGroup(hosts, 'tabs')`), then
    /// show `folder_launch_message(label, started)`.
    LaunchFolder {
        label: String,
        hosts: Vec<LaunchHost>,
    },
    /// Show a workspace: say it is already open, focus the window showing it, or open it in a
    /// new window, then show `workspace_message(name, outcome)`.
    OpenWorkspace { workspace: WorkspaceSummary },
    /// Nothing to do; the toast says why.
    Fail,
}

/// The plan and its toast.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedLaunch {
    pub action: PlannedAction,
    /// Shown as an error toast for [`PlannedAction::Fail`], as info otherwise.
    pub toast: Option<String>,
}

fn launch_host(host: &muxus_import::launch::ManagedHost<'_>) -> LaunchHost {
    LaunchHost {
        host: host.to_ref(),
        name: host.display_name().to_string(),
        color: host.color().map(str::to_string),
    }
}

/// Resolve one request against the backend's catalogs.
pub async fn plan_launch(backend: &Backend, request: &CommandLineLaunch) -> PlannedLaunch {
    let (ssh, profiles) = if needs_host_catalog(request) {
        match (backend.ssh_config().await, backend.saved_hosts().await) {
            (Ok(ssh), Ok(profiles)) => (Some(ssh), profiles.profiles),
            _ => {
                return PlannedLaunch {
                    action: PlannedAction::Fail,
                    toast: Some(HOST_CATALOG_ERROR.into()),
                };
            }
        }
    } else {
        (None, Vec::new())
    };
    let workspaces = if matches!(request, CommandLineLaunch::Workspace { .. }) {
        backend.workspaces().await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let catalogs = LaunchCatalogs {
        ssh_hosts: ssh.as_ref().map_or(&[], |ssh| ssh.hosts.as_slice()),
        saved_profiles: &profiles,
        files: ssh.as_ref().map_or(&[], |ssh| ssh.files.as_slice()),
        root_file: ssh.as_ref().map(|ssh| ssh.path.as_str()),
        workspaces: &workspaces,
    };
    let (action, toast) = plan_command_line_launch(request, catalogs);
    let action = match action {
        LaunchAction::ConnectTarget { target } => PlannedAction::ConnectTarget { target },
        LaunchAction::OpenProfile {
            profile,
            title,
            color,
        } => PlannedAction::OpenProfile {
            profile: Box::new(profile),
            title,
            color,
        },
        LaunchAction::ConnectHost { host } => PlannedAction::ConnectHost {
            host: launch_host(&host),
        },
        LaunchAction::LaunchFolder { folder } => PlannedAction::LaunchFolder {
            label: folder.label.clone(),
            hosts: folder.hosts.iter().map(launch_host).collect(),
        },
        LaunchAction::OpenWorkspace { workspace } => PlannedAction::OpenWorkspace {
            workspace: workspace.clone(),
        },
        LaunchAction::Fail => PlannedAction::Fail,
    };
    PlannedLaunch { action, toast }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use muxus_session::BackendOptions;

    #[test]
    fn plans_launches_against_the_backend_catalogs() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
        std::fs::write(
            home.path().join(".ssh").join("config"),
            "Host web\n  HostName web.example\n  User deploy\n\nHost db\n  HostName db.example\n",
        )
        .unwrap();
        let backend =
            Backend::start_blocking(BackendOptions::sandbox(data.path(), home.path())).unwrap();

        let host = block_on(plan_launch(
            &backend,
            &CommandLineLaunch::Host { name: "web".into() },
        ));
        assert_eq!(
            host.action,
            PlannedAction::ConnectHost {
                host: LaunchHost {
                    host: ManagedHostRef::Ssh {
                        alias: "web".into()
                    },
                    name: "web".into(),
                    color: None,
                }
            }
        );
        assert_eq!(host.toast.as_deref(), Some("Connecting to “web”."));

        let missing = block_on(plan_launch(
            &backend,
            &CommandLineLaunch::Host {
                name: "nowhere".into(),
            },
        ));
        assert_eq!(missing.action, PlannedAction::Fail);
        assert_eq!(
            missing.toast.as_deref(),
            Some("No host named “nowhere” was found.")
        );

        let link = block_on(plan_launch(
            &backend,
            &CommandLineLaunch::Url {
                name: "ssh://admin@web.example:2222".into(),
            },
        ));
        assert!(matches!(link.action, PlannedAction::OpenProfile { .. }));

        let connect = block_on(plan_launch(
            &backend,
            &CommandLineLaunch::Connect {
                name: "root@10.0.0.1".into(),
            },
        ));
        assert_eq!(
            connect.action,
            PlannedAction::ConnectTarget {
                target: "root@10.0.0.1".into()
            }
        );

        let workspace = block_on(plan_launch(
            &backend,
            &CommandLineLaunch::Workspace { name: "Lab".into() },
        ));
        assert_eq!(workspace.action, PlannedAction::Fail);
        block_on(backend.shutdown());
    }
}
