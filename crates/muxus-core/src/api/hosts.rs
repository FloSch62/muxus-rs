//! Hosts: saved hosts, OpenSSH host blocks, Muxus metadata, keyword
//! highlighting, login sequences, folder defaults and SSH keys.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use super::ForwardType;
use crate::js::Patch;
use crate::protocol::{RequestTty, SavedHostSessionProfile, StrictHostKeyChecking};
use crate::validate::{self, Validate, ValidationError, join};

/// The kinds a saved host can have (everything but local shells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedHostKind {
    Ssh,
    Telnet,
    Serial,
    Rdp,
    Vnc,
    Gnmi,
    Netconf,
}

impl SavedHostKind {
    pub const ALL: [SavedHostKind; 7] = [
        SavedHostKind::Ssh,
        SavedHostKind::Serial,
        SavedHostKind::Telnet,
        SavedHostKind::Rdp,
        SavedHostKind::Vnc,
        SavedHostKind::Gnmi,
        SavedHostKind::Netconf,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SavedHostKind::Ssh => "ssh",
            SavedHostKind::Telnet => "telnet",
            SavedHostKind::Serial => "serial",
            SavedHostKind::Rdp => "rdp",
            SavedHostKind::Vnc => "vnc",
            SavedHostKind::Gnmi => "gnmi",
            SavedHostKind::Netconf => "netconf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// SSH/Telnet/serial/RDP/VNC/gNMI/NETCONF host stored natively by Muxus rather than in ssh_config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedHostProfile {
    pub id: String,
    pub kind: SavedHostKind,
    pub name: String,
    pub profile: SavedHostSessionProfile,
    pub metadata: OpenSshProfileMetadata,
    pub created_at: String,
    pub updated_at: String,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedHostProfileInput {
    /// Omit for a generated ID; supply for idempotent import/restore upserts.
    pub id: Option<String>,
    pub name: String,
    pub profile: SavedHostSessionProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedHostProfilesResponse {
    pub profiles: Vec<SavedHostProfile>,
}

/// One sidebar host, addressable across both host sources.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ManagedHostRef {
    Ssh { alias: String },
    Profile { id: String },
}

/// One complete visual order for a sidebar group, mixing both host sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOrderRequest {
    pub hosts: Vec<ManagedHostRef>,
}

/// A LocalForward / RemoteForward / DynamicForward rule declared in ssh config.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigForward {
    #[serde(rename = "type")]
    pub kind: ForwardType,
    /// Listen port — local port for local/dynamic, remote bind port for remote.
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
}

/// Prefix of a ProxyJump hop that names a saved Muxus SSH host instead of an
/// ssh_config alias or `user@host:port`. The colon keeps it out of OpenSSH's
/// hop grammar (a port is numeric; "muxus-host" is not an IPv6 address), so
/// it only ever appears in Muxus-owned profiles, never in ssh_config.
const SAVED_HOST_HOP_PREFIX: &str = "muxus-host:";

/// ProxyJump hop that reaches the next host through a saved Muxus SSH host.
pub fn saved_host_hop(profile_id: &str) -> String {
    format!("{SAVED_HOST_HOP_PREFIX}{profile_id}")
}

/// Saved Muxus SSH host a ProxyJump hop names, or `None` for ssh hops.
pub fn saved_host_hop_id(hop: &str) -> Option<&str> {
    hop.strip_prefix(SAVED_HOST_HOP_PREFIX)
        .filter(|id| !id.is_empty())
}

/// One option Muxus does not model, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostOptionExtra {
    pub keyword: String,
    pub value: String,
}

