//! The SSH host editor's form state (`client/src/components/host-editor/draft.ts`):
//! one draft for an OpenSSH Host block or a Muxus-owned SSH profile.

use muxus_core::anti_idle::{
    AntiIdleDraft, anti_idle_draft, anti_idle_from_draft, anti_idle_problem,
};
use muxus_core::api::{
    ConfigForward, ForwardType, HostBlockOptions, HostKeywordHighlightConfig, HostOptionExtra,
    HostUpsertRequest, OpenSshMetadataPatch, SavedHostProfile, SavedHostProfileInput, SshHostEntry,
    saved_host_hop_id,
};
use muxus_core::input_modes::{
    TerminalInputModes, apply_terminal_input_modes_patch, host_terminal_input_modes,
};
use muxus_core::js::{Patch, js_trim, utf16_len};
use muxus_core::paste_prompt::{
    PastePromptWaitDraft, paste_prompt_wait_draft, paste_prompt_wait_from_draft,
    paste_prompt_wait_problem,
};
use muxus_core::protocol::{
    RequestTty, SavedHostSessionProfile, SshProfile, StrictHostKeyChecking,
};
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, login_sequence_draft, login_sequence_from_draft, login_sequence_problem,
};
use muxus_term::keywords::keyword_highlight_rules_problem;

use super::js::{non_empty, port_ok, split_words, to_port};
use super::native_draft::{
    default_highlights, highlights_patch, optional_patch, parse_host_target, text_patch,
};
use super::session_logging::{HostSessionLoggingDraft, blank_host_session_logging_draft};

