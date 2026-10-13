//! One host hierarchy over both host sources. Port of `client/src/managed-hosts.ts`
//! and `client/src/components/sidebar/host-sftp-action.ts` (the pure check).

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::hosts::organization::{
    GroupKind, compare_sort_order, group_hosts, host_address, host_display_name, host_search_text,
};
use crate::hosts::saved::{
    filter_saved_hosts, open_ssh_jump_hops, saved_host_address, saved_host_display_name,
    saved_host_search_text,
};
use crate::hosts::search::{match_score, search_tokens};
use crate::model::{
    ForwardKind, GnmiProfile, GnmiTlsMode, HostMetadata, ProfileKind, SavedHostProfile,
    SavedHostProfileExt, SavedHostSessionProfile, SessionProfile, SshHostEntry, SshProfile,
    wire_name,
};
use crate::util::locale_compare;

/// A sidebar host from either source. Cheap to clone: entries are shared.
#[derive(Clone, Debug, PartialEq)]
pub enum ManagedHost {
    Ssh(Arc<SshHostEntry>),
    Profile(Arc<SavedHostProfile>),
}

impl ManagedHost {
    pub fn ssh(entry: SshHostEntry) -> Self {
        ManagedHost::Ssh(Arc::new(entry))
    }

    pub fn profile(entry: SavedHostProfile) -> Self {
        ManagedHost::Profile(Arc::new(entry))
    }

    /// Muxus metadata; ssh_config hosts may have none yet.
    pub fn metadata(&self) -> Option<&HostMetadata> {
        match self {
            ManagedHost::Ssh(entry) => entry.metadata.as_ref(),
            ManagedHost::Profile(entry) => Some(&entry.metadata),
        }
    }

    /// The host's folder path as stored, if it has one.
    pub fn group(&self) -> Option<&str> {
        self.metadata()
            .and_then(|metadata| metadata.group.as_deref())
    }

    /// The connection kind, for the row icon: ssh_config hosts are SSH.
    pub fn kind(&self) -> ProfileKind {
        match self {
            ManagedHost::Ssh(_) => ProfileKind::Ssh,
            ManagedHost::Profile(entry) => entry.profile_kind(),
        }
    }

    pub fn display_name(&self) -> &str {
        managed_host_display_name(self)
    }

    pub fn key(&self) -> String {
        managed_host_key(self)
    }
}

/// `ManagedHostRef`: one sidebar host, addressable across both host sources.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ManagedHostRef {
    Ssh { alias: String },
    Profile { id: String },
}

/// Resolve the persisted host which produced a live tab, when one still exists.
pub fn editable_managed_host_for_profile(
    profile: &SessionProfile,
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
) -> Option<ManagedHost> {
    if profile.kind() == ProfileKind::Local {
        return None;
    }
    if let Some(profile_id) = profile.profile_id() {
        return saved_profiles
            .iter()
            .find(|candidate| candidate.id == profile_id)
            .map(|entry| ManagedHost::profile(entry.clone()));
    }
    let SessionProfile::Ssh(SshProfile {
        target, use_config, ..
    }) = profile
    else {
        return None;
    };
    if *use_config == Some(false) {
        return None;
    }
    ssh_hosts
        .iter()
        .find(|candidate| candidate.aliases.contains(target))
        .map(|entry| ManagedHost::ssh(entry.clone()))
}

/// A group of the sidebar: a Muxus folder path or an ssh_config file's ungrouped hosts.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagedHostGroup {
    pub key: String,
    pub label: String,
    pub kind: GroupKind,
    pub tooltip: Option<String>,
    pub hosts: Vec<ManagedHost>,
}

