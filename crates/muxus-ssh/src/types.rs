//! Host and folder types from `shared/src/api-types.ts`, shared through `muxus-core`, plus the
//! small constructors the SSH layer uses on them.

pub use muxus_core::api::{
    ConfigForward, FolderAuthSettings, ForwardType, HostBlockOptions,
    HostOptionExtra as ConfigExtra, HostOptionsPatch, HostUpsertRequest, ResolvedHostSettings,
    SshHostEntry, saved_host_hop, saved_host_hop_id,
};
pub use muxus_core::js::Patch;
pub use muxus_core::protocol::{RequestTty, StrictHostKeyChecking};

/// An unmodeled `Keyword value` line, kept verbatim.
pub fn extra(keyword: impl Into<String>, value: impl Into<String>) -> ConfigExtra {
    ConfigExtra {
        keyword: keyword.into(),
        value: value.into(),
    }
}

/// Constructors for forward rules.
pub trait ConfigForwardExt {
    fn dynamic(bind_port: u16) -> ConfigForward;
    fn to(kind: ForwardType, bind_port: u16, host: &str, port: u16) -> ConfigForward;
}

impl ConfigForwardExt for ConfigForward {
    fn dynamic(bind_port: u16) -> ConfigForward {
        ConfigForward {
            kind: ForwardType::Dynamic,
            bind_port,
            target_host: None,
            target_port: None,
        }
    }

    fn to(kind: ForwardType, bind_port: u16, host: &str, port: u16) -> ConfigForward {
        ConfigForward {
            kind,
            bind_port,
            target_host: Some(host.to_string()),
            target_port: Some(port),
        }
    }
}

/// Renders a patch value; "leave alone" and "remove" pass through.
pub fn render_patch<T>(patch: &Patch<T>, format: impl FnOnce(&T) -> String) -> Patch<String> {
    match patch {
        Patch::Keep => Patch::Keep,
        Patch::Clear => Patch::Clear,
        Patch::Set(value) => Patch::Set(format(value)),
    }
}

/// ssh_config spellings of the yes/no-style choice options.
pub trait ConfigChoice: Sized + Copy + 'static {
    const VALUES: &'static [(&'static str, Self)];

    /// Case-insensitive parse of an ssh_config value.
    fn parse(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        Self::VALUES
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, v)| *v)
    }

    /// The ssh_config spelling.
    fn as_str(self) -> &'static str
    where
        Self: PartialEq,
    {
        Self::VALUES
            .iter()
            .find(|(_, v)| *v == self)
            .map(|(name, _)| *name)
            .unwrap_or("")
    }
}

impl ConfigChoice for RequestTty {
    const VALUES: &'static [(&'static str, Self)] = &[
        ("no", RequestTty::No),
        ("yes", RequestTty::Yes),
        ("force", RequestTty::Force),
        ("auto", RequestTty::Auto),
    ];
}

impl ConfigChoice for StrictHostKeyChecking {
    const VALUES: &'static [(&'static str, Self)] = &[
        ("yes", StrictHostKeyChecking::Yes),
        ("no", StrictHostKeyChecking::No),
        ("accept-new", StrictHostKeyChecking::AcceptNew),
        ("ask", StrictHostKeyChecking::Ask),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_host_hops_round_trip() {
        assert_eq!(saved_host_hop("abc"), "muxus-host:abc");
        assert_eq!(saved_host_hop_id("muxus-host:abc"), Some("abc"));
        assert_eq!(saved_host_hop_id("muxus-host:"), None);
        assert_eq!(saved_host_hop_id("bastion"), None);
    }

    #[test]
    fn forwards_serialize_like_the_typescript_dto() {
        let json =
            serde_json::to_string(&ConfigForward::to(ForwardType::Local, 8080, "h", 80)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"local","bindPort":8080,"targetHost":"h","targetPort":80}"#
        );
    }
}
