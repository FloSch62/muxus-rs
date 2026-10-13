//! The SSH session profile (`sshProfileSchema` in `shared/src/ws-protocol.ts`), shared through
//! `muxus-core`.

pub use muxus_core::protocol::SshProfile;

/// Whether a profile resolves through ssh_config (the default); `useConfig: false` marks a
/// self-contained saved host or tunnel.
pub fn uses_config(profile: &SshProfile) -> bool {
    profile.use_config != Some(false)
}
