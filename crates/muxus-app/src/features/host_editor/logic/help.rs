//! The helper texts the SSH editor's selects show under them
//! (`GeneralSection.tsx`, `AuthSection.tsx`, `ForwardsSection.tsx`).

use muxus_core::api::{SshKeysResponse, X11Source, X11Status};
use muxus_core::protocol::{RequestTty, StrictHostKeyChecking};

use super::draft::{ForwardX11Mode, HostDraft, IdentityAgentMode, RemoteCommandMode};
use muxus_core::js::js_trim;

pub fn remote_command_help(mode: RemoteCommandMode, config_backed: bool) -> &'static str {
    match mode {
        RemoteCommandMode::Shell => {
            "Explicitly disables an inherited RemoteCommand and opens the normal shell."
        }
        RemoteCommandMode::Command => {
            "Runs one command after authentication instead of opening the normal shell."
        }
        RemoteCommandMode::Inherit if config_backed => {
            "Uses any RemoteCommand inherited from matching SSH configuration."
        }
        RemoteCommandMode::Inherit => "Opens the server’s default login shell.",
    }
}

pub fn request_tty_help(
    value: Option<RequestTty>,
    command_mode: RemoteCommandMode,
    config_backed: bool,
) -> &'static str {
    let command = command_mode == RemoteCommandMode::Command;
    match value {
        Some(RequestTty::Auto) if command => {
            "A startup command runs without a terminal; choose “Always” for interactive tools such as tmux."
        }
        Some(RequestTty::Auto) => "Allocates a terminal for a login shell.",
        Some(RequestTty::Yes) => "Allocates a terminal for both login shells and startup commands.",
        Some(RequestTty::Force) => {
            "Forces a terminal even when the local session is not interactive."
        }
        Some(RequestTty::No) => "Runs without a terminal.",
        None if !config_backed && command => {
            "Runs startup commands without a terminal unless you choose another setting."
        }
        None if !config_backed => "Allocates a terminal for a login shell.",
        None if command => {
            "Inherits RequestTTY; SSH normally runs startup commands without a terminal."
        }
        None => "Inherits RequestTTY; SSH normally allocates a terminal for a login shell.",
    }
}

pub fn host_verification_help(
    value: Option<StrictHostKeyChecking>,
    config_backed: bool,
) -> &'static str {
    match value {
        Some(StrictHostKeyChecking::Yes) => "Refuses hosts whose key is not already saved.",
        Some(StrictHostKeyChecking::AcceptNew) => {
            "Saves first-contact keys silently, but still warns if a saved key changes."
        }
        Some(StrictHostKeyChecking::No) => {
            "Accepts first-contact keys. Muxus still warns if a saved key changes."
        }
        Some(StrictHostKeyChecking::Ask) => {
            "Prompts before saving a first-contact key and whenever a saved key changes."
        }
        None if config_backed => "Inherits StrictHostKeyChecking; the normal default is to ask.",
        None => "Prompts before saving a first-contact key and whenever a saved key changes.",
    }
}

pub fn agent_source_help(mode: IdentityAgentMode, config_backed: bool) -> &'static str {
    match mode {
        IdentityAgentMode::Environment => {
            "Overrides inherited agent settings and reads SSH_AUTH_SOCK when connecting."
        }
        IdentityAgentMode::Custom => {
            "Use a 1Password, Secretive, or other agent socket for this host."
        }
        IdentityAgentMode::None => {
            "Disables agent authentication and agent forwarding for this host."
        }
        IdentityAgentMode::Default if config_backed => {
            "Inherits the normal SSH configuration and environment."
        }
        IdentityAgentMode::Default => {
            "Uses the agent from SSH_AUTH_SOCK and the normal default key files."
        }
    }
}

/// The line under the agent switch saying what was detected.
pub fn agent_detection_status(draft: &HostDraft, keys: Option<&SshKeysResponse>) -> String {
    if draft.identity_agent_mode == IdentityAgentMode::Custom
        && js_trim(&draft.identity_agent).is_empty()
    {
        return String::new();
    }
    if draft.identity_agent_mode == IdentityAgentMode::None {
        return "Agent disabled for this host.".into();
    }
    let Some(keys) = keys else {
        return String::new();
    };
    if keys.agent_available {
        let count = keys.agent_keys.len();
        return format!(
            "Agent detected — {count} key{} loaded.",
            if count == 1 { "" } else { "s" }
        );
    }
    match draft.identity_agent_mode {
        IdentityAgentMode::Environment => "No agent detected in SSH_AUTH_SOCK.".into(),
        IdentityAgentMode::Custom => "No agent detected at the selected socket.".into(),
        _ => "No agent detected for this host.".into(),
    }
}

