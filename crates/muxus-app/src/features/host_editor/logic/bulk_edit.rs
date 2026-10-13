//! Editing many hosts at once (`client/src/host-bulk-edit.ts`): what the
//! selection shares per setting, and the writes that apply only the changed
//! settings, only where they mean something, only where they differ.

use std::collections::{BTreeMap, HashMap};

use muxus_core::api::{
    HostKeywordHighlightConfig, HostOptionsPatch, OpenSshMetadataPatch, SavedHostProfileInput,
    SessionLoggingPolicy, SessionLoggingPolicyInput,
};
use muxus_core::js::{Patch, js_trim, utf16_len};
use muxus_core::protocol::{SavedHostSessionProfile, SshProfile, StrictHostKeyChecking};
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, login_sequence_draft, login_sequence_from_draft, login_sequence_problem,
    same_login_sequence_draft,
};

use super::js::js_number;
use super::session_logging::{
    HostSessionLoggingDraft, host_session_logging_draft, same_session_logging_draft,
    session_logging_policy_input,
};
use crate::hosts::managed::{ManagedHost, managed_host_key};
use crate::hosts::tree::{MAX_FOLDER_DEPTH, MAX_GROUP_PATH, folder_depth, normalize_group_path};

/// An optional flag: unset follows the defaults, otherwise on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BulkTriState {
    Inherit,
    Yes,
    No,
}

/// Which hosts a field means anything for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BulkAudience {
    All,
    Terminal,
    Ssh,
}

/// Whether two values of one field apply the same setting.
pub trait BulkValue: Clone {
    fn same(&self, other: &Self) -> bool;
}

macro_rules! same_by_eq {
    ($($ty:ty),*) => { $(impl BulkValue for $ty { fn same(&self, other: &Self) -> bool { self == other } })* };
}
same_by_eq!(
    String,
    bool,
    BulkTriState,
    Option<String>,
    Option<u32>,
    Option<StrictHostKeyChecking>
);

impl BulkValue for HostSessionLoggingDraft {
    fn same(&self, other: &Self) -> bool {
        same_session_logging_draft(self, other)
    }
}

impl BulkValue for LoginSequenceDraft {
    fn same(&self, other: &Self) -> bool {
        same_login_sequence_draft(self, other)
    }
}

/// What the selection holds for one field.
#[derive(Debug, Clone, PartialEq)]
pub enum BulkFieldSummary<T> {
    /// No selected host has this setting (a remote desktop has no terminal colors).
    None,
    /// Not every host's value has arrived yet.
    Loading {
        count: usize,
    },
    Same {
        count: usize,
        value: T,
    },
    Mixed {
        count: usize,
    },
}

impl<T> BulkFieldSummary<T> {
    /// Hosts the field applies to.
    pub fn count(&self) -> usize {
        match self {
            BulkFieldSummary::None => 0,
            BulkFieldSummary::Loading { count }
            | BulkFieldSummary::Same { count, .. }
            | BulkFieldSummary::Mixed { count } => *count,
        }
    }
}

