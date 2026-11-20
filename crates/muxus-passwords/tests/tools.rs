//! Databases and stores the password managers wrote themselves: KeePassXC, `pass` and
//! `gopass`, in the container of `hack/e2e/containers/passwords`. Decrypting the stores
//! needs `gpg` on this computer.
//!
//! ```bash
//! docker build -t muxusrs-e2e-passwords hack/e2e/containers/passwords
//! docker run --rm --user "$(id -u):$(id -g)" -v "$PWD/target/e2e-passwords:/out" muxusrs-e2e-passwords
//! MUXUS_E2E_PASSWORDS=$PWD/target/e2e-passwords cargo test -p muxus-passwords --test tools -- --ignored
//! ```
#![cfg(unix)]

use std::path::{Path, PathBuf};

use muxus_core::api::{PasswordManagerConfig, PasswordManagerEntry, PasswordManagerState};
use muxus_passwords::{
    HostEnv, KeePassProvider, PassProvider, PasswordManagerProblem, PasswordManagerProvider,
    ProviderStatus,
};

fn written() -> PathBuf {
    PathBuf::from(
        std::env::var("MUXUS_E2E_PASSWORDS")
            .expect("MUXUS_E2E_PASSWORDS names the directory the container wrote"),
    )
}

fn keepass(database: &str, key_file: Option<&str>) -> KeePassProvider {
    let file = |name: &str| written().join("keepass").join(name);
    KeePassProvider::new(
        &PasswordManagerConfig {
            path: Some(file(database).to_string_lossy().into_owned()),
            key_file: key_file.map(|name| file(name).to_string_lossy().into_owned()),
            ..Default::default()
        },
        &HostEnv::system(),
    )
}

fn entry(id: &str, title: &str, group: Option<&str>) -> PasswordManagerEntry {
    PasswordManagerEntry {
        id: id.to_owned(),
        title: title.to_owned(),
        group: group.map(str::to_owned),
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "needs what the muxusrs-e2e-passwords container wrote: see the module docs"]
async fn reads_a_keepassxc_database_opened_with_a_password_and_a_key_file() {
    let provider = keepass("both.kdbx", Some("both.keyx"));
    assert_eq!(
        provider.status().await.unwrap().state,
        PasswordManagerState::Locked
    );
    // Either half alone does not open it.
    let wrong = provider.unlock("not-the-password").await.unwrap_err();
    assert_eq!(wrong.problem, PasswordManagerProblem::InvalidUnlock);
    assert_eq!(wrong.message, "The database password or key file is wrong.");
    assert_eq!(
        provider.unlock("").await.unwrap_err().problem,
        PasswordManagerProblem::InvalidUnlock
    );
    let without_key_file = keepass("both.kdbx", None);
    let wrong = without_key_file.unlock("kf-master").await.unwrap_err();
    assert_eq!(wrong.problem, PasswordManagerProblem::InvalidUnlock);
    assert_eq!(wrong.message, "The database password is wrong.");

    provider.unlock("kf-master").await.unwrap();
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    let entries = provider.entries().await.unwrap();
    // The entry without a password and the one in the recycle bin are left out.
    let listed: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.group.as_deref(),
                entry.title.as_str(),
                entry.username.as_deref(),
                entry.url.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [(
            Some("Network / Edge"),
            "edge-fw",
            Some("fw-admin"),
            Some("https://edge-fw")
        )]
    );
    assert_eq!(
        provider.password(&entries[0].id).await.unwrap(),
        "edge-pässword"
    );
}

#[tokio::test]
#[ignore = "needs what the muxusrs-e2e-passwords container wrote: see the module docs"]
async fn reads_a_keepassxc_database_opened_with_a_key_file_alone() {
    let provider = keepass("only.kdbx", Some("only.keyx"));
    // No password is typed for a database that has none.
    provider.unlock("").await.unwrap();
    let entries = provider.entries().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "console");
    assert_eq!(entries[0].username.as_deref(), Some("root"));
    assert_eq!(
        provider.password(&entries[0].id).await.unwrap(),
        "only-pässword"
    );
    // The other database's key file is not this one's.
    let other = keepass("only.kdbx", Some("both.keyx"));
    assert_eq!(
        other.unlock("").await.unwrap_err().problem,
        PasswordManagerProblem::InvalidUnlock
    );
}

/// The container's key ring, copied to a short path: gpg-agent's socket lives under
/// GNUPGHOME and has a length limit.
struct KeyRing(tempfile::TempDir);

impl KeyRing {
    fn copy() -> Self {
        let home = tempfile::Builder::new()
            .prefix("mxg-")
            .tempdir_in("/tmp")
            .unwrap();
        copy_tree(&written().join("gnupg"), home.path());
        Self(home)
    }

    fn host(&self) -> HostEnv {
        HostEnv::system().with_var("GNUPGHOME", self.0.path().to_string_lossy())
    }
}

impl Drop for KeyRing {
    fn drop(&mut self) {
        let _ = std::process::Command::new("gpgconf")
            .args(["--kill", "all"])
            .env("GNUPGHOME", self.0.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn copy_tree(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(to).unwrap();
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700)).unwrap();
    for child in std::fs::read_dir(from).unwrap().flatten() {
        let kind = child.file_type().unwrap();
        let target = to.join(child.file_name());
        if kind.is_dir() {
            copy_tree(&child.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(child.path(), target).unwrap();
        }
    }
}

fn store(ring: &KeyRing, name: &str) -> PassProvider {
    PassProvider::new(
        &PasswordManagerConfig {
            path: Some(written().join(name).to_string_lossy().into_owned()),
            ..Default::default()
        },
        &ring.host(),
    )
}

#[tokio::test]
#[ignore = "needs what the muxusrs-e2e-passwords container wrote: see the module docs"]
async fn reads_a_store_pass_made() {
    let ring = KeyRing::copy();
    let provider = store(&ring, "pass");
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry("wifi", "wifi", None),
            entry("servers/lab/root", "root", Some("servers/lab")),
        ]
    );
    // The first line is the password; what follows is the entry's notes.
    assert_eq!(
        provider.password("servers/lab/root").await.unwrap(),
        "pass-password"
    );
    assert_eq!(provider.password("wifi").await.unwrap(), "wifi-pässword");
}

#[tokio::test]
#[ignore = "needs what the muxusrs-e2e-passwords container wrote: see the module docs"]
async fn reads_a_store_gopass_made() {
    let ring = KeyRing::copy();
    let provider = store(&ring, "gopass");
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    // Its own bookkeeping (.gpg-id, .public-keys) is not offered as entries.
    assert_eq!(
        provider.entries().await.unwrap(),
        [entry("network/core-sw", "core-sw", Some("network"))]
    );
    assert_eq!(
        provider.password("network/core-sw").await.unwrap(),
        "gopass-password"
    );
}
