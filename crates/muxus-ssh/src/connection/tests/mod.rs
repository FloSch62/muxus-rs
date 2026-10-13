//! End-to-end tests of the connection layer against the in-process SSH server.

mod algorithms;
mod auth;
mod mux;
mod session;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::io::{
    AuthPromptInfo, AuthPromptResponse, BoxFuture, Cancelled, ConnectIo, HostKeyChallenge,
    PasswordVault, VaultError, VaultStatus,
};
use super::manager::{ManagerOptions, SshConnectionManager};
use crate::config::load_config_document;
use crate::known_hosts::KnownHostsStore;
use crate::local_env::LocalEnv;

/// Records statuses and answers prompts from a script.
#[derive(Default)]
pub(crate) struct TestIo {
    pub statuses: Mutex<Vec<String>>,
    pub prompts: Mutex<Vec<AuthPromptInfo>>,
    pub answers: Mutex<VecDeque<AuthPromptResponse>>,
    /// Answer for prompts beyond the scripted ones.
    pub default_answer: Mutex<Option<String>>,
    pub host_keys: Mutex<Vec<HostKeyChallenge>>,
    pub accept_host_keys: Mutex<bool>,
    pub prompt_delay: Mutex<Option<Duration>>,
}

impl TestIo {
    pub fn accepting(password: &str) -> Arc<Self> {
        let io = Self::default();
        *io.default_answer.lock().unwrap() = Some(password.to_string());
        *io.accept_host_keys.lock().unwrap() = true;
        Arc::new(io)
    }

    pub fn statuses(&self) -> Vec<String> {
        self.statuses.lock().unwrap().clone()
    }

    pub fn prompt_count(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }
}

impl ConnectIo for TestIo {
    fn status(&self, message: &str, _transient: bool) {
        self.statuses.lock().unwrap().push(message.to_string());
    }

    fn prompt(&self, info: AuthPromptInfo) -> BoxFuture<'_, Result<AuthPromptResponse, Cancelled>> {
        Box::pin(async move {
            let delay = *self.prompt_delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let count = info.prompts.len();
            self.prompts.lock().unwrap().push(info);
            if let Some(answer) = self.answers.lock().unwrap().pop_front() {
                return Ok(answer);
            }
            match self.default_answer.lock().unwrap().clone() {
                Some(answer) => Ok(crate::connection::io::answers(vec![answer; count])),
                None => Err(Cancelled),
            }
        })
    }

    fn host_key(&self, challenge: HostKeyChallenge) -> BoxFuture<'_, Result<bool, Cancelled>> {
        Box::pin(async move {
            self.host_keys.lock().unwrap().push(challenge);
            Ok(*self.accept_host_keys.lock().unwrap())
        })
    }
}

/// An in-memory password vault.
#[derive(Default)]
pub(crate) struct MemoryVault {
    pub passwords: Mutex<HashMap<String, String>>,
    pub configured: Mutex<bool>,
    pub locked: Mutex<bool>,
    pub master: Mutex<String>,
    pub fail_reads: Mutex<Option<VaultError>>,
}

impl MemoryVault {
    pub fn unlocked() -> Arc<Self> {
        let vault = Self::default();
        *vault.configured.lock().unwrap() = true;
        Arc::new(vault)
    }

    pub fn store(&self, account: &str, password: &str) {
        self.passwords
            .lock()
            .unwrap()
            .insert(account.into(), password.into());
    }
}

impl PasswordVault for MemoryVault {
    fn status(&self) -> VaultStatus {
        VaultStatus {
            configured: *self.configured.lock().unwrap(),
            locked: *self.locked.lock().unwrap(),
        }
    }

    fn has_ssh_password(&self, account: &str) -> bool {
        self.passwords.lock().unwrap().contains_key(account)
    }

    fn ssh_password<'a>(
        &'a self,
        account: &'a str,
        master: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, VaultError>> {
        Box::pin(async move {
            if let Some(err) = self.fail_reads.lock().unwrap().clone() {
                return Err(err);
            }
            if *self.locked.lock().unwrap() && master != Some(self.master.lock().unwrap().as_str())
            {
                return Err(VaultError::InvalidMasterPassword(
                    "Wrong master password.".into(),
                ));
            }
            self.passwords
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .ok_or(VaultError::Other("missing".into()))
        })
    }

    fn remember_ssh_password<'a>(
        &'a self,
        account: &'a str,
        _label: &'a str,
        password: &'a str,
        _master: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), VaultError>> {
        Box::pin(async move {
            self.store(account, password);
            Ok(())
        })
    }

    fn create<'a>(&'a self, master: &'a str) -> BoxFuture<'a, Result<(), VaultError>> {
        Box::pin(async move {
            *self.configured.lock().unwrap() = true;
            *self.master.lock().unwrap() = master.to_string();
            Ok(())
        })
    }
}

/// A scratch HOME with an ssh_config and known_hosts of its own.
pub(crate) struct Sandbox {
    pub dir: tempfile::TempDir,
    pub env: LocalEnv,
    pub config: PathBuf,
    pub known_hosts: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let env = LocalEnv {
            home: home.clone(),
            username: "tester".into(),
            hostname: "laptop".into(),
            uid: 1000,
        };
        Self {
            config: home.join(".ssh").join("config"),
            known_hosts: home.join(".ssh").join("known_hosts"),
            dir,
            env,
        }
    }

    pub fn write_config(&self, lines: &[&str]) {
        std::fs::write(&self.config, format!("{}\n", lines.join("\n"))).unwrap();
    }

    /// Manager options pointing at this sandbox only: no agent, no global known_hosts.
    pub fn options(&self) -> ManagerOptions {
        let mut options = ManagerOptions::new(self.env.clone());
        let config = self.config.clone();
        let env = self.env.clone();
        options.load_config = Arc::new(move || load_config_document(&config, &env));
        options.known_hosts = KnownHostsStore::new(
            vec![self.known_hosts.clone()],
            vec![self.dir.path().join("no-global-known-hosts")],
        );
        options.env_lookup = Arc::new(|_: &str| None);
        options
    }

    pub fn manager(&self) -> SshConnectionManager {
        SshConnectionManager::new(self.options())
    }
}

/// Reads channel data until `needle` shows up.
pub(crate) async fn read_until(
    channel: &mut super::session::SessionChannel,
    needle: &str,
) -> String {
    let mut out = String::new();
    let wait = async {
        while let Some(msg) = channel.wait().await {
            if let russh::ChannelMsg::Data { data } = msg {
                out.push_str(&String::from_utf8_lossy(&data));
                if out.contains(needle) {
                    break;
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), wait).await;
    out
}
