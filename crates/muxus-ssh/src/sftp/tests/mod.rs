//! SFTP operations end to end: the service on a real connection to the in-process SSH server,
//! whose SFTP subsystem serves a scratch directory.

mod lease_socket;
mod routes;
mod upload;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::service::SftpService;
use crate::connection::leases::ConnectionLeaseOwner;
use crate::connection::manager::{
    ConnectOptions, ManagerOptions, MuxedConnectionLease, SshConnectionManager,
};
use crate::connection::profile::SshProfile;
use crate::connection::tests::{Sandbox, TestIo};
use crate::testing::server::{ServerSpec, TestServer, start};
use crate::testing::sftp_server::{self, SftpServerOptions, SftpServerState};

const PASSWORD: &str = "secret";

/// A terminal's connection to the test server, with SFTP served from `root`.
pub(crate) struct Fixture {
    pub root: tempfile::TempDir,
    pub sftp: Arc<SftpServerState>,
    pub manager: SshConnectionManager,
    pub service: SftpService,
    pub terminal: Option<MuxedConnectionLease>,
    pub conn_id: String,
    _sandbox: Sandbox,
    pub server: TestServer,
}

impl Fixture {
    pub async fn new() -> Self {
        Self::with(SftpServerOptions::default(), |_| {}).await
    }

    pub async fn with(options: SftpServerOptions, spec: impl FnOnce(&mut ServerSpec)) -> Self {
        Self::build(options, spec, |_| {}).await
    }

    pub async fn build(
        options: SftpServerOptions,
        spec: impl FnOnce(&mut ServerSpec),
        manager_options: impl FnOnce(&mut ManagerOptions),
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let sftp = sftp_server::register(root.path(), options);
        let mut server_spec = ServerSpec::new().password(PASSWORD);
        server_spec.sftp_root = Some(root.path().to_path_buf());
        spec(&mut server_spec);
        let server = start(server_spec).await;
        let sandbox = Sandbox::new();
        let port = format!("  Port {}", server.port());
        sandbox.write_config(&[
            "Host lab",
            "  HostName 127.0.0.1",
            "  User tester",
            &port,
            "  StrictHostKeyChecking accept-new",
        ]);
        let mut options = sandbox.options();
        manager_options(&mut options);
        let manager = SshConnectionManager::new(options);
        let terminal = manager
            .connect(
                &SshProfile::new("lab"),
                TestIo::accepting(PASSWORD),
                ConnectionLeaseOwner::Terminal,
                ConnectOptions::default(),
            )
            .await
            .unwrap();
        let conn_id = terminal.connection().id.clone();
        Self {
            root,
            sftp,
            service: SftpService::new(manager.clone()),
            manager,
            terminal: Some(terminal),
            conn_id,
            _sandbox: sandbox,
            server,
        }
    }

    /// The local file behind a remote path.
    pub fn local(&self, remote: &str) -> PathBuf {
        self.root.path().join(remote.trim_start_matches('/'))
    }

    pub fn write(&self, remote: &str, content: &str) -> PathBuf {
        let path = self.local(remote);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    pub fn read(&self, remote: &str) -> String {
        std::fs::read_to_string(self.local(remote)).unwrap()
    }

    pub fn mkdir(&self, remote: &str) {
        std::fs::create_dir_all(self.local(remote)).unwrap();
    }

    pub fn exists(&self, remote: &str) -> bool {
        std::fs::symlink_metadata(self.local(remote)).is_ok()
    }

    pub fn ops(&self) -> Vec<String> {
        self.sftp.ops()
    }

    /// Requests whose log line starts with `prefix`.
    pub fn ops_starting(&self, prefix: &str) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter(|op| op.starts_with(prefix))
            .collect()
    }

    /// Live SFTP leases on the fixture's connection.
    pub fn sftp_leases(&self) -> usize {
        self.manager
            .lease_count(&self.conn_id, Some(&[ConnectionLeaseOwner::Sftp]))
    }
}

/// Sets a local file's modification time (seconds since the epoch).
pub(crate) fn set_mtime(path: &Path, seconds: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

pub(crate) fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// `<base>.muxus-<12 hex>.<suffix>`: the staged temporary file of `base`.
pub(crate) fn is_staged(path: &str, base: &str, suffix: &str) -> bool {
    let Some(rest) = path.strip_prefix(&format!("{base}.muxus-")) else {
        return false;
    };
    let Some(hex) = rest.strip_suffix(&format!(".{suffix}")) else {
        return false;
    };
    hex.len() == 12
        && hex
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Waits until `condition` holds (a background task finishing up), for at most five seconds.
pub(crate) async fn eventually(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..500 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}