/// Build one host hierarchy independent of the underlying connection protocol.
pub fn group_managed_hosts(
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
    files: &[String],
    root_file: Option<&str>,
    filter: &str,
) -> Vec<ManagedHostGroup> {
    let profiles = filter_saved_hosts(saved_profiles, filter);
    let has_saved_custom_groups = profiles.iter().any(|profile| {
        profile
            .metadata
            .group
            .as_deref()
            .is_some_and(|g| !g.trim().is_empty())
    });
    let mut groups: Vec<ManagedHostGroup> =
        group_hosts(ssh_hosts, files, root_file, filter, has_saved_custom_groups)
            .into_iter()
            .map(|group| ManagedHostGroup {
                key: group.key,
                label: group.label,
                kind: group.kind,
                tooltip: group.tooltip,
                hosts: group
                    .hosts
                    .into_iter()
                    .map(|entry| ManagedHost::ssh(entry.clone()))
                    .collect(),
            })
            .collect();
    let mut custom_by_key: HashMap<String, usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.kind == GroupKind::Custom)
        .map(|(index, group)| (group.key.clone(), index))
        .collect();
    let mut ungrouped: Vec<&SavedHostProfile> = Vec::new();

    for profile in profiles {
        let label = profile
            .metadata
            .group
            .as_deref()
            .map(str::trim)
            .unwrap_or("");
        if label.is_empty() {
            ungrouped.push(profile);
            continue;
        }
        let key = format!("custom:{}", label.to_lowercase());
        let index = match custom_by_key.get(&key) {
            Some(&index) => index,
            None => {
                groups.push(ManagedHostGroup {
                    key: key.clone(),
                    label: label.to_string(),
                    kind: GroupKind::Custom,
                    tooltip: None,
                    hosts: Vec::new(),
                });
                custom_by_key.insert(key, groups.len() - 1);
                groups.len() - 1
            }
        };
        groups[index]
            .hosts
            .push(ManagedHost::profile(profile.clone()));
    }

    if !ungrouped.is_empty() {
        let root_key = root_file.map(|file| format!("file:{file}"));
        let existing = root_key
            .as_ref()
            .and_then(|key| groups.iter().position(|candidate| &candidate.key == key));
        let index = match existing {
            Some(index) => index,
            None => {
                let group = ManagedHostGroup {
                    key: "managed:ungrouped".into(),
                    label: if custom_by_key.is_empty() {
                        "Hosts"
                    } else {
                        "Ungrouped"
                    }
                    .into(),
                    kind: GroupKind::File,
                    tooltip: None,
                    hosts: Vec::new(),
                };
                let first_file = groups
                    .iter()
                    .position(|candidate| candidate.kind == GroupKind::File)
                    .unwrap_or(groups.len());
                groups.insert(first_file, group);
                first_file
            }
        };
        // A mixed group is no longer sourced solely from one ssh_config file.
        groups[index].tooltip = None;
        groups[index].hosts.extend(
            ungrouped
                .into_iter()
                .map(|entry| ManagedHost::profile(entry.clone())),
        );
    }

    for group in &mut groups {
        group.hosts.sort_by(compare_managed_hosts);
    }
    let (mut custom, file): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|group| group.kind == GroupKind::Custom);
    custom.sort_by(|a, b| locale_compare(&a.label, &b.label));
    custom.extend(file);
    custom
}

/// The host a query is most likely spelling out: what Enter connects to, and
/// what the sidebar highlights so you can see it before pressing it. Ties keep
/// the caller's order, leaving the tree's own arrangement to break them.
///
/// Scoring the flat list beats grouping it: quick-connect has to answer for the
/// text as typed, on every keystroke.
pub fn best_managed_host_match<'a>(
    hosts: &'a [ManagedHost],
    normalized_query: &str,
) -> Option<&'a ManagedHost> {
    let tokens = search_tokens(normalized_query);
    if tokens.is_empty() {
        return None;
    }
    let mut best = None;
    let mut best_score = 0;
    for host in hosts {
        let searchable = match host {
            ManagedHost::Ssh(entry) => host_search_text(entry),
            ManagedHost::Profile(entry) => saved_host_search_text(entry),
        };
        let label = managed_host_display_name(host).to_lowercase();
        let Some(score) = match_score(&label, &searchable, normalized_query, &tokens) else {
            continue;
        };
        if score > best_score {
            best = Some(host);
            best_score = score;
        }
    }
    best
}

/// Every Muxus group label in use, across both host sources, for pickers.
pub fn known_host_groups(
    ssh_hosts: &[SshHostEntry],
    saved_profiles: &[SavedHostProfile],
) -> Vec<String> {
    let unique: BTreeSet<String> = ssh_hosts
        .iter()
        .filter_map(|host| host.metadata.as_ref().and_then(|m| m.group.clone()))
        .chain(
            saved_profiles
                .iter()
                .filter_map(|profile| profile.metadata.group.clone()),
        )
        .filter(|group| !group.is_empty())
        .collect();
    let mut out: Vec<String> = unique.into_iter().collect();
    out.sort_by(|a, b| locale_compare(a, b));
    out
}

