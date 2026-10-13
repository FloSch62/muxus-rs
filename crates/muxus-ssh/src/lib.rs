//! SSH client: config, known_hosts, auth, jump hosts, shell integration, SFTP, forwards, X11.
//!
//! Port of the SSH stack in `server/src/ssh`, `server/src/forwards` and `server/src/x11`. Settings
//! come in as arguments and user interaction goes through async callback traits; this crate never
//! reads the Muxus store itself.

pub mod algorithms;
pub mod certificates;
pub mod config;
pub mod connection;
pub mod error;
pub mod folder_auth;
pub mod folder_paths;
pub mod forwards;
pub mod key_scan;
pub mod known_hosts;
pub mod local_env;
pub mod paths;
pub mod remote_shell_integration;
pub mod responsive_agent;
pub mod security_key_agent;
pub mod security_keys;
pub mod sftp;
pub mod shell;
pub mod types;
pub mod x11;

/// Test fixtures: an in-process SSH server (and SFTP server) on loopback. Built for this crate's
/// tests and, with the `testing` feature, for other crates' tests.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::Problem;
pub use local_env::LocalEnv;