/// Options written in one Host block — exactly what the editor round-trips.
/// Anything Muxus doesn't model stays verbatim in `extras`, so editing a
/// block never drops hand-written options.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostBlockOptions {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Option<Vec<String>>,
    /// OpenSSH user certificates paired to matching IdentityFile keys.
    pub certificate_files: Option<Vec<String>>,
    pub identities_only: Option<bool>,
    /// Per-host authentication agent: socket path, environment indirection, SSH_AUTH_SOCK, or none.
    pub identity_agent: Option<String>,
    pub forward_agent: Option<bool>,
    /// ForwardX11; absent = the Muxus default (on with the bundled Windows X server).
    #[serde(rename = "forwardX11")]
    pub forward_x11: Option<bool>,
    /// ProxyJump hops in order ("bastion", "user@host:2222"); absent = none.
    pub proxy_jump: Option<Vec<String>>,
    /// Shell command whose stdin/stdout provide the SSH transport.
    pub proxy_command: Option<String>,
    pub forwards: Option<Vec<ConfigForward>>,
    /// Skip public keys and go straight to password/keyboard-interactive.
    pub password_only: Option<bool>,
    /// Command to run after connecting; `none` explicitly restores a login shell.
    pub remote_command: Option<String>,
    pub request_tty: Option<RequestTty>,
    pub strict_host_key_checking: Option<StrictHostKeyChecking>,
    /// Unmodeled options, order-preserved ("Compression yes", …).
    pub extras: Option<Vec<HostOptionExtra>>,
}

/// Effective settings after full Host-pattern resolution (what connect uses).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedHostSettings {
    pub hostname: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_files: Vec<String>,
    /// User certificates paired with matching private IdentityFile keys.
    pub certificate_files: Vec<String>,
    pub identities_only: bool,
    /// Effective authentication agent after applying all matching Host blocks.
    pub identity_agent: Option<String>,
    pub forward_agent: bool,
    /// Effective ForwardX11; `None` when no matching block sets it.
    #[serde(rename = "forwardX11")]
    pub forward_x11: Option<bool>,
    pub proxy_jump: Vec<String>,
    /// Raw ProxyCommand after Host-pattern resolution; tokens expand at dial time.
    pub proxy_command: Option<String>,
    pub forwards: Vec<ConfigForward>,
    pub password_only: bool,
}

/// One connectable Host entry parsed from ~/.ssh/config.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHostEntry {
    /// Primary alias — the block's first concrete Host pattern.
    pub alias: String,
    /// Every concrete alias on the block's Host line (includes `alias`).
    pub aliases: Vec<String>,
    /// Comment lines sitting directly above the Host block.
    pub description: Option<String>,
    /// Absolute path of the config file the block lives in.
    pub file: String,
    /// The block's own options (editing target).
    pub options: HostBlockOptions,
    /// Effective values with `Host *`-style defaults applied (display + connect).
    pub resolved: ResolvedHostSettings,
    /// Muxus-only metadata. Connection details still resolve live from OpenSSH.
    pub metadata: Option<OpenSshProfileMetadata>,
}

/// One terminal keyword or pattern and the colors used to render every match.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordHighlightRule {
    /// Stable client-generated id used while editing and reordering rules.
    pub id: String,
    /// Optional label saying what the rule is for, e.g. "IPv4 addresses".
    pub name: Option<String>,
    /// Literal text, or a JavaScript regular expression source when `regex` is set.
    pub keyword: String,
    /// An opaque `#RRGGBB` color.
    pub foreground: String,
    pub background: Option<String>,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Absent in rules saved before regex support; treated as a literal keyword.
    pub regex: Option<bool>,
}

impl KeywordHighlightRule {
    /// A new rule's colors (`DEFAULT_RULE` of `KeywordHighlightRulesEditor.tsx`: white on
    /// dark red).
    pub const DEFAULT_FOREGROUND: &str = "#ffffff";
    pub const DEFAULT_BACKGROUND: &str = "#b91c1c";
    /// The background a rule gets when its background switch is turned on.
    pub const BACKGROUND_ON: &str = "#713f12";
}

/// A named, reusable rule set that can be assigned to any saved host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeywordHighlightProfile {
    /// Stable across export/import so assigned hosts keep following the profile.
    pub id: String,
    pub name: String,
    pub rules: Vec<KeywordHighlightRule>,
}

