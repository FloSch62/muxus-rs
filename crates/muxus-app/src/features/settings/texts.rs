//! The words the settings show where they depend on state: descriptions,
//! option lists and status lines from `SettingsDialog.tsx` and the section
//! components, kept apart from the views so they can be tested.

use muxus_core::api::{
    AppInfo, KeywordHighlightProfile, PasswordVaultStatus, PasswordVaultUnlockPolicy, X11Source,
    X11Status,
};
use muxus_core::protocol::DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS;
use muxus_store::prefs::layout::{INTERFACE_ZOOM_STEPS, interface_zoom_label};
use muxus_ui::theme::Status;

use super::js::number_to_string;

/// The Interface scale row's explanation, naming the terminal zoom chords.
pub fn interface_scale_description(zoom_in: Option<&str>, zoom_out: Option<&str>) -> String {
    let chords = match (zoom_in, zoom_out) {
        (Some(zoom_in), Some(zoom_out)) => format!(" ({zoom_in} / {zoom_out} or Ctrl+scroll)"),
        _ => " (Ctrl+scroll)".into(),
    };
    format!(
        "Scales the whole window. Terminal text has its own zoom{chords}, which this does not touch."
    )
}

/// The Interface scale options: `String(step)` values with percent labels.
pub fn interface_zoom_options() -> Vec<(String, String)> {
    INTERFACE_ZOOM_STEPS
        .iter()
        .map(|&step| {
            let default = if step == 1. { " (default)" } else { "" };
            (
                number_to_string(step),
                format!("{}{default}", interface_zoom_label(step)),
            )
        })
        .collect()
}

/// What the Font family row says, and whether it says it as an error.
pub fn font_family_description(
    loading: bool,
    available: Option<bool>,
    family: &str,
    installed_known: bool,
) -> (String, bool) {
    if loading {
        return ("Reading installed fonts\u{2026}".into(), false);
    }
    if available == Some(false) {
        let name = muxus_core::js::js_trim(family);
        let name = if name.is_empty() { "This font" } else { name };
        return (
            format!("{name} is not installed; JetBrains Mono is used instead."),
            true,
        );
    }
    if installed_known {
        (
            "JetBrains Mono is bundled; the other choices are installed on this machine.".into(),
            false,
        )
    } else {
        (
            "JetBrains Mono is bundled; other font names use the system installation when available."
                .into(),
            false,
        )
    }
}

/// The Command bar row's note (`commandBarNote`).
pub fn command_bar_note(show_command_bar: bool, has_command_buttons: bool) -> &'static str {
    if !show_command_bar {
        "Turned off. Switch it back on from the saved command buttons control in the top bar."
    } else if !has_command_buttons {
        "Appears once a command button has been saved."
    } else {
        "Holds your saved command buttons."
    }
}

pub fn text_color_description(overridden: bool) -> &'static str {
    if overridden {
        "Replaces the scheme's default text color. Output that picks its own ANSI colors keeps the scheme palette."
    } else {
        "Following the color scheme."
    }
}

pub fn background_color_description(overridden: bool) -> &'static str {
    if overridden {
        "Replaces the scheme's background in every terminal."
    } else {
        "Following the color scheme."
    }
}

/// `SSH_KEEPALIVE_CHOICES` as options. A hand-edited or newer-version value
/// must stay visible and active instead of rendering the select blank.
pub fn keepalive_options(current: f64) -> Vec<(f64, String)> {
    let default = f64::from(DEFAULT_SSH_KEEPALIVE_INTERVAL_SECONDS);
    let mut options = vec![
        (0., "SSH configuration only".to_string()),
        (15., "Every 15 seconds".to_string()),
        (default, "Every 30 seconds (recommended)".to_string()),
        (60., "Every minute".to_string()),
        (120., "Every 2 minutes".to_string()),
    ];
    if !options.iter().any(|(value, _)| *value == current) {
        options.push((
            current,
            format!("Every {} seconds (custom)", number_to_string(current)),
        ));
    }
    options
}

