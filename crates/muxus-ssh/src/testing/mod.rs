//! Test fixtures: an in-process ssh-agent and SSH server bound to loopback.

#[cfg(test)]
pub mod agent;
pub mod keys;
pub mod server;
pub mod sftp_server;