/// Host rules are additive by default, but can replace the global rule set.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeywordHighlightConfig {
    pub inherit_global: bool,
    /// Reusable profile applied before this host's own rules.
    pub profile_id: Option<String>,
    pub rules: Vec<KeywordHighlightRule>,
}

/// A `wait` step: the sequence waits for the pattern, or stops after the timeout.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginWaitStep {
    pub id: String,
    /// Literal text, or a JavaScript regular expression source when `regex` is set.
    pub pattern: String,
    pub regex: Option<bool>,
    /// The sequence stops when nothing matches within this many seconds.
    pub timeout_seconds: u32,
}

/// A `send` step: types text, then Enter when asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSendStep {
    pub id: String,
    /// May be empty when the step only presses Enter.
    pub text: String,
    pub enter: bool,
}

/// A `secret` step: types a named vault secret, never stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSecretStep {
    pub id: String,
    /// A named secret in the password vault; the value itself is never stored here.
    pub secret_id: String,
    pub enter: bool,
}

/// One step of a login sequence. Ids are client-generated, for editing and reordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginSequenceStep {
    Wait(LoginWaitStep),
    Send(LoginSendStep),
    Secret(LoginSecretStep),
}

impl LoginSequenceStep {
    pub fn id(&self) -> &str {
        match self {
            LoginSequenceStep::Wait(step) => &step.id,
            LoginSequenceStep::Send(step) => &step.id,
            LoginSequenceStep::Secret(step) => &step.id,
        }
    }
}

// Written as zod outputs it (`id` before `kind`), so stored JSON matches the
// TypeScript byte for byte.
impl Serialize for LoginSequenceStep {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[skip_serializing_none]
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wait<'a> {
            id: &'a str,
            kind: &'static str,
            pattern: &'a str,
            regex: Option<bool>,
            timeout_seconds: u32,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Send<'a> {
            id: &'a str,
            kind: &'static str,
            text: &'a str,
            enter: bool,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Secret<'a> {
            id: &'a str,
            kind: &'static str,
            secret_id: &'a str,
            enter: bool,
        }
        match self {
            LoginSequenceStep::Wait(step) => Wait {
                id: &step.id,
                kind: "wait",
                pattern: &step.pattern,
                regex: step.regex,
                timeout_seconds: step.timeout_seconds,
            }
            .serialize(serializer),
            LoginSequenceStep::Send(step) => Send {
                id: &step.id,
                kind: "send",
                text: &step.text,
                enter: step.enter,
            }
            .serialize(serializer),
            LoginSequenceStep::Secret(step) => Secret {
                id: &step.id,
                kind: "secret",
                secret_id: &step.secret_id,
                enter: step.enter,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for LoginSequenceStep {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "lowercase")]
        enum Wire {
            Wait(LoginWaitStep),
            Send(LoginSendStep),
            Secret(LoginSecretStep),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Wait(step) => LoginSequenceStep::Wait(step),
            Wire::Send(step) => LoginSequenceStep::Send(step),
            Wire::Secret(step) => LoginSequenceStep::Secret(step),
        })
    }
}

/// Steps a terminal session runs once after it connects, and again after
/// every reconnect: wait for a prompt, then answer it. A host without its own
/// sequence inherits its nearest folder's; an empty list is an explicit
/// "none" that stops a folder's sequence from applying.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LoginSequence {
    pub steps: Vec<LoginSequenceStep>,
}

pub const LOGIN_SEQUENCE_MAX_STEPS: usize = 32;
/// Longest wait pattern or sent text, in characters.
pub const LOGIN_SEQUENCE_TEXT_MAX_LENGTH: usize = 1024;
pub const LOGIN_SEQUENCE_DEFAULT_TIMEOUT_SECONDS: u32 = 10;
pub const LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS: u32 = 3600;

