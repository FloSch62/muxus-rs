//! Containers, clusters, labs and VMs on the computer running Muxus and on the
//! far end of live SSH connections (`server/src/environments`). Each request
//! runs the host's own tools (docker, podman, kubectl, pvesh, virsh) over one
//! exec channel of that connection; nothing is installed.
//!
//! The routes are `Backend` methods in [`crate::routes::environments`].

pub mod discovery;
pub mod parse;
pub mod probe;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use muxus_core::environments::EnvironmentsResponse;
use tokio::sync::OnceCell;

/// Where a host's tools run: this computer, or the far end of a live SSH connection.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EnvironmentSource {
    Local,
    Ssh { conn_id: String },
}

impl EnvironmentSource {
    pub fn ssh(conn_id: impl Into<String>) -> Self {
        EnvironmentSource::Ssh {
            conn_id: conn_id.into(),
        }
    }

    fn key(&self) -> &str {
        match self {
            EnvironmentSource::Local => "local",
            EnvironmentSource::Ssh { conn_id } => conn_id,
        }
    }
}

type Reading = Arc<OnceCell<Result<EnvironmentsResponse, crate::ApiError>>>;

/// Discoveries in progress: windows asking about the same host at once share one reading
/// (one exec channel).
#[derive(Default)]
pub struct InFlight {
    readings: Mutex<HashMap<String, Reading>>,
}

impl InFlight {
    pub(crate) async fn read<F, Fut>(
        &self,
        source: &EnvironmentSource,
        auto: bool,
        read: F,
    ) -> Result<EnvironmentsResponse, crate::ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<EnvironmentsResponse, crate::ApiError>>,
    {
        let key = format!("{}:{}", source.key(), if auto { "1" } else { "" });
        let (cell, owner) = {
            let mut readings = self
                .readings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match readings.get(&key) {
                Some(cell) => (cell.clone(), false),
                None => {
                    let cell: Reading = Arc::default();
                    readings.insert(key.clone(), cell.clone());
                    (cell, true)
                }
            }
        };
        let result = cell.get_or_init(read).await.clone();
        if owner {
            self.readings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&key);
        }
        result
    }
}

/// Whether the Kubus desktop app takes `kubus://` links on this computer.
pub async fn kubus_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        let output = tokio::process::Command::new("xdg-mime")
            .args(["query", "default", "x-scheme-handler/kubus"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
        output.is_ok_and(|output| {
            output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        })
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        std::path::Path::new("/Applications/Kubus.app").exists()
            || home.is_some_and(|home| home.join("Applications/Kubus.app").exists())
    }
    #[cfg(windows)]
    {
        let output = tokio::process::Command::new("reg")
            .args(["query", r"HKCR\kubus", "/v", "URL Protocol"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        output.is_ok_and(|status| status.success())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        false
    }
}

/// `KUBUS_ROUTE`: only an in-app route travels, a path that cannot reach another origin
/// (`^/(?!/)[A-Za-z0-9/._~%|=&?:@+-]*$`).
pub fn kubus_url(route: &str) -> Option<String> {
    let rest = route.strip_prefix('/')?;
    if rest.starts_with('/') || route.len() > 2048 {
        return None;
    }
    rest.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._~%|=&?:@+-".contains(&b))
        .then(|| format!("kubus://{rest}"))
}

#[cfg(test)]
mod tests;