/// The vault row's status line (`vaultStatus`).
pub fn vault_status(status: &PasswordVaultStatus) -> (Status, &'static str) {
    if !status.configured {
        return (
            Status::Info,
            "Password saving is off. Create a master password to protect viewing and editing saved SSH passwords.",
        );
    }
    if status.unlock_policy == Some(PasswordVaultUnlockPolicy::Credential) {
        return (
            Status::Info,
            "Muxus asks for the master password whenever a saved credential is needed.",
        );
    }
    if !status.locked {
        return (
            Status::Success,
            if status.unlock_policy == Some(PasswordVaultUnlockPolicy::Never) {
                "OS credential-store access is ready. Saved passwords are used without a master-password prompt."
            } else {
                "Unlocked for this app session."
            },
        );
    }
    if status.unlock_policy == Some(PasswordVaultUnlockPolicy::Never) {
        (
            Status::Error,
            "The OS credential-store key is unavailable. Enter the master password to restore it.",
        )
    } else {
        (
            Status::Warning,
            "Needs the master password for this app session.",
        )
    }
}

/// `UNLOCK_POLICY_LABELS`.
pub fn unlock_policy_label(policy: PasswordVaultUnlockPolicy) -> &'static str {
    match policy {
        PasswordVaultUnlockPolicy::Never => {
            "Never for saved credentials: the vault key lives in the OS credential store."
        }
        PasswordVaultUnlockPolicy::Startup => {
            "When Muxus starts: the vault is unlocked into memory once per app session."
        }
        PasswordVaultUnlockPolicy::Credential => "Whenever a saved credential is needed.",
    }
}

/// The options of the master-password dialog's policy select.
pub const UNLOCK_POLICY_OPTIONS: [(PasswordVaultUnlockPolicy, &str); 3] = [
    (
        PasswordVaultUnlockPolicy::Never,
        "Never for saved credentials (OS keyring)",
    ),
    (PasswordVaultUnlockPolicy::Startup, "When Muxus starts"),
    (
        PasswordVaultUnlockPolicy::Credential,
        "Whenever a saved credential is needed",
    ),
];

/// The X11 default row's explanation for the server in use.
pub fn x11_default_description(source: X11Source) -> &'static str {
    if source == X11Source::Bundled {
        "Hosts whose ForwardX11 is unset forward X11 to the built-in X server, where each connection gets its own display. A host\u{2019}s own setting always wins."
    } else {
        "Hosts whose ForwardX11 is unset forward X11 to your display. Forwarded programs can watch your other X11 windows, so turning X11 on per host is safer. A host\u{2019}s own setting always wins."
    }
}

/// The alert under the X11 switch (`ServerStatus`). macOS links XQuartz.
pub fn x11_server_status(status: &X11Status, platform: Option<&str>) -> (Status, String) {
    match status.source {
        X11Source::Bundled => (
            Status::Success,
            "Built-in X server. Each SSH connection gets its own display, started when a program opens its first window.".into(),
        ),
        X11Source::Display => (
            Status::Success,
            format!(
                "{} on display {}.",
                if platform == Some("darwin") {
                    "XQuartz"
                } else {
                    "Your X server"
                },
                status.display.as_deref().unwrap_or_default()
            ),
        ),
        X11Source::None => (
            Status::Warning,
            match platform {
                Some("darwin") => {
                    "No X server found. Install XQuartz, then log out and back in.".into()
                }
                Some("win32") => "This Muxus build has no built-in X server. Set DISPLAY to use your own X server.".into(),
                _ => "No X server found. Start Muxus from a graphical session so that DISPLAY is set.".into(),
            },
        ),
    }
}

/// `updateReasonLabel`.
pub fn update_reason_label(reason: Option<&str>) -> String {
    match reason {
        Some("store") => "Microsoft Store manages updates for this installation.".into(),
        Some("disabled") => "No updates are published for this build.".into(),
        Some("store-open-failed") => "Open Microsoft Store to check for Muxus updates.".into(),
        Some("timeout") => "The update check timed out.".into(),
        Some("network") => "The update check could not reach GitHub.".into(),
        Some("no-release") => "No published release was found.".into(),
        Some("missing-version" | "missing-release-url") => {
            "The latest release metadata is incomplete.".into()
        }
        Some(reason) if reason.starts_with("manifest-") => format!(
            "The update manifest returned {}.",
            reason.replacen("manifest-", "", 1)
        ),
        _ => "The update check could not be completed.".into(),
    }
}

/// The documentation site.
pub const DOCS_URL: &str = "https://flosch62.github.io/muxus/";

/// The docs page with this version's release notes: `0.8.1` → `/release-notes/0.8/`.
pub fn release_notes_url(version: Option<&str>) -> String {
    let minor = version.and_then(|version| {
        let mut parts = version.splitn(3, '.');
        let major = parts
            .next()
            .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?;
        let minor: String = parts
            .next()?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!minor.is_empty()).then(|| format!("{major}.{minor}"))
    });
    match minor {
        Some(minor) => format!("{DOCS_URL}release-notes/{minor}/"),
        None => format!("{DOCS_URL}release-notes/"),
    }
}

