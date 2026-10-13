//! The dial plan: which hosts `ssh <target>` would dial, in order, and with which settings.

use crate::config::{
    ConfigDocument, HostSpec, IdentityTokens, ResolvedTarget, expand_identity_path,
    is_concrete_alias, parse_host_spec, resolve_host,
};
use crate::connection::profile::SshProfile;
use crate::folder_auth::{FolderAuthDefaults, FolderPasswordRef};
use crate::types::{ConfigForward, saved_host_hop_id};

const MAX_JUMP_DEPTH: usize = 8;

/// One node of the dial plan: the final target or a ProxyJump hop before it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainHop {
    /// What the user wrote for this node: an alias or [user@]host[:port].
    pub spec: HostSpec,
    pub resolved: ResolvedTarget,
    pub user: String,
    pub port: u16,
    /// Display label for prompts when this is an intermediate hop.
    pub hop_label: Option<String>,
    /// Folder passwords in the vault this hop may fall back to, nearest first.
    pub folder_passwords: Option<Vec<FolderPasswordRef>>,
    /// Target only: the host key an ssh:// link pinned, in canonical form.
    pub host_key_fingerprint: Option<String>,
}

impl ChainHop {
    /// `user@hostname:port`.
    pub fn label(&self) -> String {
        format!("{}@{}:{}", self.user, self.resolved.hostname, self.port)
    }

    /// The name prompts and messages use for this hop.
    pub fn display_name(&self) -> &str {
        self.hop_label.as_deref().unwrap_or(&self.spec.host)
    }
}

/// Folder defaults by alias or saved-profile ID.
pub type FolderLookup<'a> = &'a (dyn Fn(&str) -> Option<FolderAuthDefaults> + Send + Sync);
/// Saved Muxus SSH hosts by profile ID.
pub type SavedProfileLookup<'a> = &'a (dyn Fn(&str) -> Option<SshProfile> + Send + Sync);

/// Lookups the dial plan needs from the store.
#[derive(Default, Clone, Copy)]
pub struct ChainSources<'a> {
    /// Folder defaults per ssh_config alias.
    pub folder_auth: Option<FolderLookup<'a>>,
    /// Folder defaults per saved profile ID.
    pub profile_folder_auth: Option<FolderLookup<'a>>,
    /// Saved Muxus SSH hosts by profile ID.
    pub saved_profile: Option<SavedProfileLookup<'a>>,
}

/// Why a dial plan could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DialPlanError(pub String);

/// Expand a profile into the ordered list of hosts to dial: every ProxyJump hop (each
/// recursively resolved through the config, like the `ssh -W` processes OpenSSH would spawn) and
/// the final target last. Each hop that is a saved alias picks up its own folder's defaults as
/// the lowest-priority config layer. A hop naming a saved Muxus SSH host dials with that host's
/// own settings, exactly as connecting to it directly would.
pub fn build_chain(
    doc: &ConfigDocument,
    profile: &SshProfile,
    sources: ChainSources<'_>,
) -> Result<Vec<ChainHop>, DialPlanError> {
    let mut walker = Walker {
        doc,
        profile,
        sources,
        chain: Vec::new(),
        visited: Vec::new(),
    };
    walker.walk(
        parse_host_spec(&profile.target),
        Some(profile.clone()),
        true,
        0,
    )?;
    Ok(walker.chain)
}

struct Walker<'a> {
    doc: &'a ConfigDocument,
    profile: &'a SshProfile,
    sources: ChainSources<'a>,
    chain: Vec<ChainHop>,
    visited: Vec<String>,
}