/// Whether Muxus owns the connection or writes an OpenSSH Host block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Storage {
    #[default]
    Openssh,
    Muxus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AuthMode {
    #[default]
    Default,
    Key,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IdentityAgentMode {
    #[default]
    Default,
    Environment,
    Custom,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RemoteCommandMode {
    #[default]
    Inherit,
    Shell,
    Command,
}

/// ForwardX11: `Inherit` follows ssh_config, else the Muxus platform default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ForwardX11Mode {
    #[default]
    Inherit,
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RouteMode {
    #[default]
    Direct,
    Jump,
    Command,
}

/// Look for containers, clusters and VMs on connect; `Default` follows Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EnvironmentDiscoveryDraft {
    #[default]
    Default,
    On,
    Off,
}

/// `environmentDiscoveryDraft`.
pub fn environment_discovery_draft(value: Option<bool>) -> EnvironmentDiscoveryDraft {
    match value {
        None => EnvironmentDiscoveryDraft::Default,
        Some(true) => EnvironmentDiscoveryDraft::On,
        Some(false) => EnvironmentDiscoveryDraft::Off,
    }
}

/// The stored override (`environmentDiscoveryFromDraft`): `None` follows the global setting.
pub fn environment_discovery_from_draft(value: EnvironmentDiscoveryDraft) -> Option<bool> {
    match value {
        EnvironmentDiscoveryDraft::Default => None,
        EnvironmentDiscoveryDraft::On => Some(true),
        EnvironmentDiscoveryDraft::Off => Some(false),
    }
}

/// Everything the editor form holds, in form-friendly shapes (ports as text).
#[derive(Debug, Clone, PartialEq)]
pub struct HostDraft {
    pub storage: Storage,
    /// Space-separated aliases for the Host line (usually one).
    pub alias_text: String,
    pub description: String,
    /// Muxus-only presentation metadata, saved alongside the block.
    pub display_name: String,
    pub group: String,
    pub color: Option<String>,
    /// Empty follows the application theme's terminal scheme preference.
    pub terminal_scheme: Option<String>,
    pub terminal_font_color: Option<String>,
    pub terminal_background_color: Option<String>,
    /// Command button group the bar switches to in this host's sessions.
    pub command_button_group: Option<String>,
    /// Paste pacing; unset follows the Terminal settings.
    pub paste_line_delay_ms: Option<u32>,
    pub paste_char_delay_ms: Option<u32>,
    /// Pastes wait for the prompt after each line.
    pub paste_prompt_wait: PastePromptWaitDraft,
    /// Text typed into idle sessions.
    pub anti_idle: AntiIdleDraft,
    /// Backspace and Enter keys, implicit CR and local echo.
    pub input_modes: TerminalInputModes,
    /// Muxus-only plain-shell mode: no SFTP or shell integration.
    pub disable_sftp: bool,
    /// Muxus-only console mode: also no env requests, with PTY rejection fallback.
    pub console_compatibility: bool,
    /// Connect with the computer's `ssh` program instead of the built-in client.
    pub system_open_ssh: bool,
    /// Look for containers, clusters and VMs on connect; `Default` follows Settings.
    pub discover_environments: EnvironmentDiscoveryDraft,
    /// Target config file; empty keeps the block's file (or the root for new hosts).
    pub file: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub auth_mode: AuthMode,
    pub identity_files: Vec<String>,
    pub certificate_files: Vec<String>,
    /// As the host has it (unset when absent); [`auth_choice`] sets it.
    pub identities_only: Option<bool>,
    pub identity_agent_mode: IdentityAgentMode,
    /// Custom agent socket path or environment indirection.
    pub identity_agent: String,
    pub forward_agent: bool,
    pub forward_x11: ForwardX11Mode,
    pub route_mode: RouteMode,
    pub proxy_jump: Vec<String>,
    /// Jump routes tried in order when `proxy_jump` fails, each a whole chain.
    pub proxy_jump_fallbacks: Vec<Vec<String>>,
    pub proxy_command: String,
    pub forwards: Vec<ConfigForward>,
    pub remote_command_mode: RemoteCommandMode,
    pub remote_command: String,
    /// `None` inherits RequestTTY.
    pub request_tty: Option<RequestTty>,
    /// `None` inherits StrictHostKeyChecking.
    pub strict_host_key_checking: Option<StrictHostKeyChecking>,
    pub extras: Vec<HostOptionExtra>,
    pub keyword_highlights: HostKeywordHighlightConfig,
    pub session_logging: HostSessionLoggingDraft,
    /// Muxus-only login steps, saved with the metadata.
    pub login_sequence: LoginSequenceDraft,
}

/// `/[@:]/.test(target)`.
fn looks_like_target(target: &str) -> bool {
    target.contains(['@', ':'])
}

/// `blankDraft(prefillTarget, group, storage)`.
pub fn blank_draft(prefill_target: &str, group: &str, storage: Storage) -> HostDraft {
    // A quick-connect target already carries the fields the form asks for; a bare
    // name the sidebar could not find is just the alias. A Muxus profile needs
    // its hostname spelled out, so the bare name fills that too.
    let target = js_trim(prefill_target);
    let parsed = looks_like_target(target).then(|| parse_host_target(target));
    HostDraft {
        storage,
        alias_text: parsed
            .as_ref()
            .map(|p| p.host.clone())
            .unwrap_or_else(|| target.to_owned()),
        description: String::new(),
        display_name: String::new(),
        group: group.to_owned(),
        color: None,
        terminal_scheme: None,
        terminal_font_color: None,
        terminal_background_color: None,
        command_button_group: None,
        paste_line_delay_ms: None,
        paste_char_delay_ms: None,
        paste_prompt_wait: paste_prompt_wait_draft(None),
        anti_idle: anti_idle_draft(None),
        input_modes: host_terminal_input_modes(None),
        disable_sftp: false,
        console_compatibility: false,
        system_open_ssh: false,
        discover_environments: EnvironmentDiscoveryDraft::Default,
        file: String::new(),
        hostname: match &parsed {
            Some(parsed) => parsed.host.clone(),
            None if storage == Storage::Muxus => target.to_owned(),
            None => String::new(),
        },
        user: parsed
            .as_ref()
            .and_then(|p| p.user.clone())
            .unwrap_or_default(),
        port: parsed
            .as_ref()
            .and_then(|p| p.port.clone())
            .unwrap_or_default(),
        auth_mode: AuthMode::Default,
        identity_files: Vec::new(),
        certificate_files: Vec::new(),
        // "Specific key file" is an exact-key mode. This stays dormant while the
        // default auth mode is selected, then serializes as IdentitiesOnly yes.
        identities_only: Some(true),
        identity_agent_mode: IdentityAgentMode::Default,
        identity_agent: String::new(),
        forward_agent: false,
        forward_x11: ForwardX11Mode::Inherit,
        route_mode: RouteMode::Direct,
        proxy_jump: Vec::new(),
        proxy_jump_fallbacks: Vec::new(),
        proxy_command: String::new(),
        forwards: Vec::new(),
        remote_command_mode: RemoteCommandMode::Inherit,
        remote_command: String::new(),
        request_tty: None,
        strict_host_key_checking: None,
        extras: Vec::new(),
        keyword_highlights: default_highlights(),
        session_logging: blank_host_session_logging_draft(),
        login_sequence: login_sequence_draft(None),
    }
}

impl Default for HostDraft {
    fn default() -> Self {
        blank_draft("", "", Storage::Openssh)
    }
}

/// The authentication mode a stored host implies.
fn auth_mode_of(
    password_only: Option<bool>,
    identity_files: &Option<Vec<String>>,
    certificates: &Option<Vec<String>>,
) -> AuthMode {
    if password_only == Some(true) {
        AuthMode::Password
    } else if identity_files.as_ref().is_some_and(|f| !f.is_empty())
        || certificates.as_ref().is_some_and(|f| !f.is_empty())
    {
        AuthMode::Key
    } else {
        AuthMode::Default
    }
}

fn route_mode_of(proxy_command: &Option<String>, proxy_jump: &Option<Vec<String>>) -> RouteMode {
    if proxy_command.as_ref().is_some_and(|c| !c.is_empty()) {
        RouteMode::Command
    } else if proxy_jump.as_ref().is_some_and(|hops| !hops.is_empty()) {
        RouteMode::Jump
    } else {
        RouteMode::Direct
    }
}

/// Build the form state from a listed entry's own block options.
pub fn draft_from_entry(entry: &SshHostEntry, duplicate: bool) -> HostDraft {
    let o = &entry.options;
    let metadata = entry.metadata.as_ref();
    let (identity_agent_mode, identity_agent) = identity_agent_draft(o.identity_agent.as_deref());
    let (remote_command_mode, remote_command) = remote_command_draft(o.remote_command.as_deref());
    HostDraft {
        storage: Storage::Openssh,
        alias_text: if duplicate {
            format!("{}-copy", entry.alias)
        } else {
            entry.aliases.join(" ")
        },
        description: entry.description.clone().unwrap_or_default(),
        display_name: if duplicate {
            String::new()
        } else {
            metadata
                .and_then(|m| m.display_name.clone())
                .unwrap_or_default()
        },
        group: metadata.and_then(|m| m.group.clone()).unwrap_or_default(),
        color: metadata.and_then(|m| m.color.clone()),
        terminal_scheme: metadata.and_then(|m| m.terminal_scheme.clone()),
        terminal_font_color: metadata.and_then(|m| m.terminal_font_color.clone()),
        terminal_background_color: metadata.and_then(|m| m.terminal_background_color.clone()),
        command_button_group: metadata.and_then(|m| m.command_button_group.clone()),
        paste_line_delay_ms: metadata.and_then(|m| m.paste_line_delay_ms),
        paste_char_delay_ms: metadata.and_then(|m| m.paste_char_delay_ms),
        paste_prompt_wait: paste_prompt_wait_draft(
            metadata.and_then(|m| m.paste_prompt_wait.as_ref()),
        ),
        anti_idle: anti_idle_draft(metadata.and_then(|m| m.anti_idle.as_ref())),
        input_modes: host_terminal_input_modes(metadata),
        disable_sftp: metadata.and_then(|m| m.disable_sftp).unwrap_or(false),
        console_compatibility: metadata
            .and_then(|m| m.console_compatibility)
            .unwrap_or(false),
        system_open_ssh: metadata.and_then(|m| m.system_open_ssh).unwrap_or(false),
        discover_environments: environment_discovery_draft(
            metadata.and_then(|m| m.discover_environments),
        ),
        file: entry.file.clone(),
        hostname: o.hostname.clone().unwrap_or_default(),
        user: o.user.clone().unwrap_or_default(),
        port: o.port.map(|p| p.to_string()).unwrap_or_default(),
        auth_mode: auth_mode_of(o.password_only, &o.identity_files, &o.certificate_files),
        identity_files: o.identity_files.clone().unwrap_or_default(),
        certificate_files: o.certificate_files.clone().unwrap_or_default(),
        identities_only: o.identities_only,
        identity_agent_mode,
        identity_agent,
        forward_agent: o.forward_agent.unwrap_or(false),
        forward_x11: forward_x11_draft(o.forward_x11),
        route_mode: route_mode_of(&o.proxy_command, &o.proxy_jump),
        proxy_jump: o.proxy_jump.clone().unwrap_or_default(),
        proxy_jump_fallbacks: metadata
            .and_then(|m| m.proxy_jump_fallbacks.clone())
            .unwrap_or_default(),
        proxy_command: o.proxy_command.clone().unwrap_or_default(),
        forwards: o.forwards.clone().unwrap_or_default(),
        remote_command_mode,
        remote_command,
        request_tty: o.request_tty,
        strict_host_key_checking: o.strict_host_key_checking,
        extras: o.extras.clone().unwrap_or_default(),
        keyword_highlights: metadata
            .and_then(|m| m.keyword_highlights.clone())
            .unwrap_or_else(default_highlights),
        session_logging: blank_host_session_logging_draft(),
        login_sequence: login_sequence_draft(metadata.and_then(|m| m.login_sequence.as_ref())),
    }
}

/// Build the SSH form from a Muxus-owned database profile; `None` when the
/// saved host is not an SSH profile (the TypeScript throws).
pub fn draft_from_saved_ssh_profile(
    saved: &SavedHostProfile,
    duplicate: bool,
) -> Option<HostDraft> {
    let SavedHostSessionProfile::Ssh(profile) = &saved.profile else {
        return None;
    };
    let metadata = &saved.metadata;
    let (identity_agent_mode, identity_agent) =
        identity_agent_draft(profile.identity_agent.as_deref());
    let (remote_command_mode, remote_command) =
        remote_command_draft(profile.remote_command.as_deref());
    Some(HostDraft {
        storage: Storage::Muxus,
        alias_text: if duplicate {
            format!("{} copy", saved.name)
        } else {
            saved.name.clone()
        },
        description: String::new(),
        display_name: if duplicate {
            String::new()
        } else {
            metadata.display_name.clone().unwrap_or_default()
        },
        group: metadata.group.clone().unwrap_or_default(),
        color: metadata.color.clone(),
        terminal_scheme: metadata.terminal_scheme.clone(),
        terminal_font_color: metadata.terminal_font_color.clone(),
        terminal_background_color: metadata.terminal_background_color.clone(),
        command_button_group: metadata.command_button_group.clone(),
        paste_line_delay_ms: metadata.paste_line_delay_ms,
        paste_char_delay_ms: metadata.paste_char_delay_ms,
        paste_prompt_wait: paste_prompt_wait_draft(metadata.paste_prompt_wait.as_ref()),
        anti_idle: anti_idle_draft(metadata.anti_idle.as_ref()),
        input_modes: host_terminal_input_modes(Some(metadata)),
        disable_sftp: metadata.disable_sftp.unwrap_or(false),
        console_compatibility: metadata.console_compatibility.unwrap_or(false),
        system_open_ssh: metadata.system_open_ssh.unwrap_or(false),
        discover_environments: environment_discovery_draft(metadata.discover_environments),
        file: String::new(),
        hostname: profile.target.clone(),
        user: profile.user.clone().unwrap_or_default(),
        port: profile.port.map(|p| p.to_string()).unwrap_or_default(),
        auth_mode: auth_mode_of(
            profile.password_only,
            &profile.identity_files,
            &profile.certificate_files,
        ),
        identity_files: profile.identity_files.clone().unwrap_or_default(),
        certificate_files: profile.certificate_files.clone().unwrap_or_default(),
        identities_only: profile.identities_only,
        identity_agent_mode,
        identity_agent,
        forward_agent: profile.forward_agent.unwrap_or(false),
        forward_x11: forward_x11_draft(profile.forward_x11),
        route_mode: route_mode_of(&profile.proxy_command, &profile.proxy_jump),
        proxy_jump: profile.proxy_jump.clone().unwrap_or_default(),
        proxy_jump_fallbacks: profile.proxy_jump_fallbacks.clone().unwrap_or_default(),
        proxy_command: profile.proxy_command.clone().unwrap_or_default(),
        forwards: profile.forwards.clone().unwrap_or_default(),
        remote_command_mode,
        remote_command,
        request_tty: profile.request_tty,
        strict_host_key_checking: profile.strict_host_key_checking,
        extras: Vec::new(),
        keyword_highlights: metadata
            .keyword_highlights
            .clone()
            .unwrap_or_else(default_highlights),
        session_logging: blank_host_session_logging_draft(),
        login_sequence: login_sequence_draft(metadata.login_sequence.as_ref()),
    })
}

/// The fallback routes to save (`draftJumpFallbacks`): only a jump route has any.
pub fn draft_jump_fallbacks(draft: &HostDraft) -> Option<Vec<Vec<String>>> {
    (draft.route_mode == RouteMode::Jump && !draft.proxy_jump_fallbacks.is_empty())
        .then(|| draft.proxy_jump_fallbacks.clone())
}

/// The Host line's aliases.
pub fn draft_aliases(draft: &HostDraft) -> Vec<String> {
    split_words(&draft.alias_text)
}

/// `/^[^\s#*?!]+$/`.
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias
            .chars()
            .any(|c| muxus_core::js::is_js_whitespace(c) || matches!(c, '#' | '*' | '?' | '!'))
}