/// Muxus-only metadata for an OpenSSH alias or a saved host.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSshProfileMetadata {
    /// Stable local ID survives an OpenSSH alias rename.
    pub profile_id: String,
    /// User-defined position inside the host's current sidebar group.
    pub sort_order: Option<i64>,
    pub display_name: Option<String>,
    /// Muxus-only organizational group; does not alter the OpenSSH config file.
    pub group: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
    /// Built-in terminal color scheme selected for this host.
    pub terminal_scheme: Option<String>,
    /// Host-specific terminal foreground and background overrides.
    pub terminal_font_color: Option<String>,
    pub terminal_background_color: Option<String>,
    /// Muxus-only terminal highlighting for this host.
    pub keyword_highlights: Option<HostKeywordHighlightConfig>,
    /// Command button group the bar switches to while this host's session is active.
    pub command_button_group: Option<String>,
    /// Paste pacing for this host's sessions; absent follows the Terminal settings.
    pub paste_line_delay_ms: Option<u32>,
    pub paste_char_delay_ms: Option<u32>,
    /// Do not open SFTP channels or probe for remote Unix shell integration.
    /// Present only when set.
    pub disable_sftp: Option<bool>,
    /// Console appliances: also suppress env requests and tolerate a rejected PTY.
    /// Present only when set.
    pub console_compatibility: Option<bool>,
    /// The host's own login sequence; absent inherits the folder's.
    pub login_sequence: Option<LoginSequence>,
    pub last_connected_at: Option<String>,
    pub connect_count: u64,
}

/// A partial metadata update: absent keeps, `null` clears, a value sets.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSshMetadataPatch {
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub display_name: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub group: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub color: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub icon: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub terminal_scheme: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub terminal_font_color: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub terminal_background_color: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub keyword_highlights: Patch<HostKeywordHighlightConfig>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub command_button_group: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub paste_line_delay_ms: Patch<u32>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub paste_char_delay_ms: Patch<u32>,
    pub disable_sftp: Option<bool>,
    pub console_compatibility: Option<bool>,
    /// `null` goes back to the folder's sequence.
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub login_sequence: Patch<LoginSequence>,
}

impl OpenSshMetadataPatch {
    /// Whether the patch names at least one field (the route rejects empty patches).
    pub fn is_empty(&self) -> bool {
        self.display_name.is_keep()
            && self.group.is_keep()
            && self.color.is_keep()
            && self.icon.is_keep()
            && self.terminal_scheme.is_keep()
            && self.terminal_font_color.is_keep()
            && self.terminal_background_color.is_keep()
            && self.keyword_highlights.is_keep()
            && self.command_button_group.is_keep()
            && self.paste_line_delay_ms.is_keep()
            && self.paste_char_delay_ms.is_keep()
            && self.disable_sftp.is_none()
            && self.console_compatibility.is_none()
            && self.login_sequence.is_keep()
    }
}

/// Shared SSH defaults a sidebar folder hands to the hosts inside it. Every
/// field is optional: a folder only overrides what it sets. At connect time
/// these apply *below* everything in ssh_config — a host's own options and
/// `Host *` blocks always win — with the nearest folder beating its
/// ancestors. The folder password is not here; it lives in the encrypted vault.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderAuthSettings {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Option<Vec<String>>,
    pub identities_only: Option<bool>,
    /// Agent socket override: a path, `$VAR`/`${VAR}`, `SSH_AUTH_SOCK`, or `none`.
    pub identity_agent: Option<String>,
    pub forward_agent: Option<bool>,
}

impl FolderAuthSettings {
    /// Whether no field is set (`Object.keys(auth).length === 0`).
    pub fn is_empty(&self) -> bool {
        self == &FolderAuthSettings::default()
    }