pub fn managed_host_display_name(host: &ManagedHost) -> &str {
    match host {
        ManagedHost::Ssh(entry) => host_display_name(entry),
        ManagedHost::Profile(entry) => saved_host_display_name(entry),
    }
}

/// Alphabetize a mixed host list by the name shown in the sidebar.
pub fn alphabetize_managed_hosts(hosts: &[ManagedHost]) -> Vec<ManagedHost> {
    let mut out = hosts.to_vec();
    out.sort_by(|left, right| {
        locale_compare(
            managed_host_display_name(left),
            managed_host_display_name(right),
        )
    });
    out
}

pub fn managed_host_address(host: &ManagedHost) -> String {
    match host {
        ManagedHost::Ssh(entry) => host_address(entry),
        ManagedHost::Profile(entry) => saved_host_address(entry),
    }
}

pub fn managed_host_ref(host: &ManagedHost) -> ManagedHostRef {
    match host {
        ManagedHost::Ssh(entry) => ManagedHostRef::Ssh {
            alias: entry.alias.clone(),
        },
        ManagedHost::Profile(entry) => ManagedHostRef::Profile {
            id: entry.id.clone(),
        },
    }
}

pub fn managed_host_ref_key(reference: &ManagedHostRef) -> String {
    match reference {
        ManagedHostRef::Ssh { alias } => format!("ssh:{alias}"),
        ManagedHostRef::Profile { id } => format!("profile:{id}"),
    }
}

/// Stable identity used for list keys, drag state, and reorder payloads.
pub fn managed_host_key(host: &ManagedHost) -> String {
    match host {
        ManagedHost::Ssh(entry) => format!("ssh:{}", entry.alias),
        ManagedHost::Profile(entry) => format!("profile:{}", entry.id),
    }
}

/// Whether the host context menu may offer the SSH-backed file browser.
pub fn managed_host_supports_sftp(host: &ManagedHost) -> bool {
    let allowed = |metadata: Option<&HostMetadata>| {
        metadata
            .is_none_or(|m| m.disable_sftp != Some(true) && m.console_compatibility != Some(true))
    };
    match host {
        ManagedHost::Ssh(entry) => allowed(entry.metadata.as_ref()),
        ManagedHost::Profile(entry) => {
            entry.profile_kind() == ProfileKind::Ssh && allowed(Some(&entry.metadata))
        }
    }
}

/// A clipboard action offered in the host context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyCommand {
    pub label: &'static str,
    pub text: String,
}

/// The per-kind clipboard action offered in the host context menu. Saved hosts
/// it jumps through are spelled out from `saved_profiles`.
pub fn managed_host_copy_command(
    host: &ManagedHost,
    saved_profiles: &[SavedHostProfile],
) -> CopyCommand {
    let entry = match host {
        ManagedHost::Ssh(entry) => {
            return CopyCommand {
                label: "Copy ssh command",
                text: format!("ssh {}", entry.alias),
            };
        }
        ManagedHost::Profile(entry) => entry,
    };
    match &entry.profile {
        SavedHostSessionProfile::Ssh(profile) => CopyCommand {
            label: "Copy ssh command",
            text: saved_ssh_copy_command(profile, saved_profiles),
        },
        SavedHostSessionProfile::Rdp(profile) => CopyCommand {
            label: "Copy address",
            text: format!("{}:{}", profile.host, profile.port),
        },
        SavedHostSessionProfile::Vnc(profile) => CopyCommand {
            label: "Copy address",
            text: format!("{}:{}", profile.host, profile.port),
        },
        SavedHostSessionProfile::Gnmi(profile) => CopyCommand {
            label: "Copy gnmic command",
            text: gnmic_command(profile),
        },
        SavedHostSessionProfile::Netconf(profile) => {
            let (host, port, username) = (&profile.host, profile.port, &profile.username);
            let user = username
                .as_deref()
                .filter(|u| !u.is_empty())
                .map(|u| format!("{u}@"))
                .unwrap_or_default();
            CopyCommand {
                label: "Copy ssh command",
                text: format!("ssh -p {port} {user}{host} -s netconf"),
            }
        }
        SavedHostSessionProfile::Telnet(profile) => CopyCommand {
            label: "Copy telnet command",
            text: format!("telnet {} {}", profile.host, profile.port),
        },
        SavedHostSessionProfile::Serial(profile) => CopyCommand {
            label: "Copy device path",
            text: profile.path.clone(),
        },
    }
}