/// `platformLabel`.
pub fn platform_label(platform: &str) -> String {
    match platform {
        "darwin" => "macOS".into(),
        "win32" => "Windows".into(),
        "linux" => "Linux".into(),
        other => other.into(),
    }
}

/// `installationFacts` for the desktop app: what a bug report needs, never
/// the home directory or a saved host.
pub fn installation_facts(info: Option<&AppInfo>) -> Vec<(&'static str, String)> {
    vec![
        (
            "Version",
            info.map_or_else(|| "unknown".into(), |info| info.version.clone()),
        ),
        ("Runs as", "Desktop app".into()),
        (
            "Platform",
            platform_label(muxus_core::platform::Platform::current().node_name()),
        ),
    ]
}

/// The Automatic shell row's explanation.
pub fn automatic_shell_description(info: Option<&AppInfo>) -> String {
    match info {
        Some(info) => format!(
            "Used when no saved profile is the default. auto starts your login shell ({}); an executable name or path overrides it.",
            info.default_shell
        ),
        None => "Used when no saved profile is the default. auto starts your login shell; an executable name or path overrides it.".into(),
    }
}

/// A highlighting profile in the profile picker.
pub fn highlight_profile_option(profile: &KeywordHighlightProfile) -> String {
    format!("{} ({})", profile.name, profile.rules.len())
}

/// `unusedProfileName`: "New profile", then "New profile 2", …
pub fn unused_profile_name(profiles: &[KeywordHighlightProfile]) -> String {
    let names: std::collections::HashSet<String> = profiles
        .iter()
        .map(|profile| profile.name.to_lowercase())
        .collect();
    if !names.contains("new profile") {
        return "New profile".into();
    }
    let mut suffix = 2;
    while names.contains(&format!("new profile {suffix}")) {
        suffix += 1;
    }
    format!("New profile {suffix}")
}

/// A group title with its count, as the lists show it (`Secrets · 2`).
pub fn counted_title(title: &str, count: usize) -> String {
    if count > 0 {
        format!("{title} \u{b7} {count}")
    } else {
        title.into()
    }
}

/// The Keyboard section's button: `All shortcuts · 2 customized`.
pub fn all_shortcuts_label(customized: usize) -> String {
    if customized > 0 {
        format!("All shortcuts \u{b7} {customized} customized")
    } else {
        "All shortcuts".into()
    }
}

/// The local-terminal logging override's caption.
pub fn inherited_logging_caption(inherit: bool, enabled: bool, log_to_file: bool) -> String {
    if inherit {
        format!(
            "Inherited: logging {}{}.",
            if enabled { "enabled" } else { "disabled" },
            if log_to_file { ", log files on" } else { "" }
        )
    } else {
        "This host has its own logging and retention policy.".into()
    }
}