    /// Drop empty strings and empty lists so "cleared" fields don't linger
    /// (`cleanAuth` in the folders route).
    pub fn cleaned(&self) -> FolderAuthSettings {
        let user = self
            .user
            .as_deref()
            .map(crate::js::js_trim)
            .filter(|v| !v.is_empty());
        let identity_files: Vec<String> = self
            .identity_files
            .iter()
            .flatten()
            .map(|file| crate::js::js_trim(file).to_owned())
            .filter(|file| !file.is_empty())
            .collect();
        let identity_agent = self
            .identity_agent
            .as_deref()
            .map(crate::js::js_trim)
            .filter(|v| !v.is_empty());
        FolderAuthSettings {
            user: user.map(str::to_owned),
            port: self.port,
            identity_files: (!identity_files.is_empty()).then_some(identity_files),
            identities_only: self.identities_only,
            identity_agent: identity_agent.map(str::to_owned),
            forward_agent: self.forward_agent,
        }
    }
}

/// One folder's stored credential defaults.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSettingsRecord {
    /// Stable ID — the vault password stays attached across folder renames.
    pub id: String,
    /// Normalized folder path ("Production/EU"); matched case-insensitively.
    pub path: String,
    pub auth: FolderAuthSettings,
    /// A shared password for this folder exists in the password vault.
    pub has_password: bool,
    /// Login sequence for hosts inside that have none of their own; absent inherits the parent's.
    pub login_sequence: Option<LoginSequence>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSettingsResponse {
    pub folders: Vec<FolderSettingsRecord>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigResponse {
    /// Root config path (~/.ssh/config).
    pub path: String,
    /// All config files read — root first, includes in evaluation order.
    pub files: Vec<String>,
    pub hosts: Vec<SshHostEntry>,
    /// First parse problem encountered; hosts may be partial.
    pub error: Option<String>,
}

/// Create/update a Host block in ssh config.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostUpsertRequest {
    /// Concrete aliases for the Host line (usually exactly one).
    pub aliases: Vec<String>,
    pub description: Option<String>,
    /// Config file to write to; defaults to the edited block's file or the root config.
    pub file: Option<String>,
    pub options: HostBlockOptions,
    /// When editing: primary alias of the block being replaced.
    pub previous_alias: Option<String>,
}

/// Serialized Host block exactly as it would be written to the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPreviewResponse {
    pub text: String,
}

/// Connection options a bulk edit can set on many SSH hosts at once. A value
/// sets the option on every host; `null` removes it so the host inherits
/// again. Options left out stay exactly as each host has them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostOptionsPatch {
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub user: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub port: Patch<u16>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub forward_agent: Patch<bool>,
    #[serde(default, skip_serializing_if = "Patch::is_keep", rename = "forwardX11")]
    pub forward_x11: Patch<bool>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub strict_host_key_checking: Patch<StrictHostKeyChecking>,
}

/// Body of PATCH /api/ssh/config/hosts: one patch for the blocks of many aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBulkUpdateRequest {
    pub aliases: Vec<String>,
    pub options: HostOptionsPatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBulkUpdateResponse {
    /// Host blocks rewritten; aliases that share a block count once.
    pub updated: usize,
}

/// Body of DELETE /api/ssh/config/hosts: the Host blocks of many aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBulkDeleteRequest {
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBulkDeleteResponse {
    /// Host blocks removed; aliases that share a block count once.
    pub deleted: usize,
}