/// Render a self-contained profile as a pasteable OpenSSH command.
fn saved_ssh_copy_command(options: &SshProfile, saved_profiles: &[SavedHostProfile]) -> String {
    let SshProfile {
        target,
        user,
        port,
        identity_files,
        proxy_jump,
        forwards,
        ..
    } = options;
    let mut args: Vec<String> = vec!["ssh".into()];
    if let Some(port) = port {
        args.extend(["-p".into(), port.to_string()]);
    }
    for file in identity_files.iter().flatten() {
        args.extend(["-i".into(), file.clone()]);
    }
    for file in options.certificate_files.iter().flatten() {
        args.extend(["-o".into(), format!("CertificateFile={file}")]);
    }
    if let Some(only) = options.identities_only {
        args.extend([
            "-o".into(),
            format!("IdentitiesOnly={}", if only { "yes" } else { "no" }),
        ]);
    }
    if let Some(agent) = &options.identity_agent {
        args.extend(["-o".into(), format!("IdentityAgent={agent}")]);
    }
    if options.forward_agent == Some(true) {
        args.push("-A".into());
    }
    if let Some(x11) = options.forward_x11 {
        args.push(if x11 { "-X" } else { "-x" }.into());
    }
    let jumps = open_ssh_jump_hops(
        proxy_jump.as_deref().unwrap_or_default(),
        saved_profiles,
        None,
        &HashSet::new(),
    );
    if !jumps.is_empty() {
        args.extend(["-J".into(), jumps.join(",")]);
    }
    if let Some(command) = &options.proxy_command {
        args.extend(["-o".into(), format!("ProxyCommand={command}")]);
    }
    if options.password_only == Some(true) {
        args.extend([
            "-o".into(),
            "PubkeyAuthentication=no".into(),
            "-o".into(),
            "PreferredAuthentications=keyboard-interactive,password".into(),
        ]);
    }
    for forward in forwards.iter().flatten() {
        if forward.kind == ForwardKind::Dynamic {
            args.extend(["-D".into(), forward.bind_port.to_string()]);
            continue;
        }
        let option = if forward.kind == ForwardKind::Local {
            "-L"
        } else {
            "-R"
        };
        args.extend([
            option.into(),
            format!(
                "{}:{}:{}",
                forward.bind_port,
                forward.target_host.as_deref().unwrap_or("undefined"),
                forward
                    .target_port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "undefined".into()),
            ),
        ]);
    }
    match options.request_tty.as_ref().map(wire_name).as_deref() {
        Some("no") => args.push("-T".into()),
        Some("yes") => args.push("-t".into()),
        Some("force") => args.push("-tt".into()),
        Some("auto") => args.extend(["-o".into(), "RequestTTY=auto".into()]),
        _ => {}
    }
    if let Some(strict) = &options.strict_host_key_checking {
        args.extend([
            "-o".into(),
            format!("StrictHostKeyChecking={}", wire_name(strict)),
        ]);
    }
    args.push(match user {
        Some(user) => format!("{user}@{target}"),
        None => target.clone(),
    });
    if let Some(command) = &options.remote_command {
        args.push(command.clone());
    }
    args.iter()
        .map(|arg| shell_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_argument(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./~-".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

/// Quote for POSIX shells (`management/copy-as.ts`).
fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || "@%+=:,./-".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// The gnmic command for a host (`gnmicCommand` without a request draft).
pub fn gnmic_command(profile: &GnmiProfile) -> String {
    let GnmiProfile {
        host,
        port,
        username,
        tls,
        tls_server_name,
        ca_file,
        cert_file,
        key_file,
        encoding,
        ..
    } = profile;
    let mut args: Vec<String> = vec![
        "gnmic".into(),
        "-a".into(),
        shell_quote(&format!("{host}:{port}")),
    ];
    if let Some(username) = username.as_deref().filter(|u| !u.is_empty()) {
        args.extend([
            "-u".into(),
            shell_quote(username),
            "-p".into(),
            "\"$GNMIC_PASSWORD\"".into(),
        ]);
    }
    match tls.unwrap_or(GnmiTlsMode::Verify) {
        GnmiTlsMode::Plaintext => args.push("--insecure".into()),
        GnmiTlsMode::SkipVerify => args.push("--skip-verify".into()),
        GnmiTlsMode::Verify => {}
    }
    for (flag, value) in [
        ("--tls-ca", ca_file),
        ("--tls-cert", cert_file),
        ("--tls-key", key_file),
        ("--tls-server-name", tls_server_name),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), shell_quote(value)]);
        }
    }
    if let Some(encoding) = encoding {
        args.extend(["-e".into(), wire_name(encoding)]);
    }
    args.push("capabilities".into());
    args.join(" ")
}