/// `/^[A-Za-z][A-Za-z0-9]*$/`.
fn valid_keyword(keyword: &str) -> bool {
    let mut chars = keyword.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// The first reason the draft cannot be saved, or `None`.
pub fn draft_problem(draft: &HostDraft) -> Option<String> {
    if draft.storage == Storage::Muxus {
        let name = js_trim(&draft.alias_text);
        if name.is_empty() {
            return Some("A name is required — it labels this host in Muxus.".into());
        }
        if utf16_len(name) > 200 {
            return Some("The host name must be 200 characters or fewer.".into());
        }
        if js_trim(&draft.hostname).is_empty() {
            return Some("Enter a hostname or IP address.".into());
        }
        if !draft.extras.is_empty() {
            return Some("Additional ssh_config options require OpenSSH config storage.".into());
        }
    }
    let aliases = draft_aliases(draft);
    if draft.storage == Storage::Openssh {
        if aliases.is_empty() {
            return Some("An alias is required — it is what you connect as.".into());
        }
        if let Some(alias) = aliases.iter().find(|a| !valid_alias(a)) {
            return Some(format!(
                "\"{alias}\" is not a valid alias (no spaces, wildcards or \"!\")."
            ));
        }
    }
    if !draft.port.is_empty() && !port_ok(&draft.port) {
        return Some("Port must be 1–65535.".into());
    }
    if draft.auth_mode == AuthMode::Key
        && !draft.identity_files.iter().any(|f| !js_trim(f).is_empty())
    {
        return Some("Pick at least one key file, or switch the auth mode.".into());
    }
    if draft.identity_agent_mode == IdentityAgentMode::Custom
        && js_trim(&draft.identity_agent).is_empty()
    {
        return Some("Enter an agent socket path or choose another agent source.".into());
    }
    if draft.route_mode == RouteMode::Command && js_trim(&draft.proxy_command).is_empty() {
        return Some("Proxy command is required when ProxyCommand routing is selected.".into());
    }
    if draft.storage == Storage::Openssh
        && draft.route_mode == RouteMode::Jump
        && draft
            .proxy_jump
            .iter()
            .any(|hop| saved_host_hop_id(hop).is_some())
    {
        return Some(
            "Jump hosts saved in Muxus need Muxus app data storage; ssh_config cannot reach them."
                .into(),
        );
    }
    if draft.route_mode == RouteMode::Jump && !draft.proxy_jump_fallbacks.is_empty() {
        if draft.proxy_jump.is_empty() {
            return Some("Add a jump host before adding fallback routes.".into());
        }
        if draft.storage == Storage::Openssh
            && draft
                .proxy_jump_fallbacks
                .iter()
                .any(|route| route.iter().any(|hop| saved_host_hop_id(hop).is_some()))
        {
            return Some(
                "Fallback routes through hosts saved in Muxus need Muxus app data storage.".into(),
            );
        }
    }
    if draft.remote_command_mode == RemoteCommandMode::Command
        && js_trim(&draft.remote_command).is_empty()
    {
        return Some("Enter a startup command or choose a login shell.".into());
    }
    for forward in &draft.forwards {
        if forward.bind_port == 0 {
            return Some("Every forward needs a listen port (1–65535).".into());
        }
        if forward.kind != ForwardType::Dynamic
            && (forward
                .target_host
                .as_deref()
                .is_none_or(|host| js_trim(host).is_empty())
                || forward.target_port.is_none_or(|port| port == 0))
        {
            return Some("Local/remote forwards need a target host and port.".into());
        }
    }
    if let Some(extra) = draft.extras.iter().find(|e| !valid_keyword(&e.keyword)) {
        return Some(format!(
            "\"{}\" is not a valid option keyword.",
            extra.keyword
        ));
    }
    paste_prompt_wait_problem(&draft.paste_prompt_wait)
        .or_else(|| anti_idle_problem(&draft.anti_idle))
        .or_else(|| login_sequence_problem(&draft.login_sequence))
        .or_else(|| keyword_highlight_rules_problem(&draft.keyword_highlights.rules))
}

fn trimmed_files(files: &[String]) -> Vec<String> {
    files
        .iter()
        .map(|f| js_trim(f).to_owned())
        .filter(|f| !f.is_empty())
        .collect()
}

fn identity_agent_option(draft: &HostDraft) -> Option<String> {
    match draft.identity_agent_mode {
        IdentityAgentMode::Environment => Some("SSH_AUTH_SOCK".into()),
        IdentityAgentMode::None => Some("none".into()),
        IdentityAgentMode::Custom => non_empty(&draft.identity_agent),
        IdentityAgentMode::Default => None,
    }
}

/// Serialize a database-backed SSH host without involving ssh_config.
pub fn draft_to_saved_ssh_input(
    draft: &HostDraft,
    existing_id: Option<&str>,
) -> SavedHostProfileInput {
    let key = draft.auth_mode == AuthMode::Key;
    SavedHostProfileInput {
        id: existing_id.map(str::to_owned),
        name: js_trim(&draft.alias_text).to_owned(),
        profile: SavedHostSessionProfile::Ssh(SshProfile {
            target: js_trim(&draft.hostname).to_owned(),
            use_config: Some(false),
            user: non_empty(&draft.user),
            port: if draft.port.is_empty() {
                None
            } else {
                to_port(&draft.port)
            },
            identity_files: key.then(|| trimmed_files(&draft.identity_files)),
            certificate_files: key.then(|| trimmed_files(&draft.certificate_files)),
            identities_only: if key { draft.identities_only } else { None },
            identity_agent: identity_agent_option(draft),
            forward_agent: draft.forward_agent.then_some(true),
            forward_x11: forward_x11_option(draft.forward_x11),
            proxy_jump: (draft.route_mode == RouteMode::Jump && !draft.proxy_jump.is_empty())
                .then(|| draft.proxy_jump.clone()),
            proxy_jump_fallbacks: draft_jump_fallbacks(draft),
            proxy_command: if draft.route_mode == RouteMode::Command {
                non_empty(&draft.proxy_command)
            } else {
                None
            },
            forwards: (!draft.forwards.is_empty()).then(|| draft.forwards.clone()),
            password_only: (draft.auth_mode == AuthMode::Password).then_some(true),
            remote_command: if draft.remote_command_mode == RemoteCommandMode::Command {
                non_empty(&draft.remote_command)
            } else {
                None
            },
            request_tty: draft.request_tty,
            strict_host_key_checking: draft.strict_host_key_checking,
            ..SshProfile::default()
        }),
    }
}

/// The Host block upsert a save of an OpenSSH host sends.
pub fn draft_to_request(draft: &HostDraft, previous_alias: Option<&str>) -> HostUpsertRequest {
    let key = draft.auth_mode == AuthMode::Key;
    HostUpsertRequest {
        aliases: draft_aliases(draft),
        description: non_empty(&draft.description),
        file: (!draft.file.is_empty()).then(|| draft.file.clone()),
        previous_alias: previous_alias.map(str::to_owned),
        options: HostBlockOptions {
            hostname: non_empty(&draft.hostname),
            user: non_empty(&draft.user),
            port: if draft.port.is_empty() {
                None
            } else {
                to_port(&draft.port)
            },
            identity_files: key.then(|| trimmed_files(&draft.identity_files)),
            certificate_files: key.then(|| trimmed_files(&draft.certificate_files)),
            // Set by auth_choice when the user picks keys; otherwise as the block has it.
            identities_only: if key { draft.identities_only } else { None },
            identity_agent: identity_agent_option(draft),
            forward_agent: draft.forward_agent.then_some(true),
            forward_x11: forward_x11_option(draft.forward_x11),
            proxy_jump: (draft.route_mode == RouteMode::Jump && !draft.proxy_jump.is_empty())
                .then(|| draft.proxy_jump.clone()),
            proxy_command: if draft.route_mode == RouteMode::Command {
                non_empty(&draft.proxy_command)
            } else {
                None
            },
            forwards: (!draft.forwards.is_empty()).then(|| draft.forwards.clone()),
            password_only: (draft.auth_mode == AuthMode::Password).then_some(true),
            remote_command: match draft.remote_command_mode {
                RemoteCommandMode::Shell => Some("none".into()),
                RemoteCommandMode::Command => non_empty(&draft.remote_command),
                RemoteCommandMode::Inherit => None,
            },
            request_tty: draft.request_tty,
            strict_host_key_checking: draft.strict_host_key_checking,
            extras: (!draft.extras.is_empty()).then(|| draft.extras.clone()),
        },
    }
}

/// The Muxus metadata a save writes next to the host (both storages;
/// `metadataPatch` in HostEditorDialog.tsx).
pub fn draft_metadata_patch(draft: &HostDraft) -> OpenSshMetadataPatch {
    let mut patch = OpenSshMetadataPatch {
        display_name: text_patch(&draft.display_name),
        group: text_patch(&draft.group),
        color: optional_patch(&draft.color),
        terminal_scheme: optional_patch(&draft.terminal_scheme),
        terminal_font_color: optional_patch(&draft.terminal_font_color),
        terminal_background_color: optional_patch(&draft.terminal_background_color),
        command_button_group: optional_patch(&draft.command_button_group),
        paste_line_delay_ms: optional_patch(&draft.paste_line_delay_ms),
        paste_char_delay_ms: optional_patch(&draft.paste_char_delay_ms),
        paste_prompt_wait: optional_patch(&paste_prompt_wait_from_draft(&draft.paste_prompt_wait)),
        anti_idle: optional_patch(&anti_idle_from_draft(&draft.anti_idle)),
        disable_sftp: Some(draft.disable_sftp),
        console_compatibility: Some(draft.console_compatibility),
        system_open_ssh: Some(draft.system_open_ssh),
        discover_environments: optional_patch(&environment_discovery_from_draft(
            draft.discover_environments,
        )),
        keyword_highlights: highlights_patch(&draft.keyword_highlights),
        login_sequence: optional_patch(&login_sequence_from_draft(&draft.login_sequence)),
        icon: Patch::Keep,
        // ssh_config cannot hold fallback routes, so Muxus keeps them for the
        // alias; a Muxus host has them in its profile instead.
        proxy_jump_fallbacks: match draft.storage {
            Storage::Openssh => optional_patch(&draft_jump_fallbacks(draft)),
            Storage::Muxus => Patch::Keep,
        },
        ..OpenSshMetadataPatch::default()
    };
    apply_terminal_input_modes_patch(&draft.input_modes, &mut patch);
    patch
}

/// Agent source represented by the current editor state, for live key detection.
pub fn identity_agent_for_detection(
    mode: IdentityAgentMode,
    identity_agent: &str,
    inherited_identity_agent: Option<&str>,
) -> Option<String> {
    match mode {
        IdentityAgentMode::Environment => Some("SSH_AUTH_SOCK".into()),
        // Do not accidentally scan the default agent while the custom field is empty.
        IdentityAgentMode::Custom => {
            Some(non_empty(identity_agent).unwrap_or_else(|| "none".into()))
        }
        IdentityAgentMode::None => Some("none".into()),
        IdentityAgentMode::Default => inherited_identity_agent.map(str::to_owned),
    }
}

fn identity_agent_draft(value: Option<&str>) -> (IdentityAgentMode, String) {
    match value {
        None => (IdentityAgentMode::Default, String::new()),
        Some("SSH_AUTH_SOCK") => (IdentityAgentMode::Environment, String::new()),
        Some(value) if value.to_lowercase() == "none" => (IdentityAgentMode::None, String::new()),
        Some(value) => (IdentityAgentMode::Custom, value.to_owned()),
    }
}

fn forward_x11_draft(value: Option<bool>) -> ForwardX11Mode {
    match value {
        None => ForwardX11Mode::Inherit,
        Some(true) => ForwardX11Mode::Yes,
        Some(false) => ForwardX11Mode::No,
    }
}

fn forward_x11_option(mode: ForwardX11Mode) -> Option<bool> {
    match mode {
        ForwardX11Mode::Inherit => None,
        ForwardX11Mode::Yes => Some(true),
        ForwardX11Mode::No => Some(false),
    }
}

fn remote_command_draft(value: Option<&str>) -> (RemoteCommandMode, String) {
    match value {
        None => (RemoteCommandMode::Inherit, String::new()),
        Some(value) if value.to_lowercase() == "none" => (RemoteCommandMode::Shell, String::new()),
        Some(value) => (RemoteCommandMode::Command, value.to_owned()),
    }
}

/// A change from the authentication controls (`authChoice`). Picking the specific-key mode or
/// its files promises that only those files are offered, and OpenSSH still tries the agent
/// first for IdentityFile alone, so the choice also sets IdentitiesOnly yes. A host saved
/// without such a change keeps its own value.
pub fn auth_choice(draft: &mut HostDraft) {
    draft.identities_only = Some(true);
}

/// Sub-label of the specific-key choice (`keyModeHelp`). Choosing it sets IdentitiesOnly yes,
/// but a host loaded in key mode without it only lists its files.
pub fn key_mode_help(draft: &HostDraft) -> &'static str {
    let exact = draft.auth_mode != AuthMode::Key || draft.identities_only == Some(true);
    match (draft.storage, exact) {
        (Storage::Openssh, true) => "Writes IdentityFile — exactly these keys are offered",
        (Storage::Openssh, false) => "Writes IdentityFile",
        (_, true) => "Exactly these keys are offered for this Muxus connection",
        (_, false) => "These keys are offered for this Muxus connection",
    }
}

