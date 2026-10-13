//! Port of tests/unit/server/connection-algorithms.test.ts: legacy algorithm negotiation against
//! a server restricted to what old console servers and switches speak.

use std::borrow::Cow;

use russh::keys::{Algorithm, PrivateKey};
use russh::{Preferred, cipher, kex, mac};

use super::*;
use crate::connection::profile::SshProfile;
use crate::testing::server::{ServerSpec, start};

const PASSWORD: &str = "secret";

/// An sshd stand-in for an old console server: ssh-rsa host key only, and a handshake restricted
/// to the legacy algorithms modern clients disable by default.
fn legacy_spec(
    kex: &'static [kex::Name],
    ciphers: &'static [cipher::Name],
    macs: &'static [mac::Name],
) -> ServerSpec {
    let mut spec = ServerSpec::new()
        .password(PASSWORD)
        .methods(&[russh::MethodKind::Password]);
    spec.host_key = PrivateKey::from_openssh(crate::testing::keys::RSA_HOST_KEY).unwrap();
    spec.preferred = Preferred {
        kex: Cow::Borrowed(kex),
        key: Cow::Borrowed(&[Algorithm::Rsa { hash: None }]),
        cipher: Cow::Borrowed(ciphers),
        mac: Cow::Borrowed(macs),
        ..Preferred::default()
    };
    spec
}

fn config(sandbox: &Sandbox, port: u16, algorithm_lines: &[&str]) {
    let port_line = format!("  Port {port}");
    let mut lines = vec![
        "Host console",
        "  HostName 127.0.0.1",
        "  User tester",
        &port_line,
        "  PubkeyAuthentication no",
        "  StrictHostKeyChecking accept-new",
    ];
    lines.extend_from_slice(algorithm_lines);
    sandbox.write_config(&lines);
}

const GROUP14_SHA1: &[kex::Name] = &[kex::DH_G14_SHA1, kex::EXTENSION_SUPPORT_AS_SERVER];
const AES128_CBC: &[cipher::Name] = &[cipher::AES_128_CBC];
const SHA2_MACS: &[mac::Name] = &[mac::HMAC_SHA256, mac::HMAC_SHA1];

#[tokio::test]
async fn fails_against_a_legacy_only_server_when_the_config_sets_no_algorithms() {
    let server = start(legacy_spec(GROUP14_SHA1, AES128_CBC, SHA2_MACS)).await;
    let sandbox = Sandbox::new();
    config(&sandbox, server.port(), &[]);
    let err = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("console"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no matching key exchange algorithm"), "{err}");
    assert!(err.contains("KexAlgorithms"), "{err}");
}

#[tokio::test]
async fn connects_once_the_config_lists_the_legacy_algorithms() {
    let server = start(legacy_spec(GROUP14_SHA1, AES128_CBC, SHA2_MACS)).await;
    let sandbox = Sandbox::new();
    config(
        &sandbox,
        server.port(),
        &[
            "  Ciphers aes128-cbc",
            "  KexAlgorithms +diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1",
            "  HostKeyAlgorithms +ssh-rsa",
        ],
    );
    let mut shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("console"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        read_until(&mut shell.channel, "shell ok")
            .await
            .contains("shell ok")
    );
    assert_eq!(
        shell.summary.kex.as_deref(),
        Some("diffie-hellman-group14-sha1")
    );
    assert_eq!(shell.summary.cipher.as_deref(), Some("aes128-cbc"));
}

#[tokio::test]
async fn connects_to_a_device_that_only_speaks_diffie_hellman_group1_sha1() {
    // Cisco IOS 12.2 switches offer nothing newer than these.
    const GROUP1: &[kex::Name] = &[kex::DH_G1_SHA1, kex::EXTENSION_SUPPORT_AS_SERVER];
    const AES256_CBC: &[cipher::Name] = &[cipher::AES_256_CBC];
    const SHA1: &[mac::Name] = &[mac::HMAC_SHA1];
    let server = start(legacy_spec(GROUP1, AES256_CBC, SHA1)).await;
    let sandbox = Sandbox::new();
    config(
        &sandbox,
        server.port(),
        &[
            "  KexAlgorithms diffie-hellman-group1-sha1",
            "  HostKeyAlgorithms ssh-rsa",
            "  Ciphers aes256-cbc",
            "  MACs hmac-sha1",
        ],
    );
    let mut shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("console"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        read_until(&mut shell.channel, "shell ok")
            .await
            .contains("shell ok")
    );
    assert_eq!(
        shell.summary.kex.as_deref(),
        Some("diffie-hellman-group1-sha1")
    );
}

#[tokio::test]
async fn negotiates_3des_cbc_for_the_oldest_gear() {
    const TRIPLE_DES: &[cipher::Name] = &[cipher::TRIPLE_DES_CBC];
    let server = start(legacy_spec(GROUP14_SHA1, TRIPLE_DES, SHA2_MACS)).await;
    let sandbox = Sandbox::new();
    config(
        &sandbox,
        server.port(),
        &[
            "  Ciphers 3des-cbc",
            "  KexAlgorithms diffie-hellman-group14-sha1",
            "  HostKeyAlgorithms ssh-rsa",
        ],
    );
    let shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("console"),
            TestIo::accepting(PASSWORD),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(shell.summary.cipher.as_deref(), Some("3des-cbc"));
}

#[tokio::test]
async fn reports_config_algorithms_the_engine_cannot_offer() {
    let server = start(ServerSpec::new().password(PASSWORD)).await;
    let sandbox = Sandbox::new();
    config(
        &sandbox,
        server.port(),
        &["  KexAlgorithms +sntrup761x25519-sha512@openssh.com"],
    );
    let io = TestIo::accepting(PASSWORD);
    let _shell = sandbox
        .manager()
        .connect_shell(
            &SshProfile::new("console"),
            io.clone(),
            80,
            24,
            "xterm-256color",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        io.statuses()
            .iter()
            .any(|s| s.contains("KexAlgorithms: skipping sntrup761x25519-sha512@openssh.com"))
    );
}