fn compare_managed_hosts(a: &ManagedHost, b: &ManagedHost) -> Ordering {
    compare_sort_order(
        a.metadata().and_then(|m| m.sort_order),
        b.metadata().and_then(|m| m.sort_order),
    )
    .then_with(|| locale_compare(managed_host_display_name(a), managed_host_display_name(b)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hosts::saved::saved_host_hop;
    use crate::hosts::test_support::{ROOT, ssh_host};
    use crate::model::{
        ConfigForward, RequestTty, SerialProfile, StrictHostKeyChecking, TelnetProfile,
        local_profile, ssh_profile,
    };
    use crate::sidebar::host_details::host_detail_lines;

    fn telnet_host(name: &str, group: Option<&str>) -> SavedHostProfile {
        let id = format!("telnet-{name}");
        SavedHostProfile {
            id: id.clone(),
            name: name.into(),
            kind: crate::model::SavedHostKind::Telnet,
            profile: SavedHostSessionProfile::Telnet(TelnetProfile {
                profile_id: Some(id.clone()),
                host: format!("{name}.example.test"),
                port: 23,
            }),
            metadata: HostMetadata {
                profile_id: id,
                group: group.map(Into::into),
                ..Default::default()
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn serial_host(name: &str) -> SavedHostProfile {
        let id = format!("serial-{name}");
        SavedHostProfile {
            id: id.clone(),
            name: name.into(),
            kind: crate::model::SavedHostKind::Serial,
            profile: SavedHostSessionProfile::Serial(SerialProfile {
                profile_id: Some(id.clone()),
                ..SerialProfile::new("COM3")
            }),
            metadata: HostMetadata {
                profile_id: id,
                ..Default::default()
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    pub(crate) fn native_ssh_host(name: &str) -> SavedHostProfile {
        let id = format!("native-ssh-{name}");
        SavedHostProfile {
            id: id.clone(),
            kind: crate::model::SavedHostKind::Ssh,
            name: name.into(),
            profile: SavedHostSessionProfile::Ssh(SshProfile {
                profile_id: Some(id.clone()),
                use_config: Some(false),
                user: Some("admin".into()),
                port: Some(2222),
                ..SshProfile::new(format!("{name}.example.test"))
            }),
            metadata: HostMetadata {
                profile_id: id,
                ..Default::default()
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn with_ssh(
        profile: &SavedHostProfile,
        edit: impl FnOnce(&mut SshProfile),
    ) -> SavedHostProfile {
        let mut out = profile.clone();
        if let SavedHostSessionProfile::Ssh(ssh) = &mut out.profile {
            edit(ssh);
        }
        out
    }

    fn kinds(group: &ManagedHostGroup) -> Vec<String> {
        group
            .hosts
            .iter()
            .map(|host| match host {
                ManagedHost::Profile(entry) => format!("profile:{}", entry.kind.as_str()),
                ManagedHost::Ssh(_) => "ssh:ssh".into(),
            })
            .collect()
    }

    #[test]
    fn places_ungrouped_ssh_telnet_and_serial_profiles_in_one_hosts_group() {
        let groups = group_managed_hosts(
            &[ssh_host("router", None)],
            &[telnet_host("console", None), serial_host("rack console")],
            &[ROOT.into()],
            Some(ROOT),
            "",
        );
        assert_eq!(
            groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(),
            ["Hosts"]
        );
        assert_eq!(
            kinds(&groups[0]),
            ["profile:telnet", "profile:serial", "ssh:ssh"]
        );
    }

    #[test]
    fn merges_profiles_and_ssh_entries_that_use_the_same_custom_group() {
        let groups = group_managed_hosts(
            &[
                ssh_host("router", Some("Production")),
                ssh_host("lab", None),
            ],
            &[telnet_host("console", Some("production"))],
            &[ROOT.into()],
            Some(ROOT),
            "",
        );
        assert_eq!(
            groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(),
            ["Production", "Ungrouped"]
        );
        let kinds: Vec<_> = groups[0]
            .hosts
            .iter()
            .map(|h| {
                if matches!(h, ManagedHost::Ssh(_)) {
                    "ssh"
                } else {
                    "profile"
                }
            })
            .collect();
        assert_eq!(kinds, ["profile", "ssh"]);
    }

    #[test]
    fn resolves_the_persisted_host_behind_a_session_profile_for_editing() {
        let ssh_entry = ssh_host("router", None);
        let native = native_ssh_host("core");
        let telnet = telnet_host("console", None);
        assert_eq!(
            editable_managed_host_for_profile(
                &ssh_profile("router"),
                std::slice::from_ref(&ssh_entry),
                &[native.clone(), telnet.clone()],
            ),
            Some(ManagedHost::ssh(ssh_entry.clone()))
        );
        let mut telnet_profile = telnet.session_profile();
        if let SessionProfile::Telnet(TelnetProfile { profile_id, .. }) = &mut telnet_profile {
            *profile_id = Some(telnet.id.clone());
        }
        assert_eq!(
            editable_managed_host_for_profile(
                &telnet_profile,
                std::slice::from_ref(&ssh_entry),
                &[native.clone(), telnet.clone()],
            ),
            Some(ManagedHost::profile(telnet.clone()))
        );
        assert_eq!(
            editable_managed_host_for_profile(
                &ssh_profile("unlisted.example.test"),
                std::slice::from_ref(&ssh_entry),
                std::slice::from_ref(&native),
            ),
            None
        );
        let mut no_config = ssh_profile("router");
        if let SessionProfile::Ssh(SshProfile { use_config, .. }) = &mut no_config {
            *use_config = Some(false);
        }
        assert_eq!(
            editable_managed_host_for_profile(&no_config, std::slice::from_ref(&ssh_entry), &[]),
            None
        );
        assert_eq!(
            editable_managed_host_for_profile(&local_profile(), &[ssh_entry], &[native]),
            None
        );
    }

    #[test]
    fn derives_stable_keys_matching_the_reorder_refs() {
        let ssh = ManagedHost::ssh(ssh_host("router", None));
        let telnet = ManagedHost::profile(telnet_host("console", None));
        let serial = ManagedHost::profile(serial_host("rack"));
        assert_eq!(managed_host_key(&ssh), "ssh:router");
        assert_eq!(managed_host_key(&telnet), "profile:telnet-console");
        assert_eq!(
            managed_host_ref(&ssh),
            ManagedHostRef::Ssh {
                alias: "router".into()
            }
        );
        assert_eq!(
            managed_host_ref(&serial),
            ManagedHostRef::Profile {
                id: "serial-rack".into()
            }
        );
        assert_eq!(
            managed_host_ref_key(&managed_host_ref(&serial)),
            "profile:serial-rack"
        );
    }

    #[test]
    fn offers_a_per_kind_copy_command() {
        let ssh = ManagedHost::ssh(ssh_host("router", None));
        let telnet = ManagedHost::profile(telnet_host("console", None));
        let serial = ManagedHost::profile(serial_host("rack"));
        let native = native_ssh_host("core");
        assert_eq!(
            managed_host_copy_command(&ssh, &[]),
            CopyCommand {
                label: "Copy ssh command",
                text: "ssh router".into()
            }
        );
        assert_eq!(
            managed_host_copy_command(&telnet, &[]),
            CopyCommand {
                label: "Copy telnet command",
                text: "telnet console.example.test 23".into()
            }
        );
        assert_eq!(
            managed_host_copy_command(&serial, &[]),
            CopyCommand {
                label: "Copy device path",
                text: "COM3".into()
            }
        );
        assert_eq!(
            managed_host_copy_command(&ManagedHost::profile(native.clone()), &[]),
            CopyCommand {
                label: "Copy ssh command",
                text: "ssh -p 2222 admin@core.example.test".into()
            }
        );

        let routed = with_ssh(&native, |profile| {
            profile.identity_files = Some(vec!["~/.ssh/core key".into()]);
            profile.proxy_jump = Some(vec!["bastion".into(), "ops@jump.example.test:2200".into()]);
            profile.forwards = Some(vec![ConfigForward {
                kind: ForwardKind::Local,
                bind_port: 8080,
                target_host: Some("127.0.0.1".into()),
                target_port: Some(80),
            }]);
            profile.certificate_files = Some(vec!["~/.ssh/core-cert.pub".into()]);
            profile.identities_only = Some(true);
            profile.identity_agent = Some("none".into());
            profile.forward_agent = Some(true);
            profile.forward_x11 = Some(true);
            profile.password_only = Some(true);
            profile.request_tty = Some(RequestTty::Force);
            profile.strict_host_key_checking = Some(StrictHostKeyChecking::AcceptNew);
            profile.remote_command = Some("tmux new -A -s main".into());
        });
        assert_eq!(
            managed_host_copy_command(&ManagedHost::profile(routed), &[]).text,
            "ssh -p 2222 -i '~/.ssh/core key' -o CertificateFile=~/.ssh/core-cert.pub \
             -o IdentitiesOnly=yes -o IdentityAgent=none -A -X \
             -J bastion,ops@jump.example.test:2200 -o PubkeyAuthentication=no \
             -o PreferredAuthentications=keyboard-interactive,password \
             -L 8080:127.0.0.1:80 -tt -o StrictHostKeyChecking=accept-new \
             admin@core.example.test 'tmux new -A -s main'"
        );

        let proxied = with_ssh(&native, |profile| {
            profile.proxy_command = Some("cloudflared access ssh --hostname %h".into());
        });
        assert_eq!(
            managed_host_copy_command(&ManagedHost::profile(proxied), &[]).text,
            "ssh -p 2222 -o 'ProxyCommand=cloudflared access ssh --hostname %h' admin@core.example.test"
        );
    }

    #[test]
    fn spells_saved_muxus_jump_hosts_out_in_the_copied_ssh_command() {
        let bastion = with_ssh(&native_ssh_host("bastion"), |profile| {
            profile.user = Some("jump".into());
            profile.port = None;
        });
        let bastion_id = bastion.id.clone();
        let relay = with_ssh(&native_ssh_host("relay"), |profile| {
            profile.proxy_jump = Some(vec![saved_host_hop(&bastion_id)]);
        });
        let relay_id = relay.id.clone();
        let routed = with_ssh(&native_ssh_host("core"), |profile| {
            profile.proxy_jump = Some(vec![
                saved_host_hop(&relay_id),
                "ops@edge.example.test".into(),
                saved_host_hop("deleted"),
            ]);
        });
        assert_eq!(
            managed_host_copy_command(&ManagedHost::profile(routed), &[bastion, relay]).text,
            "ssh -p 2222 -J jump@bastion.example.test,admin@relay.example.test:2222,ops@edge.example.test \
             admin@core.example.test"
        );
    }

    #[test]
    fn names_saved_muxus_jump_hosts_in_the_hover_card() {
        let mut bastion = native_ssh_host("bastion");
        bastion.metadata.display_name = Some("Bastion EU".into());
        let bastion_id = bastion.id.clone();
        let routed = with_ssh(&native_ssh_host("core"), |profile| {
            profile.proxy_jump = Some(vec![
                saved_host_hop(&bastion_id),
                "edge".into(),
                saved_host_hop("deleted"),
            ]);
        });
        assert_eq!(
            host_detail_lines(&ManagedHost::profile(routed), Some(&[bastion]))[0],
            "via Bastion EU → edge → Deleted Muxus host"
        );
    }

    #[test]
    fn offers_sftp_only_for_ssh_hosts_where_it_is_not_explicitly_disabled() {
        let mut disabled = ssh_host("console", None);
        disabled.metadata = Some(HostMetadata {
            profile_id: "ssh-console".into(),
            disable_sftp: Some(true),
            ..Default::default()
        });
        let native = native_ssh_host("core");
        assert!(managed_host_supports_sftp(&ManagedHost::ssh(ssh_host(
            "router", None
        ))));
        assert!(managed_host_supports_sftp(&ManagedHost::profile(
            native.clone()
        )));
        assert!(!managed_host_supports_sftp(&ManagedHost::ssh(disabled)));

        let mut console_compatible = ssh_host("console-compatible", None);
        console_compatible.metadata = Some(HostMetadata {
            profile_id: "ssh-console-compatible".into(),
            console_compatibility: Some(true),
            ..Default::default()
        });
        assert!(!managed_host_supports_sftp(&ManagedHost::ssh(
            console_compatible
        )));
        let mut native_disabled = native.clone();
        native_disabled.metadata.disable_sftp = Some(true);
        assert!(!managed_host_supports_sftp(&ManagedHost::profile(
            native_disabled
        )));
        assert!(!managed_host_supports_sftp(&ManagedHost::profile(
            telnet_host("console", None)
        )));
        assert!(!managed_host_supports_sftp(&ManagedHost::profile(
            serial_host("rack")
        )));
    }

    #[test]
    fn sorts_mixed_host_kinds_by_their_sidebar_display_name_without_changing_the_input() {
        let router = ManagedHost::ssh(ssh_host("router", None));
        let console = ManagedHost::profile(telnet_host("Console", None));
        let mut db = ssh_host("db-primary", None);
        db.metadata = Some(HostMetadata {
            profile_id: "ssh-db-primary".into(),
            display_name: Some("Primary database".into()),
            ..Default::default()
        });
        let database = ManagedHost::ssh(db);
        let hosts = vec![router.clone(), database.clone(), console.clone()];
        let names: Vec<_> = alphabetize_managed_hosts(&hosts)
            .iter()
            .map(|h| managed_host_display_name(h).to_string())
            .collect();
        assert_eq!(names, ["Console", "Primary database", "router"]);
        assert_eq!(hosts, vec![router, database, console]);
    }

    // host-search.test.ts: best match ranking.
    fn searchable(alias: &str, group: Option<&str>, hostname: Option<&str>) -> ManagedHost {
        let mut entry = ssh_host(alias, group);
        if entry.metadata.is_none() {
            entry.metadata = Some(HostMetadata {
                profile_id: format!("ssh-{alias}"),
                ..Default::default()
            });
        }
        if let Some(hostname) = hostname {
            entry.resolved.hostname = hostname.into();
        }
        ManagedHost::ssh(entry)
    }

    #[test]
    fn ranks_an_exact_name_over_a_prefix_and_a_prefix_over_a_metadata_hit() {
        let hosts = vec![
            searchable("lab-gateway", None, None),
            searchable("gateway-backup", None, None),
            searchable("router", Some("gateway"), None),
            searchable("gateway", None, None),
        ];
        let key = |found: Option<&ManagedHost>| found.map(managed_host_key);
        assert_eq!(
            key(best_managed_host_match(&hosts, "gateway")),
            Some("ssh:gateway".into())
        );
        assert_eq!(
            key(best_managed_host_match(&hosts[..3], "gateway")),
            Some("ssh:gateway-backup".into())
        );
        assert_eq!(
            key(best_managed_host_match(&hosts[2..3], "gateway")),
            Some("ssh:router".into())
        );
    }

    #[test]
    fn keeps_the_first_host_on_an_even_tie() {
        let hosts = vec![
            searchable("edge-a", None, Some("core.example.test")),
            searchable("edge-b", None, Some("core.example.test")),
        ];
        assert!(std::ptr::eq(
            best_managed_host_match(&hosts, "core").unwrap(),
            &hosts[0]
        ));
    }

    #[test]
    fn has_no_answer_for_an_empty_query_or_a_query_nothing_matches() {
        let hosts = vec![searchable("router", None, None)];
        assert!(best_managed_host_match(&hosts, "").is_none());
        assert!(best_managed_host_match(&hosts, "nothing-like-this").is_none());
    }

    #[test]
    fn identifies_the_winner_by_the_same_key_the_tree_rows_carry() {
        let hosts = vec![searchable("router", None, None)];
        assert_eq!(
            managed_host_key(best_managed_host_match(&hosts, "router").unwrap()),
            "ssh:router"
        );
    }

    #[test]
    fn spells_a_gnmic_command() {
        let profile = GnmiProfile {
            profile_id: None,
            host: "leaf1".into(),
            port: 57400,
            username: Some("admin".into()),
            ssh_gateway: None,
            tls: Some(GnmiTlsMode::SkipVerify),
            tls_server_name: None,
            ca_file: None,
            cert_file: None,
            key_file: None,
            encoding: None,
        };
        assert_eq!(
            gnmic_command(&profile),
            "gnmic -a leaf1:57400 -u admin -p \"$GNMIC_PASSWORD\" --skip-verify capabilities"
        );
    }
}