/// The X11 forwarding select's helper text.
pub fn x11_help(
    mode: ForwardX11Mode,
    x11: &X11Status,
    platform: Option<&str>,
    config_backed: bool,
) -> String {
    // Switched off in Settings (the macOS default): a quiet note, no warnings.
    if !x11.enabled {
        return "X11 forwarding is turned off in Settings.".into();
    }
    if mode == ForwardX11Mode::No {
        return "Muxus never requests X11 forwarding for this host.".into();
    }
    if x11.source == X11Source::None {
        return if platform == Some("darwin") {
            "No X server found. Install XQuartz from xquartz.org, then log out and back in.".into()
        } else {
            "No local X server found, so X11 forwarding is unavailable.".into()
        };
    }
    let trust = "Forwarded programs can see other X11 windows on your desktop, so use this only for hosts you trust.";
    if mode == ForwardX11Mode::Yes {
        return if x11.source == X11Source::Bundled {
            "Remote windows open through the X server built into Muxus.".into()
        } else {
            format!(
                "Remote windows open on display {}. {trust}",
                x11.display.as_deref().unwrap_or("")
            )
        };
    }
    let fallback = if !x11.forward_by_default {
        "off"
    } else if x11.source == X11Source::Bundled {
        "on — Muxus includes its own X server"
    } else {
        "on — forwarded programs can watch other X11 windows on your desktop"
    };
    if config_backed {
        format!("Uses ForwardX11 from matching SSH configuration; otherwise {fallback} (Settings).")
    } else {
        let mut chars = fallback.chars();
        let first = chars
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        format!("{first}{} (Settings).", chars.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::X11Defaults;

    fn x11(source: X11Source, forward_by_default: bool) -> X11Status {
        X11Status {
            enabled: true,
            forward_by_default,
            clipboard: false,
            source,
            display: Some(":0".into()),
            defaults: X11Defaults {
                enabled: true,
                forward_by_default,
            },
        }
    }

    #[test]
    fn explains_the_x11_choice() {
        assert_eq!(
            x11_help(
                ForwardX11Mode::Inherit,
                &x11(X11Source::Display, true),
                Some("linux"),
                true
            ),
            "Uses ForwardX11 from matching SSH configuration; otherwise on — forwarded programs can watch other X11 windows on your desktop (Settings)."
        );
        assert_eq!(
            x11_help(
                ForwardX11Mode::Inherit,
                &x11(X11Source::Bundled, true),
                None,
                false
            ),
            "On — Muxus includes its own X server (Settings)."
        );
        assert_eq!(
            x11_help(
                ForwardX11Mode::Inherit,
                &x11(X11Source::Display, false),
                None,
                false
            ),
            "Off (Settings)."
        );
        assert!(
            x11_help(
                ForwardX11Mode::Yes,
                &x11(X11Source::Display, true),
                None,
                true
            )
            .starts_with("Remote windows open on display :0.")
        );
        assert_eq!(
            x11_help(
                ForwardX11Mode::Yes,
                &x11(X11Source::None, true),
                Some("darwin"),
                true
            ),
            "No X server found. Install XQuartz from xquartz.org, then log out and back in."
        );
    }

    #[test]
    fn explains_tty_and_agent_choices() {
        assert_eq!(
            request_tty_help(None, RemoteCommandMode::Command, true),
            "Inherits RequestTTY; SSH normally runs startup commands without a terminal."
        );
        assert_eq!(
            request_tty_help(None, RemoteCommandMode::Inherit, false),
            "Allocates a terminal for a login shell."
        );
        let mut draft = HostDraft::default();
        assert_eq!(agent_detection_status(&draft, None), "");
        draft.identity_agent_mode = IdentityAgentMode::None;
        assert_eq!(
            agent_detection_status(&draft, None),
            "Agent disabled for this host."
        );
        draft.identity_agent_mode = IdentityAgentMode::Default;
        let keys = SshKeysResponse {
            agent_available: false,
            agent_keys: Vec::new(),
            keys: Vec::new(),
        };
        assert_eq!(
            agent_detection_status(&draft, Some(&keys)),
            "No agent detected for this host."
        );
    }
}