/// Private key discovered in ~/.ssh.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeyInfo {
    pub path: String,
    /// File name ("id_ed25519").
    pub name: String,
    /// Public key algorithm when known ("ssh-ed25519").
    #[serde(rename = "type")]
    pub key_type: Option<String>,
    /// Comment from the sibling .pub file.
    pub comment: Option<String>,
    pub encrypted: bool,
    /// Currently loaded in the SSH agent.
    pub in_agent: bool,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshAgentKey {
    #[serde(rename = "type")]
    pub key_type: String,
    pub comment: Option<String>,
    /// SHA256:… fingerprint, OpenSSH presentation.
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeysResponse {
    pub agent_available: bool,
    pub agent_keys: Vec<SshAgentKey>,
    pub keys: Vec<SshKeyInfo>,
}

// ---------------------------------------------------------------------------
// Route validation (`server/src/routes/metadata-schema.ts`, `profiles.ts`)
// ---------------------------------------------------------------------------

/// `/^#[0-9a-fA-F]{6}$/`: an opaque color the terminal can draw with.
fn is_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn hex_color(path: &str, value: &str) -> Result<(), ValidationError> {
    validate::pattern(path, value, is_hex_color(value), "/^#[0-9a-fA-F]{6}$/")
}

/// Whether `new RegExp(pattern)` accepts the pattern (no flags, so the lenient
/// Annex B grammar: a lone `{` or `]` is a literal). Covers the errors that
/// grammar has: a trailing backslash, unterminated classes and groups, an
/// unmatched `)`, invalid group syntax, out-of-order class ranges and
/// quantifier bounds, and quantifiers with nothing (or a lookbehind) to repeat.
pub fn is_valid_js_regex(pattern: &str) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Last {
        /// Start of the pattern, a group or an alternative.
        Nothing,
        Atom,
        /// An assertion that cannot be repeated (`^`, `$`, `\b`, a lookbehind).
        Fixed,
        Quantifier,
        LazyQuantifier,
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut groups: Vec<bool> = Vec::new(); // true for lookbehinds
    let mut last = Last::Nothing;
    let mut i = 0;
    // `{n}`, `{n,}` or `{n,m}` at `i`: its end and whether the bounds are in order.
    let braced_quantifier = |i: usize| -> Option<(usize, bool)> {
        let mut j = i + 1;
        let digits = |j: &mut usize| {
            let start = *j;
            let mut value: u64 = 0;
            while let Some(c) = chars.get(*j).and_then(|c| c.to_digit(10)) {
                value = value.saturating_mul(10).saturating_add(u64::from(c));
                *j += 1;
            }
            (*j > start).then_some(value)
        };
        let min = digits(&mut j)?;
        let mut max = Some(min);
        if chars.get(j) == Some(&',') {
            j += 1;
            max = digits(&mut j);
        }
        (chars.get(j) == Some(&'}')).then(|| (j + 1, max.is_none_or(|max| min <= max)))
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&next) = chars.get(i + 1) else {
                    return false;
                };
                last = if matches!(next, 'b' | 'B') {
                    Last::Fixed
                } else {
                    Last::Atom
                };
                i += 2;
            }
            '[' => {
                let mut j = i + 1;
                if chars.get(j) == Some(&'^') {
                    j += 1;
                }
                let mut previous: Option<char> = None;
                let mut closed = false;
                while j < chars.len() {
                    match chars[j] {
                        ']' => {
                            closed = true;
                            break;
                        }
                        '\\' => {
                            if j + 1 >= chars.len() {
                                return false;
                            }
                            previous = None;
                            j += 2;
                        }
                        '-' if previous.is_some()
                            && chars
                                .get(j + 1)
                                .is_some_and(|&end| end != ']' && end != '\\') =>
                        {
                            if previous.is_some_and(|start| start > chars[j + 1]) {
                                return false;
                            }
                            previous = None;
                            j += 2;
                        }
                        other => {
                            previous = Some(other);
                            j += 1;
                        }
                    }
                }
                if !closed {
                    return false;
                }
                last = Last::Atom;
                i = j + 1;
            }
            '(' => {
                let mut lookbehind = false;
                if chars.get(i + 1) == Some(&'?') {
                    match (chars.get(i + 2), chars.get(i + 3)) {
                        (Some(':' | '=' | '!'), _) => i += 3,
                        (Some('<'), Some('=' | '!')) => {
                            lookbehind = true;
                            i += 4;
                        }
                        (Some('<'), _) => {
                            let start = i + 3;
                            let mut j = start;
                            while chars
                                .get(j)
                                .is_some_and(|&c| c.is_alphanumeric() || c == '_' || c == '$')
                            {
                                j += 1;
                            }
                            let name_ok = j > start && !chars[start].is_ascii_digit();
                            if !name_ok || chars.get(j) != Some(&'>') {
                                return false;
                            }
                            i = j + 1;
                        }
                        _ => return false,
                    }
                } else {
                    i += 1;
                }
                groups.push(lookbehind);
                last = Last::Nothing;
            }
            ')' => {
                let Some(lookbehind) = groups.pop() else {
                    return false;
                };
                last = if lookbehind { Last::Fixed } else { Last::Atom };
                i += 1;
            }
            '|' => {
                last = Last::Nothing;
                i += 1;
            }
            '^' | '$' => {
                last = Last::Fixed;
                i += 1;
            }
            '*' | '+' | '?' => {
                last = match (last, c) {
                    (Last::Atom, _) => Last::Quantifier,
                    (Last::Quantifier, '?') => Last::LazyQuantifier,
                    _ => return false,
                };
                i += 1;
            }
            '{' => match braced_quantifier(i) {
                Some((end, ordered)) => {
                    if !ordered || last != Last::Atom {
                        return false;
                    }
                    last = Last::Quantifier;
                    i = end;
                }
                None => {
                    last = Last::Atom;
                    i += 1;
                }
            },
            _ => {
                last = Last::Atom;
                i += 1;
            }
        }
    }
    groups.is_empty()
}