/// Caption under the key files (`keyFilesCaption`). Without the host's own IdentitiesOnly yes,
/// Host *, folder credentials or the agent source may still decide.
pub fn key_files_caption(draft: &HostDraft) -> &'static str {
    if draft.identities_only == Some(true) {
        "Only these key files are offered for login (IdentitiesOnly yes). The agent can still be forwarded after connecting."
    } else {
        "IdentitiesOnly yes is not set here, so keys in the SSH agent are offered too unless another setting turns that off. Changing the key or certificate files sets it."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{
        LoginSecretStep, LoginSendStep, LoginSequence, LoginSequenceStep, LoginWaitStep,
        OpenSshProfileMetadata, ResolvedHostSettings, SavedHostKind, saved_host_hop,
    };
    use muxus_session::login_sequence::editor::LoginSequenceMode;

    fn fallback_draft() -> HostDraft {
        HostDraft {
            alias_text: "app".into(),
            hostname: "app.internal".into(),
            route_mode: RouteMode::Jump,
            proxy_jump: vec!["bastion-a".into()],
            proxy_jump_fallbacks: vec![
                vec!["bastion-b".into()],
                vec!["edge".into(), "bastion-c".into()],
            ],
            ..blank_draft("", "", Storage::Muxus)
        }
    }

    #[test]
    fn saves_fallback_jump_routes_with_a_muxus_host_on_a_jump_route_only() {
        let draft = fallback_draft();
        let SavedHostSessionProfile::Ssh(profile) = draft_to_saved_ssh_input(&draft, None).profile
        else {
            panic!("an SSH profile");
        };
        assert_eq!(profile.proxy_jump, Some(vec!["bastion-a".to_owned()]));
        assert_eq!(
            profile.proxy_jump_fallbacks,
            Some(draft.proxy_jump_fallbacks.clone())
        );
        let direct = HostDraft {
            route_mode: RouteMode::Direct,
            ..draft.clone()
        };
        let SavedHostSessionProfile::Ssh(profile) = draft_to_saved_ssh_input(&direct, None).profile
        else {
            panic!("an SSH profile");
        };
        assert_eq!(profile.proxy_jump_fallbacks, None);
        // A Muxus host keeps them in its profile, an OpenSSH alias in its metadata.
        assert_eq!(
            draft_metadata_patch(&draft).proxy_jump_fallbacks,
            Patch::Keep
        );
        let alias = HostDraft {
            storage: Storage::Openssh,
            ..draft.clone()
        };
        assert_eq!(
            draft_metadata_patch(&alias).proxy_jump_fallbacks,
            Patch::Set(draft.proxy_jump_fallbacks.clone())
        );
        let alias_direct = HostDraft {
            route_mode: RouteMode::Direct,
            ..alias
        };
        assert_eq!(
            draft_metadata_patch(&alias_direct).proxy_jump_fallbacks,
            Patch::Clear
        );
    }

    #[test]
    fn needs_a_first_jump_host_before_a_fallback() {
        let draft = fallback_draft();
        assert_eq!(draft_problem(&draft), None);
        let problem = draft_problem(&HostDraft {
            proxy_jump: Vec::new(),
            ..draft
        })
        .unwrap();
        assert!(problem.contains("jump host before"), "{problem}");
    }

    #[test]
    fn keeps_muxus_saved_hosts_out_of_fallbacks_for_an_ssh_config_host() {
        let problem = draft_problem(&HostDraft {
            storage: Storage::Openssh,
            proxy_jump_fallbacks: vec![vec![saved_host_hop("abc")]],
            ..fallback_draft()
        })
        .unwrap();
        assert!(problem.contains("Muxus app data"), "{problem}");
    }

    #[test]
    fn carries_the_new_host_settings_through_edit_and_duplicate() {
        use muxus_core::api::{AntiIdle, TerminalEnterKey};
        let mut host = entry();
        host.metadata = Some(OpenSshProfileMetadata {
            anti_idle: Some(AntiIdle {
                interval_seconds: 120,
                text: " ".into(),
                enter: false,
            }),
            paste_prompt_wait: Some(Default::default()),
            enter_key: Some(TerminalEnterKey::Lf),
            system_open_ssh: Some(true),
            discover_environments: Some(false),
            proxy_jump_fallbacks: Some(vec![vec!["bastion-b".into()]]),
            ..host.metadata.clone().unwrap_or_default()
        });
        host.options.proxy_command = None;
        host.options.proxy_jump = Some(vec!["bastion-a".into()]);
        for duplicate in [false, true] {
            let draft = draft_from_entry(&host, duplicate);
            assert_eq!(draft.discover_environments, EnvironmentDiscoveryDraft::Off);
            let patch = draft_metadata_patch(&draft);
            assert_eq!(
                patch.anti_idle,
                Patch::Set(host.metadata.as_ref().unwrap().anti_idle.clone().unwrap())
            );
            assert_eq!(patch.paste_prompt_wait, Patch::Set(Default::default()));
            assert_eq!(patch.enter_key, Patch::Set(TerminalEnterKey::Lf));
            assert_eq!(patch.backspace_key, Patch::Clear);
            assert_eq!(patch.system_open_ssh, Some(true));
            assert_eq!(patch.discover_environments, Patch::Set(false));
            assert_eq!(
                patch.proxy_jump_fallbacks,
                Patch::Set(vec![vec!["bastion-b".to_owned()]])
            );
        }
        let blank = draft_metadata_patch(&blank_draft("", "", Storage::Openssh));
        assert_eq!(blank.anti_idle, Patch::Clear);
        assert_eq!(blank.discover_environments, Patch::Clear);
        assert_eq!(blank.system_open_ssh, Some(false));
    }

    fn entry() -> SshHostEntry {
        SshHostEntry {
            alias: "cloud".into(),
            aliases: vec!["cloud".into()],
            description: None,
            file: "/home/test/.ssh/config".into(),
            options: HostBlockOptions {
                hostname: Some("cloud.example.test".into()),
                identity_files: Some(vec!["~/.ssh/cloud".into()]),
                certificate_files: Some(vec!["~/.ssh/cloud-cert.pub".into()]),
                identity_agent: Some("${ONEPASSWORD_SSH_AUTH_SOCK}".into()),
                proxy_command: Some("cloudflared access ssh --hostname %h".into()),
                remote_command: Some("tmux new -A -s main".into()),
                request_tty: Some(RequestTty::Yes),
                strict_host_key_checking: Some(StrictHostKeyChecking::AcceptNew),
                ..HostBlockOptions::default()
            },
            resolved: ResolvedHostSettings {
                hostname: "cloud.example.test".into(),
                port: 22,
                identity_files: vec!["/home/test/.ssh/cloud".into()],
                certificate_files: vec!["/home/test/.ssh/cloud-cert.pub".into()],
                identity_agent: Some("${ONEPASSWORD_SSH_AUTH_SOCK}".into()),
                proxy_command: Some("cloudflared access ssh --hostname %h".into()),
                ..ResolvedHostSettings::default()
            },
            metadata: None,
        }
    }

    fn saved(
        profile: SshProfile,
        metadata: OpenSshProfileMetadata,
        name: &str,
    ) -> SavedHostProfile {
        SavedHostProfile {
            id: metadata.profile_id.clone(),
            kind: SavedHostKind::Ssh,
            name: name.into(),
            profile: SavedHostSessionProfile::Ssh(profile),
            metadata,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn loads_and_saves_a_self_contained_muxus_ssh_profile() {
        let saved = saved(
            SshProfile {
                profile_id: Some("muxus-router".into()),
                target: "router.example.test".into(),
                use_config: Some(false),
                user: Some("admin".into()),
                port: Some(2222),
                identity_files: Some(vec!["~/.ssh/router".into()]),
                identities_only: Some(true),
                proxy_jump: Some(vec!["bastion".into()]),
                ..SshProfile::default()
            },
            OpenSshProfileMetadata {
                profile_id: "muxus-router".into(),
                group: Some("Production".into()),
                terminal_scheme: Some("nord".into()),
                terminal_font_color: Some("#f8f8f2".into()),
                terminal_background_color: Some("#282a36".into()),
                ..OpenSshProfileMetadata::default()
            },
            "Core router",
        );
        let draft = draft_from_saved_ssh_profile(&saved, false).unwrap();
        assert_eq!(draft.storage, Storage::Muxus);
        assert_eq!(draft.alias_text, "Core router");
        assert_eq!(draft.hostname, "router.example.test");
        assert_eq!(draft.user, "admin");
        assert_eq!(draft.port, "2222");
        assert_eq!(draft.group, "Production");
        assert_eq!(draft.terminal_scheme.as_deref(), Some("nord"));
        assert_eq!(draft.terminal_font_color.as_deref(), Some("#f8f8f2"));
        assert_eq!(draft.terminal_background_color.as_deref(), Some("#282a36"));
        assert_eq!(draft.auth_mode, AuthMode::Key);
        assert_eq!(draft.route_mode, RouteMode::Jump);
        assert_eq!(draft_problem(&draft), None);
        assert_eq!(
            serde_json::to_value(draft_to_saved_ssh_input(&draft, Some(&saved.id))).unwrap(),
            serde_json::json!({
                "id": "muxus-router",
                "name": "Core router",
                "profile": {
                    "kind": "ssh",
                    "target": "router.example.test",
                    "useConfig": false,
                    "user": "admin",
                    "port": 2222,
                    "identityFiles": ["~/.ssh/router"],
                    "certificateFiles": [],
                    "identitiesOnly": true,
                    "proxyJump": ["bastion"],
                },
            })
        );
    }

    #[test]
    fn keeps_saved_muxus_jump_hosts_out_of_ssh_config_storage() {
        let draft = HostDraft {
            alias_text: "router".into(),
            hostname: "router.example.test".into(),
            route_mode: RouteMode::Jump,
            proxy_jump: vec![saved_host_hop("muxus-bastion")],
            ..HostDraft::default()
        };
        let muxus = HostDraft {
            storage: Storage::Muxus,
            ..draft.clone()
        };
        assert_eq!(draft_problem(&muxus), None);
        assert!(
            draft_problem(&draft)
                .unwrap()
                .contains("Muxus app data storage")
        );
    }

    #[test]
    fn loads_and_saves_modeled_connection_options_as_first_class_fields() {
        let draft = draft_from_entry(&entry(), false);
        assert_eq!(draft.auth_mode, AuthMode::Key);
        assert_eq!(draft.certificate_files, vec!["~/.ssh/cloud-cert.pub"]);
        assert_eq!(draft.identity_agent_mode, IdentityAgentMode::Custom);
        assert_eq!(draft.identity_agent, "${ONEPASSWORD_SSH_AUTH_SOCK}");
        assert_eq!(draft.route_mode, RouteMode::Command);
        assert_eq!(draft.proxy_command, "cloudflared access ssh --hostname %h");
        assert_eq!(draft.remote_command_mode, RemoteCommandMode::Command);
        assert_eq!(draft.remote_command, "tmux new -A -s main");
        assert_eq!(draft.request_tty, Some(RequestTty::Yes));
        assert_eq!(
            draft.strict_host_key_checking,
            Some(StrictHostKeyChecking::AcceptNew)
        );

        let options = draft_to_request(&draft, Some("cloud")).options;
        assert_eq!(options.identity_files, Some(vec!["~/.ssh/cloud".into()]));
        assert_eq!(
            options.certificate_files,
            Some(vec!["~/.ssh/cloud-cert.pub".into()])
        );
        // The entry lists IdentityFile alone, and saving keeps it that way.
        assert_eq!(options.identities_only, None);
        assert_eq!(
            options.identity_agent.as_deref(),
            Some("${ONEPASSWORD_SSH_AUTH_SOCK}")
        );
        assert_eq!(
            options.proxy_command.as_deref(),
            Some("cloudflared access ssh --hostname %h")
        );
        assert_eq!(
            options.remote_command.as_deref(),
            Some("tmux new -A -s main")
        );
        assert_eq!(options.request_tty, Some(RequestTty::Yes));
        assert_eq!(
            options.strict_host_key_checking,
            Some(StrictHostKeyChecking::AcceptNew)
        );
    }

    #[test]
    fn starts_new_hosts_with_direct_routing_and_no_certificates() {
        let draft = HostDraft::default();
        assert_eq!(draft.route_mode, RouteMode::Direct);
        assert!(draft.certificate_files.is_empty());
        assert_eq!(draft.identity_agent_mode, IdentityAgentMode::Default);
        assert_eq!(draft.remote_command_mode, RemoteCommandMode::Inherit);
        assert_eq!(draft.request_tty, None);
        assert_eq!(draft.strict_host_key_checking, None);
        assert_eq!(draft.identities_only, Some(true));
        assert!(!draft.disable_sftp);
        assert!(!draft.console_compatibility);
        assert_eq!(draft.terminal_scheme, None);
        assert_eq!(draft.terminal_font_color, None);
        assert_eq!(draft.terminal_background_color, None);
        assert_eq!(draft_to_request(&draft, None).options.proxy_command, None);
    }

    #[test]
    fn loads_the_muxus_only_sftp_compatibility_setting() {
        let mut entry = entry();
        entry.metadata = Some(OpenSshProfileMetadata {
            profile_id: "profile-cloud".into(),
            disable_sftp: Some(true),
            console_compatibility: Some(true),
            terminal_scheme: Some("solarized-dark".into()),
            terminal_font_color: Some("#eeeeee".into()),
            terminal_background_color: Some("#101820".into()),
            ..OpenSshProfileMetadata::default()
        });
        let draft = draft_from_entry(&entry, false);
        assert!(draft.disable_sftp);
        assert!(draft.console_compatibility);
        assert_eq!(draft.terminal_scheme.as_deref(), Some("solarized-dark"));
        assert_eq!(draft.terminal_font_color.as_deref(), Some("#eeeeee"));
        assert_eq!(draft.terminal_background_color.as_deref(), Some("#101820"));
        let options = serde_json::to_value(draft_to_request(&draft, None).options).unwrap();
        assert!(options.get("disableSftp").is_none());
        assert!(options.get("consoleCompatibility").is_none());
    }

    #[test]
    fn makes_the_specific_key_promise_independent_of_the_ssh_agent() {
        let mut created = blank_draft("", "", Storage::Openssh);
        created.auth_mode = AuthMode::Key;
        auth_choice(&mut created);
        created.identity_files = vec!["~/.ssh/id_ed25519".into()];
        auth_choice(&mut created);
        let options = draft_to_request(&created, None).options;
        assert_eq!(
            options.identity_files,
            Some(vec!["~/.ssh/id_ed25519".into()])
        );
        assert_eq!(options.identities_only, Some(true));

        // Hosts saved by v0.3.0 list IdentityFile alone. Choosing their keys again makes the
        // promise, so the save repairs them.
        let loaded = draft_from_entry(&entry(), false);
        let mut more_keys = loaded.clone();
        more_keys.identity_files.push("~/.ssh/backup".into());
        auth_choice(&mut more_keys);
        assert_eq!(
            draft_to_request(&more_keys, Some("cloud"))
                .options
                .identities_only,
            Some(true)
        );
        let mut no_certs = loaded.clone();
        no_certs.certificate_files.clear();
        auth_choice(&mut no_certs);
        assert_eq!(
            draft_to_request(&no_certs, Some("cloud"))
                .options
                .identities_only,
            Some(true)
        );
    }

    #[test]
    fn keeps_identities_only_as_the_config_has_it_when_other_fields_change() {
        // A hand-written IdentityFile without IdentitiesOnly still offers agent keys; saving an
        // unrelated field must not take them away.
        let mut plain = entry();
        plain.options.identities_only = None;
        let mut draft = draft_from_entry(&plain, false);
        draft.terminal_scheme = Some("nord".into());
        assert_eq!(draft.auth_mode, AuthMode::Key);
        assert_eq!(
            draft_to_request(&draft, Some("cloud"))
                .options
                .identities_only,
            None
        );
        assert_eq!(
            draft_to_request(&draft_from_entry(&plain, true), None)
                .options
                .identities_only,
            None
        );

        let mut explicit_no = entry();
        explicit_no.options.identities_only = Some(false);
        let mut draft = draft_from_entry(&explicit_no, false);
        draft.user = "ops".into();
        assert_eq!(
            draft_to_request(&draft, Some("cloud"))
                .options
                .identities_only,
            Some(false)
        );
    }

    #[test]
    fn keeps_identities_only_as_a_saved_muxus_host_has_it_when_other_fields_change() {
        for identities_only in [None, Some(false)] {
            let saved: SavedHostProfile = serde_json::from_value(serde_json::json!({
                "id": "imported-router", "kind": "ssh", "name": "Imported router",
                "profile": {
                    "kind": "ssh", "profileId": "imported-router",
                    "target": "router.example.test", "useConfig": false,
                    "identityFiles": ["~/.ssh/router"],
                    "identitiesOnly": identities_only,
                },
                "metadata": { "profileId": "imported-router", "connectCount": 0 },
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            }))
            .unwrap();
            let mut draft = draft_from_saved_ssh_profile(&saved, false).unwrap();
            draft.terminal_background_color = Some("#101820".into());
            let SavedHostSessionProfile::Ssh(profile) =
                draft_to_saved_ssh_input(&draft, Some("imported-router")).profile
            else {
                panic!("an SSH profile");
            };
            assert_eq!(profile.identity_files, Some(vec!["~/.ssh/router".into()]));
            assert_eq!(profile.identities_only, identities_only);
        }
    }

    #[test]
    fn promises_exactly_these_keys_only_where_the_draft_sets_identities_only_yes() {
        let mut plain = entry();
        plain.options.identities_only = None;
        let loaded = draft_from_entry(&plain, false);
        let mut explicit_no = entry();
        explicit_no.options.identities_only = Some(false);
        let explicit_no = draft_from_entry(&explicit_no, false);
        let muxus = HostDraft {
            storage: Storage::Muxus,
            ..loaded.clone()
        };
        for draft in [&loaded, &explicit_no, &muxus] {
            assert!(!key_mode_help(draft).to_lowercase().contains("exactly"));
            // Host *, folder credentials or the agent source may still turn agent keys off.
            assert_eq!(
                key_files_caption(draft),
                "IdentitiesOnly yes is not set here, so keys in the SSH agent are offered too unless another setting turns that off. Changing the key or certificate files sets it."
            );
        }
        assert_eq!(key_mode_help(&loaded), "Writes IdentityFile");

        // Choosing keys, or the mode from another one, makes the promise.
        let mut chosen = loaded.clone();
        chosen.identity_files = vec!["~/.ssh/cloud".into()];
        auth_choice(&mut chosen);
        assert_eq!(
            key_mode_help(&chosen),
            "Writes IdentityFile — exactly these keys are offered"
        );
        assert!(key_files_caption(&chosen).starts_with("Only these key files are offered"));
        let blank_muxus = blank_draft("", "", Storage::Muxus);
        assert_eq!(
            key_mode_help(&blank_muxus),
            "Exactly these keys are offered for this Muxus connection"
        );
    }

    #[test]
    fn round_trips_explicit_agent_and_login_shell_choices() {
        let draft = HostDraft {
            identity_agent_mode: IdentityAgentMode::Environment,
            remote_command_mode: RemoteCommandMode::Shell,
            request_tty: Some(RequestTty::Auto),
            ..HostDraft::default()
        };
        let options = draft_to_request(&draft, None).options;
        assert_eq!(options.identity_agent.as_deref(), Some("SSH_AUTH_SOCK"));
        assert_eq!(options.remote_command.as_deref(), Some("none"));
        assert_eq!(options.request_tty, Some(RequestTty::Auto));
        let SavedHostSessionProfile::Ssh(profile) = draft_to_saved_ssh_input(&draft, None).profile
        else {
            panic!("expected an SSH profile");
        };
        assert_eq!(profile.remote_command, None);
    }

    #[test]
    fn writes_forward_x11_only_when_the_host_overrides_the_default() {
        let mut draft = HostDraft::default();
        assert_eq!(draft.forward_x11, ForwardX11Mode::Inherit);
        assert_eq!(draft_to_request(&draft, None).options.forward_x11, None);
        draft.forward_x11 = ForwardX11Mode::No;
        assert_eq!(
            draft_to_request(&draft, None).options.forward_x11,
            Some(false)
        );
        draft.forward_x11 = ForwardX11Mode::Yes;
        let SavedHostSessionProfile::Ssh(profile) = draft_to_saved_ssh_input(&draft, None).profile
        else {
            panic!("expected an SSH profile");
        };
        assert_eq!(profile.forward_x11, Some(true));

        let mut off = entry();
        off.options.forward_x11 = Some(false);
        assert_eq!(
            draft_from_entry(&off, false).forward_x11,
            ForwardX11Mode::No
        );
        assert_eq!(
            draft_from_entry(&entry(), false).forward_x11,
            ForwardX11Mode::Inherit
        );
    }

    #[test]
    fn selects_the_current_per_host_agent_for_live_key_detection() {
        let inherited = Some("${INHERITED_AGENT}");
        assert_eq!(
            identity_agent_for_detection(IdentityAgentMode::Default, "", inherited).as_deref(),
            Some("${INHERITED_AGENT}")
        );
        assert_eq!(
            identity_agent_for_detection(IdentityAgentMode::Environment, "", inherited).as_deref(),
            Some("SSH_AUTH_SOCK")
        );
        assert_eq!(
            identity_agent_for_detection(
                IdentityAgentMode::Custom,
                "  ~/.1password/agent.sock  ",
                inherited
            )
            .as_deref(),
            Some("~/.1password/agent.sock")
        );
        assert_eq!(
            identity_agent_for_detection(IdentityAgentMode::Custom, " ", inherited).as_deref(),
            Some("none")
        );
        assert_eq!(
            identity_agent_for_detection(IdentityAgentMode::None, "", inherited).as_deref(),
            Some("none")
        );
    }

    #[test]
    fn splits_a_prefilled_quick_connect_target_and_leaves_a_bare_name_as_the_alias() {
        let draft = blank_draft("ops@edge01.lab.test:2222", "", Storage::Openssh);
        assert_eq!(
            (
                draft.alias_text.as_str(),
                draft.hostname.as_str(),
                draft.user.as_str(),
                draft.port.as_str()
            ),
            ("edge01.lab.test", "edge01.lab.test", "ops", "2222")
        );
        let draft = blank_draft("myairframe4", "", Storage::Openssh);
        assert_eq!(
            (
                draft.alias_text.as_str(),
                draft.hostname.as_str(),
                draft.user.as_str(),
                draft.port.as_str()
            ),
            ("myairframe4", "", "", "")
        );
    }

    #[test]
    fn starts_a_new_host_in_the_requested_storage() {
        assert_eq!(HostDraft::default().storage, Storage::Openssh);
        assert_eq!(blank_draft("", "", Storage::Muxus).storage, Storage::Muxus);
    }

    #[test]
    fn uses_a_bare_prefilled_name_as_the_hostname_of_a_muxus_profile() {
        let draft = blank_draft("myairframe4", "", Storage::Muxus);
        assert_eq!(
            (draft.alias_text.as_str(), draft.hostname.as_str()),
            ("myairframe4", "myairframe4")
        );
        assert_eq!(draft_problem(&draft), None);
        let draft = blank_draft("ops@edge01.lab.test:2222", "", Storage::Muxus);
        assert_eq!(
            (
                draft.alias_text.as_str(),
                draft.hostname.as_str(),
                draft.user.as_str(),
                draft.port.as_str()
            ),
            ("edge01.lab.test", "edge01.lab.test", "ops", "2222")
        );
    }

    #[test]
    fn prefills_the_selected_folder_for_a_new_host() {
        assert_eq!(
            blank_draft("", "Production/Edge", Storage::Openssh).group,
            "Production/Edge"
        );
    }

    #[test]
    fn loads_the_login_sequence_from_either_host_source_and_keeps_it_out_of_ssh_config() {
        let steps = vec![
            LoginSequenceStep::Wait(LoginWaitStep {
                id: "w".into(),
                pattern: "[>#]\\s*$".into(),
                regex: Some(true),
                timeout_seconds: 15,
            }),
            LoginSequenceStep::Send(LoginSendStep {
                id: "s".into(),
                text: "terminal length 0".into(),
                enter: true,
            }),
        ];
        assert_eq!(
            HostDraft::default().login_sequence,
            LoginSequenceDraft {
                mode: LoginSequenceMode::Inherit,
                steps: Vec::new()
            }
        );
        let mut with_sequence = entry();
        with_sequence.metadata = Some(OpenSshProfileMetadata {
            profile_id: "p".into(),
            login_sequence: Some(LoginSequence {
                steps: steps.clone(),
            }),
            ..OpenSshProfileMetadata::default()
        });
        let from_config = draft_from_entry(&with_sequence, true);
        assert_eq!(
            from_config.login_sequence,
            LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps
            }
        );
        assert_eq!(draft_problem(&from_config), None);
        assert!(
            !serde_json::to_string(&draft_to_request(&from_config, None))
                .unwrap()
                .contains("terminal length")
        );

        let from_profile = draft_from_saved_ssh_profile(
            &saved(
                SshProfile {
                    profile_id: Some("r".into()),
                    target: "router.test".into(),
                    use_config: Some(false),
                    ..SshProfile::default()
                },
                OpenSshProfileMetadata {
                    profile_id: "r".into(),
                    login_sequence: Some(LoginSequence { steps: Vec::new() }),
                    ..OpenSshProfileMetadata::default()
                },
                "Router",
            ),
            false,
        )
        .unwrap();
        assert_eq!(from_profile.login_sequence.mode, LoginSequenceMode::None);
        assert!(
            !serde_json::to_string(&draft_to_saved_ssh_input(&from_profile, None))
                .unwrap()
                .contains("loginSequence")
        );
        let unfinished = HostDraft {
            login_sequence: LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps: vec![LoginSequenceStep::Secret(LoginSecretStep {
                    id: "x".into(),
                    secret_id: String::new(),
                    enter: true,
                })],
            },
            ..from_config
        };
        assert_eq!(
            draft_problem(&unfinished).as_deref(),
            Some("Login sequence step 1: Choose a secret from the password vault.")
        );
    }

    #[test]
    fn validates_forwards_and_option_keywords() {
        let mut draft = blank_draft("web", "", Storage::Openssh);
        draft.forwards.push(ConfigForward {
            kind: ForwardType::Local,
            bind_port: 8080,
            target_host: Some(" ".into()),
            target_port: Some(80),
        });
        assert_eq!(
            draft_problem(&draft).as_deref(),
            Some("Local/remote forwards need a target host and port.")
        );
        draft.forwards[0].kind = ForwardType::Dynamic;
        assert_eq!(draft_problem(&draft), None);
        draft.extras.push(HostOptionExtra {
            keyword: "9Bad".into(),
            value: "x".into(),
        });
        assert_eq!(
            draft_problem(&draft).as_deref(),
            Some("\"9Bad\" is not a valid option keyword.")
        );
        draft.extras.clear();
        draft.alias_text = "web*".into();
        assert!(
            draft_problem(&draft)
                .unwrap()
                .contains("is not a valid alias")
        );
    }
}