/// The File name field's helper: the pattern's error, or the placeholders
/// with an example.
pub fn log_file_pattern_helper(pattern: &str, now: chrono::NaiveDateTime) -> (String, bool) {
    use muxus_core::session_log_file::{
        SessionLogFileNameValues, session_log_file_name, session_log_file_pattern_error,
    };
    if let Some(error) = session_log_file_pattern_error(pattern) {
        return (error, true);
    }
    let example = session_log_file_name(
        pattern,
        &SessionLogFileNameValues {
            host: "router1".into(),
            title: "Core router".into(),
            kind: "ssh".into(),
            started_at: now,
        },
    )
    .join("/");
    (
        format!(
            "{{host}}, {{title}}, {{kind}}, {{date}} and {{time}} are filled in; / starts a subfolder. Example: {example}"
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_scale_names_the_zoom_chords_when_bound() {
        assert_eq!(
            interface_scale_description(Some("Ctrl+Shift+="), Some("Ctrl+Shift+-")),
            "Scales the whole window. Terminal text has its own zoom (Ctrl+Shift+= / Ctrl+Shift+- or Ctrl+scroll), which this does not touch."
        );
        assert!(interface_scale_description(None, Some("x")).contains("zoom (Ctrl+scroll), which"));
        let options = interface_zoom_options();
        assert_eq!(options[2], ("1".into(), "100% (default)".into()));
        assert_eq!(options[4], ("1.25".into(), "125%".into()));
    }

    #[test]
    fn keepalive_keeps_a_custom_value_visible() {
        assert_eq!(keepalive_options(30.).len(), 5);
        let custom = keepalive_options(45.);
        assert_eq!(custom.last().unwrap().1, "Every 45 seconds (custom)");
    }

    #[test]
    fn font_family_reports_missing_fonts() {
        assert_eq!(
            font_family_description(false, Some(false), "  ", true),
            (
                "This font is not installed; JetBrains Mono is used instead.".into(),
                true
            )
        );
        assert!(
            font_family_description(true, Some(false), "x", true)
                .0
                .starts_with("Reading")
        );
        assert!(!font_family_description(false, None, "x", false).1);
    }

    fn vault(
        configured: bool,
        policy: Option<PasswordVaultUnlockPolicy>,
        locked: bool,
    ) -> PasswordVaultStatus {
        PasswordVaultStatus {
            configured,
            unlock_policy: policy,
            locked,
            os_key_store_available: true,
            credential_count: 0,
            credentials: Vec::new(),
            secrets: Vec::new(),
        }
    }

    #[test]
    fn vault_status_follows_policy_and_lock() {
        use PasswordVaultUnlockPolicy::*;
        assert_eq!(vault_status(&vault(false, None, false)).0, Status::Info);
        assert_eq!(
            vault_status(&vault(true, Some(Credential), true)).0,
            Status::Info
        );
        assert_eq!(
            vault_status(&vault(true, Some(Never), false)).1,
            "OS credential-store access is ready. Saved passwords are used without a master-password prompt."
        );
        assert_eq!(
            vault_status(&vault(true, Some(Startup), false)).1,
            "Unlocked for this app session."
        );
        assert_eq!(
            vault_status(&vault(true, Some(Never), true)).0,
            Status::Error
        );
        assert_eq!(
            vault_status(&vault(true, Some(Startup), true)).0,
            Status::Warning
        );
    }

    #[test]
    fn update_reasons_and_release_notes() {
        assert_eq!(
            update_reason_label(Some("manifest-404")),
            "The update manifest returned 404."
        );
        assert_eq!(
            update_reason_label(None),
            "The update check could not be completed."
        );
        assert_eq!(
            release_notes_url(Some("0.8.1")),
            "https://flosch62.github.io/muxus/release-notes/0.8/"
        );
        assert_eq!(
            release_notes_url(Some("dev")),
            "https://flosch62.github.io/muxus/release-notes/"
        );
        assert_eq!(
            release_notes_url(Some("1.12-beta")),
            "https://flosch62.github.io/muxus/release-notes/1.12/"
        );
    }

    #[test]
    fn profile_names_and_counts() {
        let profile = |name: &str| KeywordHighlightProfile {
            id: name.into(),
            name: name.into(),
            rules: Vec::new(),
        };
        assert_eq!(unused_profile_name(&[]), "New profile");
        assert_eq!(
            unused_profile_name(&[profile("New Profile"), profile("new profile 2")]),
            "New profile 3"
        );
        assert_eq!(highlight_profile_option(&profile("SR OS")), "SR OS (0)");
        assert_eq!(counted_title("Secrets", 2), "Secrets \u{b7} 2");
        assert_eq!(counted_title("Secrets", 0), "Secrets");
        assert_eq!(all_shortcuts_label(1), "All shortcuts \u{b7} 1 customized");
    }

    #[test]
    fn logging_texts() {
        assert_eq!(
            inherited_logging_caption(true, true, true),
            "Inherited: logging enabled, log files on."
        );
        assert_eq!(
            inherited_logging_caption(true, false, false),
            "Inherited: logging disabled."
        );
        let now = chrono::NaiveDate::from_ymd_opt(2026, 3, 4)
            .unwrap()
            .and_hms_opt(5, 6, 7)
            .unwrap();
        let (text, error) = log_file_pattern_helper("{host}_{date}_{time}.log", now);
        assert!(!error);
        assert!(
            text.ends_with("Example: router1_2026-03-04_05-06-07.log"),
            "{text}"
        );
        assert!(log_file_pattern_helper("{nope", now).1);
    }

    #[test]
    fn x11_texts() {
        let status = X11Status {
            enabled: true,
            forward_by_default: true,
            clipboard: false,
            source: X11Source::Display,
            display: Some(":0".into()),
            defaults: muxus_core::api::X11Defaults {
                enabled: true,
                forward_by_default: false,
            },
        };
        assert_eq!(
            x11_server_status(&status, Some("linux")).1,
            "Your X server on display :0."
        );
        assert!(x11_default_description(X11Source::Bundled).contains("built-in X server"));
    }
}