impl Walker<'_> {
    /// `owner` is the Muxus-side profile whose fields win over the resolved base: the requested
    /// profile for the final target, or the saved host a jump hop names. Plain jump hops have
    /// none and resolve through the config.
    fn walk(
        &mut self,
        spec: HostSpec,
        owner: Option<SshProfile>,
        is_final: bool,
        depth: usize,
    ) -> Result<(), DialPlanError> {
        if depth > MAX_JUMP_DEPTH {
            return Err(DialPlanError("ProxyJump chain too deep".into()));
        }
        let owner_id = owner.as_ref().and_then(|o| o.profile_id.clone());
        let mut visit_keys = Vec::new();
        if is_final || owner_id.is_none() {
            visit_keys.push(spec.host.clone());
        }
        if let Some(id) = &owner_id {
            visit_keys.push(format!("profile:{id}"));
        }
        if visit_keys.iter().any(|key| self.visited.contains(key)) {
            return Err(DialPlanError(format!(
                "ProxyJump cycle detected at \"{}\"",
                spec.host
            )));
        }
        self.visited.extend(visit_keys);

        let from_config = owner
            .as_ref()
            .is_none_or(crate::connection::profile::uses_config);
        let folder = if from_config {
            self.sources.folder_auth.and_then(|f| f(&spec.host))
        } else {
            owner_id
                .as_deref()
                .and_then(|id| self.sources.profile_folder_auth.and_then(|f| f(id)))
        };
        let configured_base = if from_config {
            resolve_host(
                self.doc,
                &spec.host,
                folder.as_ref().map(|f| f.option_lines.as_slice()),
            )
        } else if let Some(folder) = &folder {
            resolve_host(
                &ConfigDocument::empty(&self.doc.env),
                &spec.host,
                Some(&folder.option_lines),
            )
        } else {
            direct_settings(&spec.host)
        };
        // The application preference is a fallback, not an override: aliases with an explicit
        // ServerAliveInterval (including 0) keep their own policy. Apply it to jump hops as well
        // so every TCP leg stays alive while idle.
        let mut base = configured_base;
        if base.server_alive_interval.is_none() {
            base.server_alive_interval = self.profile.keepalive_interval_seconds.map(f64::from);
        }
        let user = owner
            .as_ref()
            .and_then(|o| o.user.clone())
            .or_else(|| spec.user.clone())
            .or_else(|| base.user.clone())
            .unwrap_or_else(|| self.doc.env.username.clone());
        let resolved = match &owner {
            Some(owner) => overlay_profile(base, owner, &user, self.doc),
            None => base,
        };
        for hop_spec in resolved.proxy_jump.clone() {
            let Some(saved_id) = saved_host_hop_id(&hop_spec) else {
                self.walk(parse_host_spec(&hop_spec), None, false, depth + 1)?;
                continue;
            };
            let saved = self.sources.saved_profile.and_then(|f| f(saved_id));
            let Some(saved) = saved.filter(|s| !crate::connection::profile::uses_config(s)) else {
                return Err(DialPlanError(format!(
                    "saved SSH host \"{saved_id}\" used as a jump host was not found"
                )));
            };
            let saved = SshProfile {
                profile_id: Some(saved_id.to_string()),
                ..saved
            };
            self.walk(
                parse_host_spec(&saved.target),
                Some(saved),
                false,
                depth + 1,
            )?;
        }
        let port = owner
            .as_ref()
            .and_then(|o| o.port)
            .or(spec.port)
            .unwrap_or(resolved.port);
        self.chain.push(ChainHop {
            hop_label: (!is_final).then(|| spec.host.clone()),
            folder_passwords: folder.map(|f| f.passwords),
            host_key_fingerprint: if is_final {
                self.profile.host_key_fingerprint.clone()
            } else {
                None
            },
            spec,
            resolved,
            user,
            port,
        });
        Ok(())
    }
}

/// Layer a profile's own connection fields over the settings it resolved to.
fn overlay_profile(
    base: ResolvedTarget,
    profile: &SshProfile,
    user: &str,
    doc: &ConfigDocument,
) -> ResolvedTarget {
    let tokens = IdentityTokens {
        h: &base.hostname,
        r: user,
    };
    let expand = |files: &Vec<String>| -> Vec<String> {
        files
            .iter()
            .map(|f| expand_identity_path(f, &tokens, &doc.env))
            .collect()
    };
    let identity_files = profile.identity_files.as_ref().map(expand);
    let certificate_files = profile.certificate_files.as_ref().map(expand);
    let proxy_command = match (&profile.proxy_command, &profile.proxy_jump) {
        (Some(command), _) => Some(command.clone()),
        (None, None) => base.proxy_command.clone(),
        (None, Some(_)) => None,
    };
    ResolvedTarget {
        identity_files: identity_files.unwrap_or_else(|| base.identity_files.clone()),
        certificate_files: certificate_files.unwrap_or_else(|| base.certificate_files.clone()),
        identities_only: profile.identities_only.unwrap_or(base.identities_only),
        identity_agent: profile
            .identity_agent
            .clone()
            .or_else(|| base.identity_agent.clone()),
        forward_agent: profile.forward_agent.unwrap_or(base.forward_agent),
        forward_x11: profile.forward_x11.or(base.forward_x11),
        proxy_jump: profile
            .proxy_jump
            .clone()
            .unwrap_or_else(|| base.proxy_jump.clone()),
        proxy_command,
        forwards: profile
            .forwards
            .clone()
            .unwrap_or_else(|| base.forwards.clone()),
        password_only: profile.password_only.unwrap_or(base.password_only),
        remote_command: profile
            .remote_command
            .clone()
            .or_else(|| base.remote_command.clone()),
        request_tty: profile.request_tty.or(base.request_tty),
        strict_host_key_checking: profile
            .strict_host_key_checking
            .or(base.strict_host_key_checking),
        ..base
    }
}