impl Validate for KeywordHighlightRule {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "id"), &self.id, 1, 100)?;
        validate::opt_string(&join(path, "name"), &self.name, 1, 100)?;
        validate::string(&join(path, "keyword"), &self.keyword, 1, 500)?;
        hex_color(&join(path, "foreground"), &self.foreground)?;
        if let Some(background) = &self.background {
            hex_color(&join(path, "background"), background)?;
        }
        Ok(())
    }
}

impl Validate for HostKeywordHighlightConfig {
    /// `hostKeywordHighlightsSchema`.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "profileId"), &self.profile_id, 1, 200)?;
        validate::items(&join(path, "rules"), self.rules.len(), 0, 100)?;
        self.rules.validate_at(&join(path, "rules"))
    }
}

impl Validate for LoginSequenceStep {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "id"), self.id(), 1, 100)?;
        match self {
            LoginSequenceStep::Wait(step) => {
                validate::string(
                    &join(path, "pattern"),
                    &step.pattern,
                    1,
                    LOGIN_SEQUENCE_TEXT_MAX_LENGTH,
                )?;
                validate::range(
                    &join(path, "timeoutSeconds"),
                    step.timeout_seconds,
                    1,
                    LOGIN_SEQUENCE_MAX_TIMEOUT_SECONDS,
                )?;
                if step.regex == Some(true) && !is_valid_js_regex(&step.pattern) {
                    return Err(ValidationError::new(
                        join(path, "pattern"),
                        "invalid regular expression",
                    ));
                }
                Ok(())
            }
            LoginSequenceStep::Send(step) => {
                validate::string(
                    &join(path, "text"),
                    &step.text,
                    0,
                    LOGIN_SEQUENCE_TEXT_MAX_LENGTH,
                )?;
                if step.text.is_empty() && !step.enter {
                    return Err(ValidationError::new(
                        join(path, "text"),
                        "a send step needs text or Enter",
                    ));
                }
                Ok(())
            }
            LoginSequenceStep::Secret(step) => {
                validate::string(&join(path, "secretId"), &step.secret_id, 1, 200)
            }
        }
    }
}

impl Validate for LoginSequence {
    /// `loginSequenceSchema`: steps only; a secret step names its vault secret
    /// and never carries the value.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::items(
            &join(path, "steps"),
            self.steps.len(),
            0,
            LOGIN_SEQUENCE_MAX_STEPS,
        )?;
        self.steps.validate_at(&join(path, "steps"))
    }
}