macro_rules! bulk_fields {
    ($($(#[$meta:meta])* $field:ident: $ty:ty = $variant:ident, $audience:ident;)*) => {
        /// Every setting the bulk editor can change.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum BulkHostField { $($variant,)* }

        impl BulkHostField {
            pub const ALL: &'static [BulkHostField] = &[$(BulkHostField::$variant,)*];

            /// `BULK_FIELD_AUDIENCE`.
            pub fn audience(self) -> BulkAudience {
                match self { $(BulkHostField::$variant => BulkAudience::$audience,)* }
            }
        }

        /// Every setting the bulk editor can change, in the shape its control
        /// edits. Each one maps onto a single stored value, so applying it to a
        /// host leaves everything else about that host exactly as it was.
        ///
        /// Used partially: as one host's values (`None` = the field does not
        /// apply, or its value has not loaded) and as the changes the user made
        /// (`None` = untouched; `Some(None)` of an optional value is "unset",
        /// a value to apply).
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct BulkHostValues { $($(#[$meta])* pub $field: Option<$ty>,)* }

        /// The changed fields; every absent field is left alone.
        pub type BulkHostChanges = BulkHostValues;

        impl BulkHostValues {
            /// `field in values`.
            pub fn has(&self, field: BulkHostField) -> bool {
                match field { $(BulkHostField::$variant => self.$field.is_some(),)* }
            }

            /// The fields present, in declaration order.
            pub fn fields(&self) -> Vec<BulkHostField> {
                BulkHostField::ALL.iter().copied().filter(|f| self.has(*f)).collect()
            }

            /// Drop one field (`withoutField`).
            pub fn remove(&mut self, field: BulkHostField) {
                match field { $(BulkHostField::$variant => self.$field = None,)* }
            }

            /// Whether `field` holds the same setting in both (both must have it).
            fn same_field(&self, other: &Self, field: BulkHostField) -> bool {
                match field {
                    $(BulkHostField::$variant => match (&self.$field, &other.$field) {
                        (Some(a), Some(b)) => a.same(b),
                        _ => false,
                    },)*
                }
            }
        }

        /// Per field, whether the selected hosts agree.
        #[derive(Debug, Clone, PartialEq)]
        pub struct BulkSummary { $(pub $field: BulkFieldSummary<$ty>,)* }

        impl BulkSummary {
            /// The summary of one field without its value.
            pub fn state(&self, field: BulkHostField) -> BulkFieldSummary<()> {
                match field {
                    $(BulkHostField::$variant => strip(&self.$field),)*
                }
            }
        }

        /// Per field, whether the selected hosts agree. `policies` holds the
        /// session logging policies loaded so far, by [`host_logging_key`].
        pub fn summarize_bulk_values(
            hosts: &[ManagedHost],
            policies: &HashMap<String, SessionLoggingPolicy>,
        ) -> BulkSummary {
            let per_host: Vec<(&ManagedHost, BulkHostValues)> = hosts
                .iter()
                .map(|host| (host, bulk_values_for_host(host, policies.get(&host_logging_key(host)))))
                .collect();
            BulkSummary {
                $($field: {
                    let applicable: Vec<&BulkHostValues> = per_host
                        .iter()
                        .filter(|(host, _)| bulk_field_applies(host, BulkHostField::$variant))
                        .map(|(_, values)| values)
                        .collect();
                    let count = applicable.len();
                    if count == 0 {
                        BulkFieldSummary::None
                    } else if applicable.iter().any(|values| values.$field.is_none()) {
                        // An unset color is a value, an unloaded policy is not.
                        BulkFieldSummary::Loading { count }
                    } else {
                        let first = applicable[0].$field.clone().unwrap_or_else(|| unreachable!());
                        if applicable.iter().all(|values| values.$field.as_ref().is_some_and(|v| v.same(&first))) {
                            BulkFieldSummary::Same { count, value: first }
                        } else {
                            BulkFieldSummary::Mixed { count }
                        }
                    }
                },)*
            }
        }
    };
}

fn strip<T>(summary: &BulkFieldSummary<T>) -> BulkFieldSummary<()> {
    match summary {
        BulkFieldSummary::None => BulkFieldSummary::None,
        BulkFieldSummary::Loading { count } => BulkFieldSummary::Loading { count: *count },
        BulkFieldSummary::Same { count, .. } => BulkFieldSummary::Same {
            count: *count,
            value: (),
        },
        BulkFieldSummary::Mixed { count } => BulkFieldSummary::Mixed { count: *count },
    }
}

bulk_fields! {
    group: String = Group, All;
    color: Option<String> = Color, All;
    user: String = User, Ssh;
    port: String = Port, Ssh;
    /// `None` inherits StrictHostKeyChecking.
    strict_host_key_checking: Option<StrictHostKeyChecking> = StrictHostKeyChecking, Ssh;
    forward_agent: BulkTriState = ForwardAgent, Ssh;
    forward_x11: BulkTriState = ForwardX11, Ssh;
    console_compatibility: bool = ConsoleCompatibility, Ssh;
    disable_sftp: bool = DisableSftp, Ssh;
    terminal_scheme: Option<String> = TerminalScheme, Terminal;
    terminal_font_color: Option<String> = TerminalFontColor, Terminal;
    terminal_background_color: Option<String> = TerminalBackgroundColor, Terminal;
    command_button_group: Option<String> = CommandButtonGroup, Terminal;
    paste_line_delay_ms: Option<u32> = PasteLineDelayMs, Terminal;
    paste_char_delay_ms: Option<u32> = PasteCharDelayMs, Terminal;
    highlight_profile_id: Option<String> = HighlightProfileId, Terminal;
    highlight_inherit_global: bool = HighlightInheritGlobal, Terminal;
    session_logging: HostSessionLoggingDraft = SessionLogging, Terminal;
    login_sequence: LoginSequenceDraft = LoginSequence, Terminal;
}

const CONNECTION_FIELDS: [BulkHostField; 5] = [
    BulkHostField::User,
    BulkHostField::Port,
    BulkHostField::StrictHostKeyChecking,
    BulkHostField::ForwardAgent,
    BulkHostField::ForwardX11,
];

/// Whether a host is reached over SSH, from either host source.
pub fn is_ssh_host(host: &ManagedHost) -> bool {
    match host {
        ManagedHost::Ssh(_) => true,
        ManagedHost::Profile(entry) => matches!(entry.profile, SavedHostSessionProfile::Ssh(_)),
    }
}

/// Whether a host opens a terminal: SSH, Telnet or serial, not a remote desktop.
pub fn is_terminal_host(host: &ManagedHost) -> bool {
    is_ssh_host(host)
        || matches!(host, ManagedHost::Profile(entry) if matches!(
            entry.profile,
            SavedHostSessionProfile::Telnet(_) | SavedHostSessionProfile::Serial(_)
        ))
}

pub fn bulk_field_applies(host: &ManagedHost, field: BulkHostField) -> bool {
    match field.audience() {
        BulkAudience::Ssh => is_ssh_host(host),
        BulkAudience::Terminal => is_terminal_host(host),
        BulkAudience::All => true,
    }
}

/// Where a host's session logging override is stored — the same key the host editors use.
pub fn host_logging_key(host: &ManagedHost) -> String {
    managed_host_key(host)
}

fn tri_state(value: Option<bool>) -> BulkTriState {
    match value {
        None => BulkTriState::Inherit,
        Some(true) => BulkTriState::Yes,
        Some(false) => BulkTriState::No,
    }
}

fn tri_state_patch(value: BulkTriState) -> Patch<bool> {
    match value {
        BulkTriState::Inherit => Patch::Clear,
        BulkTriState::Yes => Patch::Set(true),
        BulkTriState::No => Patch::Set(false),
    }
}

/// The connection options a host stores itself: its Host block, or its saved profile.
struct SshOptions {
    user: Option<String>,
    port: Option<u16>,
    strict_host_key_checking: Option<StrictHostKeyChecking>,
    forward_agent: Option<bool>,
    forward_x11: Option<bool>,
}

fn ssh_options(host: &ManagedHost) -> Option<SshOptions> {
    match host {
        ManagedHost::Ssh(entry) => Some(SshOptions {
            user: entry.options.user.clone(),
            port: entry.options.port,
            strict_host_key_checking: entry.options.strict_host_key_checking,
            forward_agent: entry.options.forward_agent,
            forward_x11: entry.options.forward_x11,
        }),
        ManagedHost::Profile(entry) => match &entry.profile {
            SavedHostSessionProfile::Ssh(profile) => Some(SshOptions {
                user: profile.user.clone(),
                port: profile.port,
                strict_host_key_checking: profile.strict_host_key_checking,
                forward_agent: profile.forward_agent,
                forward_x11: profile.forward_x11,
            }),
            _ => None,
        },
    }
}

/// One host's current values for the fields that apply to it. Session logging
/// is only known once its policy has loaded; until then it stays `None`.
pub fn bulk_values_for_host(
    host: &ManagedHost,
    logging: Option<&SessionLoggingPolicy>,
) -> BulkHostValues {
    let metadata = host.metadata();
    let mut values = BulkHostValues {
        group: Some(normalize_group_path(
            metadata.and_then(|m| m.group.as_deref()),
        )),
        color: Some(metadata.and_then(|m| m.color.clone())),
        ..BulkHostValues::default()
    };
    if is_terminal_host(host) {
        let highlights = metadata.and_then(|m| m.keyword_highlights.as_ref());
        values.terminal_scheme = Some(metadata.and_then(|m| m.terminal_scheme.clone()));
        values.terminal_font_color = Some(metadata.and_then(|m| m.terminal_font_color.clone()));
        values.terminal_background_color =
            Some(metadata.and_then(|m| m.terminal_background_color.clone()));
        values.command_button_group = Some(metadata.and_then(|m| m.command_button_group.clone()));
        values.paste_line_delay_ms = Some(metadata.and_then(|m| m.paste_line_delay_ms));
        values.paste_char_delay_ms = Some(metadata.and_then(|m| m.paste_char_delay_ms));
        values.highlight_profile_id = Some(highlights.and_then(|h| h.profile_id.clone()));
        values.highlight_inherit_global = Some(highlights.is_none_or(|h| h.inherit_global));
        values.login_sequence = Some(login_sequence_draft(
            metadata.and_then(|m| m.login_sequence.as_ref()),
        ));
        if let Some(logging) = logging {
            values.session_logging = Some(host_session_logging_draft(logging, !logging.overridden));
        }
    }
    if let Some(options) = ssh_options(host) {
        values.user = Some(options.user.unwrap_or_default());
        values.port = Some(options.port.map(|p| p.to_string()).unwrap_or_default());
        values.strict_host_key_checking = Some(options.strict_host_key_checking);
        values.forward_agent = Some(tri_state(options.forward_agent));
        values.forward_x11 = Some(tri_state(options.forward_x11));
        values.console_compatibility = Some(
            metadata
                .and_then(|m| m.console_compatibility)
                .unwrap_or(false),
        );
        values.disable_sftp = Some(metadata.and_then(|m| m.disable_sftp).unwrap_or(false));
    }
    values
}

/// Why the changes cannot be applied yet, if anything stops them.
pub fn bulk_changes_problem(changes: &BulkHostChanges) -> Option<String> {
    if let Some(group) = &changes.group {
        let group = normalize_group_path(Some(group.as_str()));
        if utf16_len(&group) > MAX_GROUP_PATH {
            return Some(format!(
                "That folder path is longer than {MAX_GROUP_PATH} characters."
            ));
        }
        if folder_depth(&group) > MAX_FOLDER_DEPTH {
            return Some(format!("Folders can nest {MAX_FOLDER_DEPTH} levels deep."));
        }
    }
    if let Some(user) = &changes.user
        && user.contains(['\r', '\n', '"'])
    {
        return Some("The user name cannot contain quotes or line breaks.".into());
    }
    if let Some(port) = &changes.port
        && !js_trim(port).is_empty()
    {
        let port = js_number(port);
        if !(port.is_finite() && port.fract() == 0. && (1. ..=65_535.).contains(&port)) {
            return Some("Port must be 1–65535.".into());
        }
    }
    if let Some(sequence) = &changes.login_sequence {
        return login_sequence_problem(sequence);
    }
    None
}

/// Changes in their stored form, so they compare equal to what hosts already hold.
fn normalize_changes(changes: &BulkHostChanges) -> BulkHostChanges {
    let mut next = changes.clone();
    if let Some(group) = &mut next.group {
        let normalized = normalize_group_path(Some(group.as_str()));
        *group = normalized;
    }
    if let Some(user) = &mut next.user {
        *user = js_trim(user).to_owned();
    }
    if let Some(port) = &mut next.port {
        *port = if js_trim(port).is_empty() {
            String::new()
        } else {
            muxus_core::js::js_number(js_number(port)).to_string()
        };
    }
    next
}

/// One OpenSSH Host block patch for many aliases.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenSshBulkPatch {
    pub aliases: Vec<String>,
    pub options: HostOptionsPatch,
}

/// One logging write: `None` removes the host override.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggingWrite {
    pub profile_key: String,
    pub policy: Option<SessionLoggingPolicyInput>,
}

/// The writes that apply a bulk edit.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BulkEditPlan {
    /// One patch for every OpenSSH host whose Host block changes.
    pub open_ssh: Option<OpenSshBulkPatch>,
    /// Muxus-owned SSH hosts, saved whole with the same options patched in.
    pub profiles: Vec<SavedHostProfileInput>,
    pub metadata: Vec<(ManagedHost, OpenSshMetadataPatch)>,
    pub logging: Vec<LoggingWrite>,
    /// Hosts that end up different; a host that already has every value is skipped.
    pub hosts: Vec<ManagedHost>,
    /// Per changed field, how many hosts it actually changes.
    pub field_counts: BTreeMap<BulkHostField, usize>,
}

/// Turn the changed fields into the writes that apply them. A host only gets
/// the fields that apply to it and that it does not already have, and only
/// the stored values behind those fields change.
pub fn bulk_edit_plan(
    hosts: &[ManagedHost],
    raw_changes: &BulkHostChanges,
    policies: &HashMap<String, SessionLoggingPolicy>,
) -> BulkEditPlan {
    let changes = normalize_changes(raw_changes);
    let fields = changes.fields();
    let options = connection_patch(&changes);
    let mut plan = BulkEditPlan::default();
    for field in &fields {
        plan.field_counts.insert(*field, 0);
    }
    let mut open_ssh_aliases = Vec::new();

    for host in hosts {
        let current = bulk_values_for_host(host, policies.get(&host_logging_key(host)));
        let differs: Vec<BulkHostField> = fields
            .iter()
            .copied()
            .filter(|field| {
                bulk_field_applies(host, *field) && field_differs(*field, &current, &changes)
            })
            .collect();
        if differs.is_empty() {
            continue;
        }
        plan.hosts.push(host.clone());
        for field in &differs {
            *plan.field_counts.entry(*field).or_default() += 1;
        }

        if CONNECTION_FIELDS
            .iter()
            .any(|field| differs.contains(field))
        {
            match host {
                ManagedHost::Ssh(entry) => open_ssh_aliases.push(entry.alias.clone()),
                ManagedHost::Profile(entry) => {
                    if let SavedHostSessionProfile::Ssh(profile) = &entry.profile {
                        plan.profiles.push(SavedHostProfileInput {
                            id: Some(entry.id.clone()),
                            name: entry.name.clone(),
                            profile: SavedHostSessionProfile::Ssh(apply_options_patch(
                                profile, &options,
                            )),
                        });
                    }
                }
            }
        }

        let patch = metadata_patch(host, &changes, &differs);
        if !patch.is_empty() {
            plan.metadata.push((host.clone(), patch));
        }

        if differs.contains(&BulkHostField::SessionLogging)
            && let Some(logging) = &changes.session_logging
        {
            plan.logging.push(LoggingWrite {
                profile_key: host_logging_key(host),
                policy: (!logging.inherit).then(|| session_logging_policy_input(logging)),
            });
        }
    }

    if !open_ssh_aliases.is_empty() {
        plan.open_ssh = Some(OpenSshBulkPatch {
            aliases: open_ssh_aliases,
            options,
        });
    }
    plan
}

fn field_differs(
    field: BulkHostField,
    current: &BulkHostValues,
    changes: &BulkHostChanges,
) -> bool {
    // An unloaded policy counts as different, so a host is never skipped by mistake.
    if !current.has(field) {
        return true;
    }
    !current.same_field(changes, field)
}

fn connection_patch(changes: &BulkHostChanges) -> HostOptionsPatch {
    let mut patch = HostOptionsPatch::default();
    if let Some(user) = &changes.user {
        patch.user = if user.is_empty() {
            Patch::Clear
        } else {
            Patch::Set(user.clone())
        };
    }
    if let Some(port) = &changes.port {
        patch.port = if port.is_empty() {
            Patch::Clear
        } else {
            Patch::Set(js_number(port) as u16)
        };
    }
    if let Some(strict) = &changes.strict_host_key_checking {
        patch.strict_host_key_checking = match strict {
            None => Patch::Clear,
            Some(value) => Patch::Set(*value),
        };
    }
    if let Some(value) = changes.forward_agent {
        patch.forward_agent = tri_state_patch(value);
    }
    if let Some(value) = changes.forward_x11 {
        patch.forward_x11 = tri_state_patch(value);
    }
    patch
}

/// The profile with each patched option set, or removed for `null`.
fn apply_options_patch(profile: &SshProfile, patch: &HostOptionsPatch) -> SshProfile {
    let mut next = profile.clone();
    fn apply<T: Clone>(slot: &mut Option<T>, patch: &Patch<T>) {
        match patch {
            Patch::Keep => {}
            Patch::Clear => *slot = None,
            Patch::Set(value) => *slot = Some(value.clone()),
        }
    }
    apply(&mut next.user, &patch.user);
    apply(&mut next.port, &patch.port);
    apply(&mut next.forward_agent, &patch.forward_agent);
    apply(&mut next.forward_x11, &patch.forward_x11);
    apply(
        &mut next.strict_host_key_checking,
        &patch.strict_host_key_checking,
    );
    next
}

fn set_or_clear<T: Clone>(value: &Option<Option<T>>) -> Patch<T> {
    match value {
        Some(Some(value)) => Patch::Set(value.clone()),
        _ => Patch::Clear,
    }
}

fn metadata_patch(
    host: &ManagedHost,
    changes: &BulkHostChanges,
    differs: &[BulkHostField],
) -> OpenSshMetadataPatch {
    use BulkHostField as F;
    let has = |field: F| differs.contains(&field);
    let mut patch = OpenSshMetadataPatch::default();
    if has(F::Group) {
        patch.group = match changes.group.as_deref() {
            Some(group) if !group.is_empty() => Patch::Set(group.to_owned()),
            _ => Patch::Clear,
        };
    }
    if has(F::Color) {
        patch.color = set_or_clear(&changes.color);
    }
    if has(F::TerminalScheme) {
        patch.terminal_scheme = set_or_clear(&changes.terminal_scheme);
    }
    if has(F::TerminalFontColor) {
        patch.terminal_font_color = set_or_clear(&changes.terminal_font_color);
    }
    if has(F::TerminalBackgroundColor) {
        patch.terminal_background_color = set_or_clear(&changes.terminal_background_color);
    }
    if has(F::CommandButtonGroup) {
        patch.command_button_group = set_or_clear(&changes.command_button_group);
    }
    if has(F::PasteLineDelayMs) {
        patch.paste_line_delay_ms = set_or_clear(&changes.paste_line_delay_ms);
    }
    if has(F::PasteCharDelayMs) {
        patch.paste_char_delay_ms = set_or_clear(&changes.paste_char_delay_ms);
    }
    if has(F::ConsoleCompatibility) {
        patch.console_compatibility = changes.console_compatibility;
    }
    if has(F::DisableSftp) {
        patch.disable_sftp = changes.disable_sftp;
    }
    if has(F::LoginSequence)
        && let Some(sequence) = &changes.login_sequence
    {
        patch.login_sequence = match login_sequence_from_draft(sequence) {
            Some(sequence) => Patch::Set(sequence),
            None => Patch::Clear,
        };
    }
    if has(F::HighlightProfileId) || has(F::HighlightInheritGlobal) {
        let current = host.metadata().and_then(|m| m.keyword_highlights.as_ref());
        patch.keyword_highlights = match patched_highlights(current, changes, differs) {
            Some(highlights) => Patch::Set(highlights),
            None => Patch::Clear,
        };
    }
    patch
}

/// The host's highlighting with only the profile and the global switch
/// changed — its own rules always stay. `None` when the result is the
/// default, which is how the host editors store it too.
fn patched_highlights(
    current: Option<&HostKeywordHighlightConfig>,
    changes: &BulkHostChanges,
    differs: &[BulkHostField],
) -> Option<HostKeywordHighlightConfig> {
    let mut next = current.cloned().unwrap_or(HostKeywordHighlightConfig {
        inherit_global: true,
        profile_id: None,
        rules: Vec::new(),
    });
    if differs.contains(&BulkHostField::HighlightProfileId) {
        next.profile_id = changes
            .highlight_profile_id
            .clone()
            .flatten()
            .filter(|id| !id.is_empty());
    }
    if differs.contains(&BulkHostField::HighlightInheritGlobal)
        && let Some(inherit) = changes.highlight_inherit_global
    {
        next.inherit_global = inherit;
    }
    if next.inherit_global && next.profile_id.is_none() && next.rules.is_empty() {
        None
    } else {
        Some(next)
    }
}

/// "Applies to the 4 SSH hosts in the selection." unless the section applies
/// to every selected host (`describe` in the dialog).
pub fn describe(count: usize, total: usize, noun: &str, text: &str) -> String {
    if count == total {
        return text.to_owned();
    }
    format!(
        "Applies to the {count} {noun}{} in the selection. {text}",
        if count == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{
        HostBlockOptions, KeywordHighlightRule, LoginSendStep, LoginSequence, LoginSequenceStep,
        LoginWaitStep, OpenSshProfileMetadata, ResolvedHostSettings, SavedHostProfile,
        SshHostEntry,
    };
    use muxus_core::protocol::{RdpProfile, SerialProfile, TelnetProfile};
    use muxus_session::login_sequence::editor::LoginSequenceMode;

    use super::super::session_logging::{FALLBACK_MAX_PART_BYTES, FALLBACK_MAX_PARTS};

    fn open_ssh(
        alias: &str,
        options: HostBlockOptions,
        metadata: OpenSshProfileMetadata,
    ) -> ManagedHost {
        ManagedHost::ssh(SshHostEntry {
            alias: alias.into(),
            aliases: vec![alias.into()],
            description: None,
            file: "/home/test/.ssh/config".into(),
            resolved: ResolvedHostSettings {
                hostname: format!("{alias}.example.test"),
                port: options.port.unwrap_or(22),
                ..ResolvedHostSettings::default()
            },
            options,
            metadata: Some(OpenSshProfileMetadata {
                profile_id: alias.into(),
                ..metadata
            }),
        })
    }

    fn meta() -> OpenSshProfileMetadata {
        OpenSshProfileMetadata::default()
    }

    fn saved(
        id: &str,
        profile: SavedHostSessionProfile,
        metadata: OpenSshProfileMetadata,
    ) -> ManagedHost {
        ManagedHost::profile(SavedHostProfile {
            id: id.into(),
            kind: profile.kind(),
            name: id.into(),
            profile,
            metadata: OpenSshProfileMetadata {
                profile_id: id.into(),
                ..metadata
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        })
    }

    fn saved_ssh(id: &str, profile: SshProfile) -> ManagedHost {
        saved(
            id,
            SavedHostSessionProfile::Ssh(SshProfile {
                target: format!("{id}.example.test"),
                use_config: Some(false),
                ..profile
            }),
            meta(),
        )
    }

    fn telnet(id: &str, metadata: OpenSshProfileMetadata) -> ManagedHost {
        saved(
            id,
            SavedHostSessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: format!("{id}.example.test"),
                port: 23,
            }),
            metadata,
        )
    }

    fn rdp(id: &str, metadata: OpenSshProfileMetadata) -> ManagedHost {
        saved(
            id,
            SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: None,
                host: format!("{id}.example.test"),
                port: 3389,
                username: None,
                domain: None,
                ssh_gateway: None,
                share_clipboard: None,
            }),
            metadata,
        )
    }

    fn policy(
        key: &str,
        enabled: bool,
        log_to_file: bool,
        overridden: bool,
    ) -> SessionLoggingPolicy {
        SessionLoggingPolicy {
            profile_key: key.into(),
            enabled,
            capture_input: false,
            max_part_bytes: FALLBACK_MAX_PART_BYTES,
            max_parts: FALLBACK_MAX_PARTS,
            log_to_file,
            overridden,
        }
    }

    fn custom_logging() -> HostSessionLoggingDraft {
        HostSessionLoggingDraft {
            enabled: true,
            capture_input: false,
            max_part_bytes: FALLBACK_MAX_PART_BYTES,
            max_parts: FALLBACK_MAX_PARTS,
            log_to_file: true,
            inherit: false,
            loaded: true,
        }
    }

    fn no_policies() -> HashMap<String, SessionLoggingPolicy> {
        HashMap::new()
    }

    fn patch_json(plan: &BulkEditPlan) -> Vec<serde_json::Value> {
        plan.metadata
            .iter()
            .map(|(_, patch)| serde_json::to_value(patch).unwrap())
            .collect()
    }

    #[test]
    fn reports_shared_values_mixed_values_and_settings_no_selected_host_has() {
        let hosts = vec![
            open_ssh(
                "a",
                HostBlockOptions {
                    user: Some("ops".into()),
                    port: Some(22),
                    ..Default::default()
                },
                OpenSshProfileMetadata {
                    color: Some("#ef5350".into()),
                    group: Some("Lab".into()),
                    ..meta()
                },
            ),
            open_ssh(
                "b",
                HostBlockOptions {
                    user: Some("root".into()),
                    port: Some(22),
                    ..Default::default()
                },
                OpenSshProfileMetadata {
                    color: Some("#ef5350".into()),
                    group: Some("Lab/".into()),
                    ..meta()
                },
            ),
        ];
        let summary = summarize_bulk_values(&hosts, &no_policies());
        assert_eq!(
            summary.color,
            BulkFieldSummary::Same {
                count: 2,
                value: Some("#ef5350".into())
            }
        );
        // Folder paths compare in their normalized form.
        assert_eq!(
            summary.group,
            BulkFieldSummary::Same {
                count: 2,
                value: "Lab".into()
            }
        );
        assert_eq!(
            summary.port,
            BulkFieldSummary::Same {
                count: 2,
                value: "22".into()
            }
        );
        assert_eq!(summary.user, BulkFieldSummary::Mixed { count: 2 });
    }

    #[test]
    fn counts_only_the_hosts_a_setting_applies_to() {
        let hosts = vec![
            open_ssh(
                "a",
                HostBlockOptions {
                    user: Some("ops".into()),
                    ..Default::default()
                },
                meta(),
            ),
            telnet("t", meta()),
            rdp("desk", meta()),
        ];
        let summary = summarize_bulk_values(&hosts, &no_policies());
        assert_eq!(summary.group.count(), 3);
        assert_eq!(
            summary.terminal_scheme,
            BulkFieldSummary::Same {
                count: 2,
                value: None
            }
        );
        assert_eq!(
            summary.user,
            BulkFieldSummary::Same {
                count: 1,
                value: "ops".into()
            }
        );
        assert_eq!(
            summarize_bulk_values(&[rdp("desk", meta())], &no_policies()).terminal_scheme,
            BulkFieldSummary::None
        );
    }

    #[test]
    fn treats_an_unset_color_as_a_value_but_waits_for_every_logging_policy() {
        let hosts = vec![
            open_ssh("a", Default::default(), meta()),
            saved_ssh("b", SshProfile::default()),
        ];
        let pending = summarize_bulk_values(
            &hosts,
            &HashMap::from([("ssh:a".to_owned(), policy("ssh:a", false, false, false))]),
        );
        assert_eq!(
            pending.color,
            BulkFieldSummary::Same {
                count: 2,
                value: None
            }
        );
        assert_eq!(
            pending.session_logging,
            BulkFieldSummary::Loading { count: 2 }
        );
        let loaded = summarize_bulk_values(
            &hosts,
            &HashMap::from([
                ("ssh:a".to_owned(), policy("ssh:a", false, false, false)),
                (
                    "profile:b".to_owned(),
                    policy("profile:b", true, false, true),
                ),
            ]),
        );
        assert_eq!(loaded.session_logging, BulkFieldSummary::Mixed { count: 2 });
    }

    #[test]
    fn reads_the_highlighting_profile_and_global_switch_from_host_metadata() {
        let highlights = |inherit| HostKeywordHighlightConfig {
            inherit_global: inherit,
            profile_id: Some("net".into()),
            rules: Vec::new(),
        };
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    keyword_highlights: Some(highlights(true)),
                    ..meta()
                },
            ),
            telnet(
                "t",
                OpenSshProfileMetadata {
                    keyword_highlights: Some(highlights(false)),
                    ..meta()
                },
            ),
        ];
        let summary = summarize_bulk_values(&hosts, &no_policies());
        assert_eq!(
            summary.highlight_profile_id,
            BulkFieldSummary::Same {
                count: 2,
                value: Some("net".into())
            }
        );
        assert_eq!(
            summary.highlight_inherit_global,
            BulkFieldSummary::Mixed { count: 2 }
        );
    }

    #[test]
    fn reads_connection_flags_as_unset_on_or_off() {
        let values = bulk_values_for_host(
            &open_ssh(
                "a",
                HostBlockOptions {
                    forward_agent: Some(false),
                    forward_x11: Some(true),
                    strict_host_key_checking: Some(StrictHostKeyChecking::No),
                    ..Default::default()
                },
                meta(),
            ),
            None,
        );
        assert_eq!(values.forward_agent, Some(BulkTriState::No));
        assert_eq!(values.forward_x11, Some(BulkTriState::Yes));
        assert_eq!(
            values.strict_host_key_checking,
            Some(Some(StrictHostKeyChecking::No))
        );
        let values = bulk_values_for_host(&saved_ssh("b", SshProfile::default()), None);
        assert_eq!(values.forward_agent, Some(BulkTriState::Inherit));
        assert_eq!(values.forward_x11, Some(BulkTriState::Inherit));
        assert_eq!(values.strict_host_key_checking, Some(None));
        assert_eq!(values.user.as_deref(), Some(""));
        assert_eq!(values.port.as_deref(), Some(""));
    }

    #[test]
    fn sends_one_host_block_patch_for_openssh_hosts_and_saves_muxus_hosts_whole() {
        let hosts = vec![
            open_ssh(
                "a",
                HostBlockOptions {
                    user: Some("old".into()),
                    ..Default::default()
                },
                meta(),
            ),
            open_ssh("b", Default::default(), meta()),
            saved_ssh(
                "s",
                SshProfile {
                    user: Some("old".into()),
                    port: Some(2222),
                    identity_files: Some(vec!["~/.ssh/id_ed25519".into()]),
                    ..SshProfile::default()
                },
            ),
        ];
        let changes = BulkHostChanges {
            user: Some(" ops ".into()),
            port: Some(String::new()),
            ..Default::default()
        };
        let plan = bulk_edit_plan(&hosts, &changes, &no_policies());
        let open_ssh = plan.open_ssh.clone().unwrap();
        assert_eq!(open_ssh.aliases, vec!["a", "b"]);
        assert_eq!(
            serde_json::to_value(&open_ssh.options).unwrap(),
            serde_json::json!({ "user": "ops", "port": null })
        );
        assert_eq!(
            serde_json::to_value(&plan.profiles).unwrap(),
            serde_json::json!([{
                "id": "s",
                "name": "s",
                "profile": {
                    "kind": "ssh",
                    "target": "s.example.test",
                    "useConfig": false,
                    "user": "ops",
                    "identityFiles": ["~/.ssh/id_ed25519"],
                },
            }])
        );
        assert!(plan.metadata.is_empty());
        assert_eq!(plan.hosts.len(), 3);
    }

    #[test]
    fn skips_hosts_that_already_have_every_changed_value() {
        let blue = || OpenSshProfileMetadata {
            color: Some("#4285f4".into()),
            ..meta()
        };
        let hosts = vec![
            open_ssh(
                "a",
                HostBlockOptions {
                    user: Some("ops".into()),
                    ..Default::default()
                },
                blue(),
            ),
            open_ssh(
                "b",
                HostBlockOptions {
                    user: Some("root".into()),
                    ..Default::default()
                },
                blue(),
            ),
        ];
        let changes = BulkHostChanges {
            user: Some("ops".into()),
            color: Some(Some("#4285f4".into())),
            ..Default::default()
        };
        let plan = bulk_edit_plan(&hosts, &changes, &no_policies());
        assert_eq!(
            plan.hosts.iter().map(managed_host_key).collect::<Vec<_>>(),
            vec!["ssh:b"]
        );
        assert_eq!(plan.open_ssh.unwrap().aliases, vec!["b"]);
        assert!(plan.metadata.is_empty());
        assert_eq!(
            plan.field_counts,
            BTreeMap::from([(BulkHostField::User, 1), (BulkHostField::Color, 0)])
        );
    }

    #[test]
    fn clears_a_value_with_a_present_but_unset_change() {
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    color: Some("#4285f4".into()),
                    terminal_scheme: Some("nord".into()),
                    ..meta()
                },
            ),
            rdp(
                "desk",
                OpenSshProfileMetadata {
                    color: Some("#ef5350".into()),
                    ..meta()
                },
            ),
        ];
        let changes = BulkHostChanges {
            color: Some(None),
            terminal_scheme: Some(None),
            ..Default::default()
        };
        let plan = bulk_edit_plan(&hosts, &changes, &no_policies());
        assert_eq!(
            patch_json(&plan),
            vec![
                serde_json::json!({ "color": null, "terminalScheme": null }),
                // A remote desktop has no terminal scheme to clear.
                serde_json::json!({ "color": null }),
            ]
        );
    }

    #[test]
    fn only_changes_the_highlighting_profile_and_switch_keeping_each_host_its_own_rules() {
        let rule = KeywordHighlightRule {
            id: "r1".into(),
            name: None,
            keyword: "ERROR".into(),
            foreground: "#ffffff".into(),
            background: None,
            case_sensitive: false,
            whole_word: true,
            regex: None,
        };
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    keyword_highlights: Some(HostKeywordHighlightConfig {
                        inherit_global: true,
                        profile_id: None,
                        rules: vec![rule.clone()],
                    }),
                    ..meta()
                },
            ),
            telnet("t", meta()),
            open_ssh(
                "c",
                Default::default(),
                OpenSshProfileMetadata {
                    keyword_highlights: Some(HostKeywordHighlightConfig {
                        inherit_global: true,
                        profile_id: Some("old".into()),
                        rules: Vec::new(),
                    }),
                    ..meta()
                },
            ),
        ];
        let set = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                highlight_profile_id: Some(Some("net".into())),
                ..Default::default()
            },
            &no_policies(),
        );
        let highlights: Vec<_> = set
            .metadata
            .iter()
            .map(|(_, patch)| patch.keyword_highlights.clone())
            .collect();
        let with = |rules: Vec<KeywordHighlightRule>| {
            Patch::Set(HostKeywordHighlightConfig {
                inherit_global: true,
                profile_id: Some("net".into()),
                rules,
            })
        };
        assert_eq!(
            highlights,
            vec![with(vec![rule]), with(Vec::new()), with(Vec::new())]
        );
        // Back to the default stores nothing at all, as the host editor does.
        let cleared = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                highlight_profile_id: Some(None),
                ..Default::default()
            },
            &no_policies(),
        );
        let highlights: Vec<_> = cleared
            .metadata
            .iter()
            .map(|(_, patch)| patch.keyword_highlights.clone())
            .collect();
        assert_eq!(highlights, vec![Patch::Clear]);
    }

    #[test]
    fn sets_and_clears_the_command_button_group_on_terminal_hosts_only() {
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    command_button_group: Some("juniper".into()),
                    ..meta()
                },
            ),
            telnet("t", meta()),
            rdp("desk", meta()),
        ];
        assert_eq!(
            summarize_bulk_values(&hosts, &no_policies()).command_button_group,
            BulkFieldSummary::Mixed { count: 2 }
        );
        let set = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                command_button_group: Some(Some("juniper".into())),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(set.metadata.len(), 1);
        assert_eq!(set.metadata[0].0, hosts[1]);
        assert_eq!(
            serde_json::to_value(&set.metadata[0].1).unwrap(),
            serde_json::json!({ "commandButtonGroup": "juniper" })
        );
        let cleared = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                command_button_group: Some(None),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(cleared.metadata.len(), 1);
        assert_eq!(cleared.metadata[0].0, hosts[0]);
        assert_eq!(
            serde_json::to_value(&cleared.metadata[0].1).unwrap(),
            serde_json::json!({ "commandButtonGroup": null })
        );
    }

    #[test]
    fn sets_paste_delays_on_terminal_hosts_and_clears_them_back_to_the_settings() {
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    paste_line_delay_ms: Some(200),
                    ..meta()
                },
            ),
            telnet(
                "t",
                OpenSshProfileMetadata {
                    paste_line_delay_ms: Some(0),
                    ..meta()
                },
            ),
            saved(
                "console",
                SavedHostSessionProfile::Serial(SerialProfile {
                    baud_rate: 9600,
                    ..SerialProfile::new("/dev/ttyUSB0")
                }),
                meta(),
            ),
            rdp("desk", meta()),
        ];
        // An explicit 0 differs from following the settings.
        let summary = summarize_bulk_values(&hosts, &no_policies());
        assert_eq!(
            summary.paste_line_delay_ms,
            BulkFieldSummary::Mixed { count: 3 }
        );
        assert_eq!(
            summary.paste_char_delay_ms,
            BulkFieldSummary::Same {
                count: 3,
                value: None
            }
        );
        let set = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                paste_line_delay_ms: Some(Some(200)),
                paste_char_delay_ms: Some(Some(2)),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(
            patch_json(&set),
            vec![
                serde_json::json!({ "pasteCharDelayMs": 2 }),
                serde_json::json!({ "pasteLineDelayMs": 200, "pasteCharDelayMs": 2 }),
                serde_json::json!({ "pasteLineDelayMs": 200, "pasteCharDelayMs": 2 }),
            ]
        );
        assert_eq!(
            set.metadata
                .iter()
                .map(|(h, _)| h.clone())
                .collect::<Vec<_>>(),
            hosts[..3].to_vec()
        );
        let cleared = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                paste_line_delay_ms: Some(None),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(
            patch_json(&cleared),
            vec![
                serde_json::json!({ "pasteLineDelayMs": null }),
                serde_json::json!({ "pasteLineDelayMs": null })
            ]
        );
        assert_eq!(
            cleared
                .metadata
                .iter()
                .map(|(h, _)| h.clone())
                .collect::<Vec<_>>(),
            hosts[..2].to_vec()
        );
    }

    #[test]
    fn applies_terminal_and_ssh_settings_only_where_they_mean_something() {
        let hosts = vec![
            open_ssh("a", Default::default(), meta()),
            telnet("t", meta()),
            rdp("desk", meta()),
        ];
        let plan = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                console_compatibility: Some(true),
                terminal_font_color: Some(Some("#00ff00".into())),
                group: Some("Lab / Edge".into()),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(
            patch_json(&plan),
            vec![
                serde_json::json!({ "group": "Lab/Edge", "terminalFontColor": "#00ff00", "consoleCompatibility": true }),
                serde_json::json!({ "group": "Lab/Edge", "terminalFontColor": "#00ff00" }),
                serde_json::json!({ "group": "Lab/Edge" }),
            ]
        );
        assert_eq!(plan.open_ssh, None);
    }

    #[test]
    fn removes_connection_options_when_set_back_to_their_defaults() {
        let hosts = vec![saved_ssh(
            "s",
            SshProfile {
                forward_agent: Some(true),
                forward_x11: Some(false),
                strict_host_key_checking: Some(StrictHostKeyChecking::No),
                ..SshProfile::default()
            },
        )];
        let plan = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                forward_agent: Some(BulkTriState::Inherit),
                forward_x11: Some(BulkTriState::Yes),
                strict_host_key_checking: Some(None),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(
            serde_json::to_value(&plan.profiles[0].profile).unwrap(),
            serde_json::json!({ "kind": "ssh", "target": "s.example.test", "useConfig": false, "forwardX11": true })
        );
    }

    #[test]
    fn writes_or_removes_logging_overrides_for_terminal_hosts() {
        let hosts = vec![
            open_ssh("a", Default::default(), meta()),
            telnet("t", meta()),
            rdp("desk", meta()),
        ];
        let policies = HashMap::from([
            ("ssh:a".to_owned(), policy("ssh:a", false, false, false)),
            (
                "profile:t".to_owned(),
                policy("profile:t", true, true, true),
            ),
        ]);
        let custom = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                session_logging: Some(custom_logging()),
                ..Default::default()
            },
            &policies,
        );
        assert_eq!(
            custom.logging,
            vec![LoggingWrite {
                profile_key: "ssh:a".into(),
                policy: Some(SessionLoggingPolicyInput {
                    enabled: true,
                    capture_input: false,
                    max_part_bytes: 5 * 1024 * 1024,
                    max_parts: 10,
                    log_to_file: Some(true),
                }),
            }]
        );
        let inherit = bulk_edit_plan(
            &hosts,
            &BulkHostChanges {
                session_logging: Some(HostSessionLoggingDraft {
                    inherit: true,
                    ..custom_logging()
                }),
                ..Default::default()
            },
            &policies,
        );
        assert_eq!(
            inherit.logging,
            vec![LoggingWrite {
                profile_key: "profile:t".into(),
                policy: None
            }]
        );
    }

    #[test]
    fn writes_logging_for_a_host_whose_policy_is_not_known_yet() {
        let plan = bulk_edit_plan(
            &[open_ssh("a", Default::default(), meta())],
            &BulkHostChanges {
                session_logging: Some(custom_logging()),
                ..Default::default()
            },
            &no_policies(),
        );
        assert_eq!(
            plan.logging
                .iter()
                .map(|w| w.profile_key.as_str())
                .collect::<Vec<_>>(),
            vec!["ssh:a"]
        );
    }

    #[test]
    fn checks_changes_before_applying_them() {
        let cases: Vec<(BulkHostChanges, Option<&str>)> = vec![
            (
                BulkHostChanges {
                    port: Some("22".into()),
                    ..Default::default()
                },
                None,
            ),
            (
                BulkHostChanges {
                    port: Some(String::new()),
                    ..Default::default()
                },
                None,
            ),
            (
                BulkHostChanges {
                    port: Some("0".into()),
                    ..Default::default()
                },
                Some("Port must be 1–65535."),
            ),
            (
                BulkHostChanges {
                    port: Some("70000".into()),
                    ..Default::default()
                },
                Some("Port must be 1–65535."),
            ),
            (
                BulkHostChanges {
                    user: Some("a\"b".into()),
                    ..Default::default()
                },
                Some("The user name cannot contain quotes or line breaks."),
            ),
            (
                BulkHostChanges {
                    group: Some("a/b/c/d/e/f/g/h/i".into()),
                    ..Default::default()
                },
                Some("Folders can nest 8 levels deep."),
            ),
            (
                BulkHostChanges {
                    group: Some("x".repeat(301)),
                    ..Default::default()
                },
                Some("That folder path is longer than 300 characters."),
            ),
        ];
        for (changes, problem) in cases {
            assert_eq!(
                bulk_changes_problem(&changes).as_deref(),
                problem,
                "{changes:?}"
            );
        }
    }

    fn enable() -> LoginSequence {
        LoginSequence {
            steps: vec![
                LoginSequenceStep::Wait(LoginWaitStep {
                    id: "a".into(),
                    pattern: ">".into(),
                    regex: None,
                    timeout_seconds: 10,
                }),
                LoginSequenceStep::Send(LoginSendStep {
                    id: "b".into(),
                    text: "enable".into(),
                    enter: true,
                }),
            ],
        }
    }

    #[test]
    fn compares_sequences_by_their_steps_not_their_ids() {
        let renumbered = LoginSequence {
            steps: enable()
                .steps
                .into_iter()
                .map(|step| match step {
                    LoginSequenceStep::Wait(mut s) => {
                        s.id = format!("{}-copy", s.id);
                        LoginSequenceStep::Wait(s)
                    }
                    LoginSequenceStep::Send(mut s) => {
                        s.id = format!("{}-copy", s.id);
                        LoginSequenceStep::Send(s)
                    }
                    other => other,
                })
                .collect(),
        };
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    login_sequence: Some(enable()),
                    ..meta()
                },
            ),
            telnet(
                "t",
                OpenSshProfileMetadata {
                    login_sequence: Some(renumbered),
                    ..meta()
                },
            ),
        ];
        let summary = summarize_bulk_values(&hosts, &no_policies()).login_sequence;
        let BulkFieldSummary::Same { count: 2, value } = summary else {
            panic!("expected the same sequence, got {summary:?}");
        };
        assert_eq!(value.mode, LoginSequenceMode::Custom);
        let mut more = hosts.clone();
        more.push(telnet("u", meta()));
        assert_eq!(
            summarize_bulk_values(&more, &no_policies()).login_sequence,
            BulkFieldSummary::Mixed { count: 3 }
        );
        assert_eq!(
            summarize_bulk_values(&[rdp("desk", meta())], &no_policies()).login_sequence,
            BulkFieldSummary::None
        );
    }

    #[test]
    fn sets_one_sequence_on_terminal_hosts_and_clears_it_back_to_the_folder() {
        let hosts = vec![
            open_ssh(
                "a",
                Default::default(),
                OpenSshProfileMetadata {
                    login_sequence: Some(enable()),
                    ..meta()
                },
            ),
            telnet("t", meta()),
            rdp("desk", meta()),
        ];
        let draft = |mode, steps| BulkHostChanges {
            login_sequence: Some(LoginSequenceDraft { mode, steps }),
            ..Default::default()
        };
        let set = bulk_edit_plan(
            &hosts,
            &draft(LoginSequenceMode::Custom, enable().steps),
            &no_policies(),
        );
        assert_eq!(set.metadata.len(), 1);
        assert_eq!(set.metadata[0].0, hosts[1]);
        assert_eq!(set.metadata[0].1.login_sequence, Patch::Set(enable()));

        let none = bulk_edit_plan(
            &hosts,
            &draft(LoginSequenceMode::None, enable().steps),
            &no_policies(),
        );
        assert_eq!(
            none.metadata
                .iter()
                .map(|(h, p)| (h.clone(), p.login_sequence.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    hosts[0].clone(),
                    Patch::Set(LoginSequence { steps: Vec::new() })
                ),
                (
                    hosts[1].clone(),
                    Patch::Set(LoginSequence { steps: Vec::new() })
                ),
            ]
        );
        let inherit = bulk_edit_plan(
            &hosts,
            &draft(LoginSequenceMode::Inherit, Vec::new()),
            &no_policies(),
        );
        assert_eq!(
            inherit
                .metadata
                .iter()
                .map(|(h, p)| (h.clone(), p.login_sequence.clone()))
                .collect::<Vec<_>>(),
            vec![(hosts[0].clone(), Patch::Clear)]
        );
    }

    #[test]
    fn blocks_a_sequence_with_an_unfinished_step() {
        let unfinished = BulkHostChanges {
            login_sequence: Some(LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps: vec![LoginSequenceStep::Wait(LoginWaitStep {
                    id: "x".into(),
                    pattern: String::new(),
                    regex: None,
                    timeout_seconds: 10,
                })],
            }),
            ..Default::default()
        };
        assert_eq!(
            bulk_changes_problem(&unfinished).as_deref(),
            Some("Login sequence step 1: Enter the text to wait for.")
        );
        let fine = BulkHostChanges {
            login_sequence: Some(LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps: enable().steps,
            }),
            ..Default::default()
        };
        assert_eq!(bulk_changes_problem(&fine), None);
    }

    #[test]
    fn describes_sections_that_apply_to_part_of_the_selection() {
        assert_eq!(describe(3, 3, "SSH host", "Text."), "Text.");
        assert_eq!(
            describe(1, 3, "SSH host", "Text."),
            "Applies to the 1 SSH host in the selection. Text."
        );
        assert_eq!(
            describe(2, 3, "terminal host", "Text."),
            "Applies to the 2 terminal hosts in the selection. Text."
        );
    }
}