/// Settings for a self-contained host that does not read ssh_config.
pub fn direct_settings(hostname: &str) -> ResolvedTarget {
    ResolvedTarget {
        hostname: hostname.to_string(),
        port: 22,
        ..ResolvedTarget::default()
    }
}

/// Identity of a dial plan for connection sharing: the resolved hop sequence
/// (user@hostname:port and agent-forwarding policy for each hop) plus the expanded ProxyCommand
/// transport when one applies. Settings that matter only while establishing a transport (auth
/// and keepalive policy alike) are deliberately absent: a keepalive preference change must not
/// fork sharing (and demand a second login) while the established transport still works.
pub fn mux_key(chain: &[ChainHop], disable_sftp: bool, console_compatibility: bool) -> String {
    // A pinned target never shares a transport whose key it has not checked.
    let hops: Vec<String> = chain
        .iter()
        .map(|hop| {
            format!(
                "{}@{}:{};agentForward={}{}",
                hop.user,
                hop.resolved.hostname,
                hop.port,
                if hop.resolved.forward_agent {
                    "yes"
                } else {
                    "no"
                },
                hop.host_key_fingerprint
                    .as_ref()
                    .map(|f| format!(";hostKey={f}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    let proxy = chain.first().and_then(expanded_proxy_command);
    let mut key = match proxy {
        Some(proxy) => std::iter::once(format!("proxy({proxy})"))
            .chain(hops)
            .collect::<Vec<_>>()
            .join(" -> "),
        None => hops.join(" -> "),
    };
    if disable_sftp {
        key.push_str(";muxusSftp=disabled");
    }
    if console_compatibility {
        key.push_str(";muxusConsole=compatible");
    }
    key
}

/// Adds forwards another alias asked for to a shared transport's list, skipping duplicates.
pub fn merge_config_forwards(
    current: &[ConfigForward],
    requested: &[ConfigForward],
) -> Vec<ConfigForward> {
    let mut merged = current.to_vec();
    for forward in requested {
        if !merged.contains(forward) {
            merged.push(forward.clone());
        }
    }
    merged
}

/// A fingerprint in the draft's `<key type>-<md5>` form names one key type, so a server holding
/// several host keys is asked for that one first. A configured HostKeyAlgorithms list is left
/// alone.
pub fn pinned_host_key_preference(hop: &ChainHop) -> ResolvedTarget {
    let mut resolved = hop.resolved.clone();
    if let Some(pinned) = &hop.host_key_fingerprint
        && let Some(space) = pinned.find(' ')
        && resolved.host_key_algorithms.is_none()
    {
        resolved.host_key_algorithms = Some(crate::algorithms::host_key_algorithms_preferring(
            &pinned[..space],
        ));
    }
    resolved
}

/// The tokens a ProxyCommand may use.
pub struct ProxyCommandTokens<'a> {
    pub hostname: &'a str,
    pub original_host: &'a str,
    pub port: u16,
    pub user: &'a str,
}

/// Expand the tokens accepted by OpenSSH's ProxyCommand directive.
pub fn expand_proxy_command(command: &str, tokens: &ProxyCommandTokens<'_>) -> String {
    let mut out = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(&next) = chars.peek()
        {
            let replacement = match next {
                '%' => Some("%".to_string()),
                'h' => Some(tokens.hostname.to_string()),
                'n' => Some(tokens.original_host.to_string()),
                'p' => Some(tokens.port.to_string()),
                'r' => Some(tokens.user.to_string()),
                _ => None,
            };
            if let Some(replacement) = replacement {
                out.push_str(&replacement);
                chars.next();
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// A hop's ProxyCommand with its tokens filled in, if it has one.
pub fn expanded_proxy_command(hop: &ChainHop) -> Option<String> {
    let command = hop.resolved.proxy_command.as_ref()?;
    Some(expand_proxy_command(
        command,
        &ProxyCommandTokens {
            hostname: &hop.resolved.hostname,
            original_host: &hop.spec.host,
            port: hop.port,
            user: &hop.user,
        },
    ))
}

/// Ad-hoc targets never masquerade as OpenSSH-backed database profiles. Same answer as looking
/// the target up in list_hosts(), without resolving every host in the config on each connect.
pub fn find_metadata_alias(doc: &ConfigDocument, requested_host: &str) -> Option<String> {
    doc.blocks
        .iter()
        .any(|block| {
            block
                .patterns
                .iter()
                .any(|p| p == requested_host && is_concrete_alias(p))
        })
        .then(|| requested_host.to_string())
}

#[cfg(test)]
mod tests;