impl Validate for OpenSshMetadataPatch {
    /// `metadataPatchSchema`: Muxus-owned display metadata, shared by OpenSSH
    /// hosts and saved profiles.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        use crate::paste_pacing::{MAX_PASTE_CHAR_DELAY_MS, MAX_PASTE_LINE_DELAY_MS};
        let text = |key: &str, value: &Patch<String>, min: usize, max: usize| match value {
            Patch::Set(value) => validate::string(&join(path, key), value, min, max),
            _ => Ok(()),
        };
        text("displayName", &self.display_name, 0, 200)?;
        // A group is a folder path ("Production/EU/Edge"), so the cap has to
        // cover several nested names rather than a single one.
        text("group", &self.group, 0, 300)?;
        text("color", &self.color, 0, 64)?;
        text("icon", &self.icon, 0, 64)?;
        text("terminalScheme", &self.terminal_scheme, 1, 64)?;
        for (key, value) in [
            ("terminalFontColor", &self.terminal_font_color),
            ("terminalBackgroundColor", &self.terminal_background_color),
        ] {
            if let Patch::Set(value) = value {
                hex_color(&join(path, key), value)?;
            }
        }
        if let Patch::Set(highlights) = &mut self.keyword_highlights {
            highlights.validate_at(&join(path, "keywordHighlights"))?;
        }
        // The id of a command button group, which lives in the preferences.
        text("commandButtonGroup", &self.command_button_group, 1, 200)?;
        if let Patch::Set(delay) = self.paste_line_delay_ms {
            validate::range(
                &join(path, "pasteLineDelayMs"),
                delay,
                0,
                MAX_PASTE_LINE_DELAY_MS,
            )?;
        }
        if let Patch::Set(delay) = self.paste_char_delay_ms {
            validate::range(
                &join(path, "pasteCharDelayMs"),
                delay,
                0,
                MAX_PASTE_CHAR_DELAY_MS,
            )?;
        }
        if let Patch::Set(sequence) = &mut self.login_sequence {
            sequence.validate_at(&join(path, "loginSequence"))?;
        }
        if self.is_empty() {
            return Err(ValidationError::new(
                path,
                "at least one metadata field is required",
            ));
        }
        Ok(())
    }
}

impl Validate for SavedHostProfileInput {
    /// `savedProfileSchema` of the saved-host route.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::opt_string(&join(path, "id"), &self.id, 1, 200)?;
        validate::trimmed(&join(path, "name"), &mut self.name, 1, 200)?;
        self.profile.validate_at(&join(path, "profile"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_steps_keep_the_zod_key_order() {
        let json = r#"{"steps":[{"id":"a","kind":"wait","pattern":"Password:","timeoutSeconds":10},{"id":"b","kind":"secret","secretId":"s1","enter":true},{"id":"c","kind":"send","text":"","enter":true}]}"#;
        let parsed: LoginSequence = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }

    #[test]
    fn saved_host_hops_round_trip() {
        assert_eq!(saved_host_hop("p1"), "muxus-host:p1");
        assert_eq!(saved_host_hop_id("muxus-host:p1"), Some("p1"));
        assert_eq!(saved_host_hop_id("muxus-host:"), None);
        assert_eq!(saved_host_hop_id("bastion"), None);
    }

    #[test]
    fn metadata_patch_keeps_absent_and_null_apart() {
        let patch: OpenSshMetadataPatch =
            serde_json::from_str(r#"{"group":null,"color":"red","disableSftp":true}"#).unwrap();
        assert_eq!(patch.group, Patch::Clear);
        assert_eq!(patch.color, Patch::Set("red".into()));
        assert!(patch.icon.is_keep());
        assert_eq!(
            serde_json::to_string(&patch).unwrap(),
            r#"{"group":null,"color":"red","disableSftp":true}"#
        );
        assert!(OpenSshMetadataPatch::default().is_empty());
    }

    #[test]
    fn folder_auth_cleaning_drops_blank_fields() {
        let auth = FolderAuthSettings {
            user: Some("  ".into()),
            identity_files: Some(vec![" ~/.ssh/id ".into(), "".into()]),
            port: Some(22),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&auth.cleaned()).unwrap(),
            r#"{"port":22,"identityFiles":["~/.ssh/id"]}"#
        );
    }
}
