//! Which login sequence a session runs (`server/src/login-sequence/resolve.ts`).

use muxus_core::api::LoginSequence;
use muxus_core::folder_paths::folder_chain;

/// A host whose Muxus metadata can carry a login sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginSequenceHost {
    /// An ssh_config alias.
    Alias(String),
    /// A saved host.
    Profile(String),
}

/// A host's own login sequence entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostLoginSequence {
    /// The host's own sequence; `None` inherits.
    pub login_sequence: Option<LoginSequence>,
    /// The host's folder.
    pub group: Option<String>,
}

/// What the resolver needs to know; satisfied by the store.
pub trait LoginSequenceSource {
    /// The host's entry; `None` for an unknown host.
    fn host_login_sequence(&self, host: &LoginSequenceHost) -> Option<HostLoginSequence>;
    /// The login sequence a folder sets, if any.
    fn folder_login_sequence(&self, path: &str) -> Option<LoginSequence>;
}

/// The sequence a session to this host runs: the host's own, or else the
/// nearest folder's that sets one. An empty sequence on the host or on a
/// folder means "none" and ends the search, so a folder's sequence can be
/// switched off for one host or one subfolder.
pub fn resolve_login_sequence(
    source: &dyn LoginSequenceSource,
    host: &LoginSequenceHost,
) -> Option<LoginSequence> {
    let entry = source.host_login_sequence(host)?;
    let sequence = entry.login_sequence.or_else(|| {
        folder_chain(entry.group.as_deref().unwrap_or(""))
            .iter()
            .find_map(|path| source.folder_login_sequence(path))
    })?;
    (!sequence.steps.is_empty()).then_some(sequence)
}
