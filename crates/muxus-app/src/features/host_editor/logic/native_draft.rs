//! Telnet and serial hosts (`client/src/components/host-editor/native-draft.ts`).

use muxus_core::api::{
    HostKeywordHighlightConfig, OpenSshMetadataPatch, SavedHostProfile, SavedHostProfileInput,
};
use muxus_core::js::{Patch, js_trim};
use muxus_core::protocol::{
    DEFAULT_BREAK_DURATION_MS, MAX_BREAK_DURATION_MS, SavedHostSessionProfile, SerialFlowControl,
    SerialParity, SerialProfile, StopBits, TelnetProfile,
};
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, login_sequence_draft, login_sequence_from_draft, login_sequence_problem,
};
use muxus_term::keywords::keyword_highlight_rules_problem;

use super::js::{js_integer, js_number};
use super::session_logging::{HostSessionLoggingDraft, blank_host_session_logging_draft};

/// Which of the two native kinds a draft is saved as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeKind {
    Telnet,
    Serial,
}

/// Form state for Muxus-owned Telnet/serial hosts. One draft carries both
/// kinds so switching the connection type while creating never loses input.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeHostDraft {
    pub name: String,
    /// Muxus sidebar group, applied as a metadata patch after the host saves.
    pub group: String,
    /// Muxus row color, applied with the same metadata patch.
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
    pub host: String,
    pub port: String,
    pub path: String,
    pub baud_rate: String,
    pub data_bits: u8,
    pub stop_bits: StopBits,
    pub parity: SerialParity,
    pub flow_control: SerialFlowControl,
    /// Milliseconds Send BREAK holds a serial line.
    pub break_duration_ms: String,
    pub keyword_highlights: HostKeywordHighlightConfig,
    pub session_logging: HostSessionLoggingDraft,
    pub login_sequence: LoginSequenceDraft,
}

/// The highlighting a host without its own has: global rules only.
pub fn default_highlights() -> HostKeywordHighlightConfig {
    HostKeywordHighlightConfig {
        inherit_global: true,
        profile_id: None,
        rules: Vec::new(),
    }
}

/// `null` when the highlighting is the default, as the editors store it.
pub fn highlights_patch(
    highlights: &HostKeywordHighlightConfig,
) -> Patch<HostKeywordHighlightConfig> {
    if highlights.inherit_global && highlights.profile_id.is_none() && highlights.rules.is_empty() {
        Patch::Clear
    } else {
        Patch::Set(highlights.clone())
    }
}

/// `value ?? null` as a patch.
pub fn optional_patch<T: Clone>(value: &Option<T>) -> Patch<T> {
    match value {
        Some(value) => Patch::Set(value.clone()),
        None => Patch::Clear,
    }
}

/// `text.trim() || null` as a patch.
pub fn text_patch(text: &str) -> Patch<String> {
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        Patch::Clear
    } else {
        Patch::Set(trimmed.to_owned())
    }
}

/// `blankNativeDraft(prefillTarget, group)`.
pub fn blank_native_draft(prefill_target: &str, group: &str) -> NativeHostDraft {
    let target = parse_host_target(prefill_target);
    NativeHostDraft {
        name: String::new(),
        group: group.to_owned(),
        color: None,
        terminal_scheme: None,
        terminal_font_color: None,
        terminal_background_color: None,
        command_button_group: None,
        paste_line_delay_ms: None,
        paste_char_delay_ms: None,
        host: target.host,
        port: target.port.unwrap_or_else(|| "23".into()),
        path: String::new(),
        baud_rate: "115200".into(),
        data_bits: 8,
        stop_bits: StopBits::One,
        parity: SerialParity::None,
        flow_control: SerialFlowControl::None,
        break_duration_ms: DEFAULT_BREAK_DURATION_MS.to_string(),
        keyword_highlights: default_highlights(),
        session_logging: blank_host_session_logging_draft(),
        login_sequence: login_sequence_draft(None),
    }
}

impl Default for NativeHostDraft {
    fn default() -> Self {
        blank_native_draft("", "")
    }
}

/// `nativeDraftFromProfile(saved, duplicate)`; `None` for other kinds (the
/// TypeScript throws).
pub fn native_draft_from_profile(
    saved: &SavedHostProfile,
    duplicate: bool,
) -> Option<NativeHostDraft> {
    let mut draft = blank_native_draft("", "");
    let metadata = &saved.metadata;
    draft.name = if duplicate {
        format!("{} copy", saved.name)
    } else {
        saved.name.clone()
    };
    draft.group = metadata.group.clone().unwrap_or_default();
    draft.color = metadata.color.clone();
    draft.terminal_scheme = metadata.terminal_scheme.clone();
    draft.terminal_font_color = metadata.terminal_font_color.clone();
    draft.terminal_background_color = metadata.terminal_background_color.clone();
    draft.command_button_group = metadata.command_button_group.clone();
    draft.paste_line_delay_ms = metadata.paste_line_delay_ms;
    draft.paste_char_delay_ms = metadata.paste_char_delay_ms;
    if let Some(highlights) = &metadata.keyword_highlights {
        draft.keyword_highlights = highlights.clone();
    }
    draft.login_sequence = login_sequence_draft(metadata.login_sequence.as_ref());
    match &saved.profile {
        SavedHostSessionProfile::Telnet(profile) => {
            draft.host = profile.host.clone();
            draft.port = profile.port.to_string();
        }
        SavedHostSessionProfile::Serial(profile) => {
            draft.path = profile.path.clone();
            draft.baud_rate = profile.baud_rate.to_string();
            draft.data_bits = profile.data_bits;
            draft.stop_bits = profile.stop_bits;
            draft.parity = profile.parity;
            draft.flow_control = profile.flow_control;
            draft.break_duration_ms = profile
                .break_duration_ms
                .unwrap_or(DEFAULT_BREAK_DURATION_MS)
                .to_string();
        }
        _ => return None,
    }
    Some(draft)
}

/// `nativeDraftProblem(draft, kind)`.
pub fn native_draft_problem(draft: &NativeHostDraft, kind: NativeKind) -> Option<String> {
    if js_trim(&draft.name).is_empty() {
        return Some("A name is required — it labels this host in Muxus.".into());
    }
    match kind {
        NativeKind::Telnet => {
            if js_trim(&draft.host).is_empty() {
                return Some("Enter a hostname or IP address.".into());
            }
            if !js_integer(&draft.port).is_some_and(|port| (1. ..=65_535.).contains(&port)) {
                return Some("Port must be between 1 and 65535.".into());
            }
        }
        NativeKind::Serial => {
            if js_trim(&draft.path).is_empty() {
                return Some("Choose or enter a serial port.".into());
            }
            if !js_integer(&draft.baud_rate).is_some_and(|baud| (1. ..=12_000_000.).contains(&baud))
            {
                return Some("Baud rate must be between 1 and 12000000.".into());
            }
            let max = f64::from(MAX_BREAK_DURATION_MS);
            if !js_integer(&draft.break_duration_ms).is_some_and(|ms| (1. ..=max).contains(&ms)) {
                return Some(format!(
                    "Break duration must be between 1 and {MAX_BREAK_DURATION_MS} ms."
                ));
            }
        }
    }
    login_sequence_problem(&draft.login_sequence)
        .or_else(|| keyword_highlight_rules_problem(&draft.keyword_highlights.rules))
}

/// `nativeDraftToInput(draft, kind, existingId)`.
pub fn native_draft_to_input(
    draft: &NativeHostDraft,
    kind: NativeKind,
    existing_id: Option<&str>,
) -> SavedHostProfileInput {
    let profile = match kind {
        NativeKind::Telnet => SavedHostSessionProfile::Telnet(TelnetProfile {
            profile_id: None,
            host: js_trim(&draft.host).to_owned(),
            port: js_number(&draft.port) as u16,
        }),
        NativeKind::Serial => SavedHostSessionProfile::Serial(SerialProfile {
            profile_id: None,
            path: js_trim(&draft.path).to_owned(),
            baud_rate: js_number(&draft.baud_rate) as u32,
            data_bits: draft.data_bits,
            stop_bits: draft.stop_bits,
            parity: draft.parity,
            flow_control: draft.flow_control,
            break_duration_ms: Some(js_number(&draft.break_duration_ms) as u32),
        }),
    };
    SavedHostProfileInput {
        id: existing_id.map(str::to_owned),
        name: js_trim(&draft.name).to_owned(),
        profile,
    }
}

/// Muxus-only metadata written right after the host itself saves.
pub fn native_draft_metadata_patch(draft: &NativeHostDraft) -> OpenSshMetadataPatch {
    OpenSshMetadataPatch {
        group: text_patch(&draft.group),
        color: optional_patch(&draft.color),
        terminal_scheme: optional_patch(&draft.terminal_scheme),
        terminal_font_color: optional_patch(&draft.terminal_font_color),
        terminal_background_color: optional_patch(&draft.terminal_background_color),
        command_button_group: optional_patch(&draft.command_button_group),
        paste_line_delay_ms: optional_patch(&draft.paste_line_delay_ms),
        paste_char_delay_ms: optional_patch(&draft.paste_char_delay_ms),
        keyword_highlights: highlights_patch(&draft.keyword_highlights),
        login_sequence: optional_patch(&login_sequence_from_draft(&draft.login_sequence)),
        ..OpenSshMetadataPatch::default()
    }
}

/// A quick-connect target split into form fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostTarget {
    pub host: String,
    pub port: Option<String>,
    pub user: Option<String>,
}

/// Split a quick-connect target like user@host:port into form fields.
pub fn parse_host_target(target: &str) -> HostTarget {
    let trimmed = js_trim(target);
    // /^([^@\s]+)@(.+)$/
    let (user, stripped) = match trimmed.split_once('@') {
        Some((user, rest))
            if !user.is_empty()
                && !user.chars().any(muxus_core::js::is_js_whitespace)
                && !rest.is_empty() =>
        {
            (Some(user.to_owned()), rest)
        }
        _ => (None, trimmed),
    };
    // /^(.+):(\d{1,5})$/ — greedy, so the last colon splits.
    if let Some((host, port)) = stripped.rsplit_once(':')
        && !host.is_empty()
        && (1..=5).contains(&port.len())
        && port.chars().all(|c| c.is_ascii_digit())
    {
        return HostTarget {
            host: host.to_owned(),
            port: Some(port.to_owned()),
            user,
        };
    }
    HostTarget {
        host: stripped.to_owned(),
        port: None,
        user,
    }
}

/// `COMMON_BAUD_RATES`: the baud rate field's suggestions.
pub const COMMON_BAUD_RATES: [u32; 12] = [
    300, 1_200, 2_400, 4_800, 9_600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600,
];

/// The serial port field's placeholder for this platform.
pub fn platform_path_example() -> &'static str {
    if cfg!(windows) {
        "COM3"
    } else if cfg!(target_os = "macos") {
        "/dev/tty.usbserial-…"
    } else {
        "/dev/ttyUSB0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::{
        KeywordHighlightRule, LoginSecretStep, LoginSendStep, LoginSequence, LoginSequenceStep,
        LoginWaitStep, OpenSshProfileMetadata, SavedHostKind,
    };
    use muxus_session::login_sequence::editor::LoginSequenceMode;

    fn serial_host() -> SavedHostProfile {
        SavedHostProfile {
            id: "serial-console".into(),
            kind: SavedHostKind::Serial,
            name: "Rack console".into(),
            profile: SavedHostSessionProfile::Serial(SerialProfile {
                profile_id: Some("serial-console".into()),
                path: "/dev/ttyUSB0".into(),
                baud_rate: 9_600,
                data_bits: 7,
                stop_bits: StopBits::Two,
                parity: SerialParity::Even,
                flow_control: SerialFlowControl::Hardware,
                break_duration_ms: Some(600),
            }),
            metadata: OpenSshProfileMetadata {
                profile_id: "serial-console".into(),
                group: Some("Lab".into()),
                terminal_scheme: Some("gruvbox-dark".into()),
                terminal_font_color: Some("#ebdbb2".into()),
                terminal_background_color: Some("#282828".into()),
                command_button_group: Some("command-group-console".into()),
                paste_line_delay_ms: Some(300),
                keyword_highlights: Some(HostKeywordHighlightConfig {
                    inherit_global: false,
                    profile_id: Some("nokia-sros".into()),
                    rules: vec![KeywordHighlightRule {
                        id: "r1".into(),
                        name: None,
                        keyword: "ERROR".into(),
                        foreground: "#ff0000".into(),
                        background: None,
                        case_sensitive: false,
                        whole_word: false,
                        regex: None,
                    }],
                }),
                ..OpenSshProfileMetadata::default()
            },
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn named(name: &str, host: &str) -> NativeHostDraft {
        NativeHostDraft {
            name: name.into(),
            host: host.into(),
            ..blank_native_draft("", "")
        }
    }

    #[test]
    fn starts_from_telnet_and_serial_defaults() {
        let draft = blank_native_draft("", "");
        assert_eq!(draft.port, "23");
        assert_eq!(draft.baud_rate, "115200");
        assert_eq!(draft.data_bits, 8);
        assert_eq!(draft.break_duration_ms, "250");
        assert_eq!(draft.keyword_highlights, default_highlights());
    }

    #[test]
    fn seeds_the_telnet_host_from_a_quick_connect_target() {
        let draft = blank_native_draft("admin@10.0.0.5:2323", "");
        assert_eq!(
            (draft.host.as_str(), draft.port.as_str()),
            ("10.0.0.5", "2323")
        );
        let draft = blank_native_draft("switch.example.test", "");
        assert_eq!(
            (draft.host.as_str(), draft.port.as_str()),
            ("switch.example.test", "23")
        );
    }

    #[test]
    fn prefills_the_selected_folder_for_telnet_and_serial_hosts() {
        assert_eq!(blank_native_draft("", "Lab/Consoles").group, "Lab/Consoles");
    }

    #[test]
    fn restores_every_serial_field_plus_muxus_metadata() {
        let draft = native_draft_from_profile(&serial_host(), false).unwrap();
        assert_eq!(draft.name, "Rack console");
        assert_eq!(draft.group, "Lab");
        assert_eq!(draft.terminal_scheme.as_deref(), Some("gruvbox-dark"));
        assert_eq!(draft.terminal_font_color.as_deref(), Some("#ebdbb2"));
        assert_eq!(draft.terminal_background_color.as_deref(), Some("#282828"));
        assert_eq!(draft.path, "/dev/ttyUSB0");
        assert_eq!(draft.baud_rate, "9600");
        assert_eq!(draft.data_bits, 7);
        assert_eq!(draft.stop_bits, StopBits::Two);
        assert_eq!(draft.parity, SerialParity::Even);
        assert_eq!(draft.flow_control, SerialFlowControl::Hardware);
        assert_eq!(draft.break_duration_ms, "600");
        assert_eq!(
            draft.keyword_highlights.profile_id.as_deref(),
            Some("nokia-sros")
        );
        assert_eq!(draft.keyword_highlights.rules[0].keyword, "ERROR");
    }

    #[test]
    fn renames_duplicates() {
        assert_eq!(
            native_draft_from_profile(&serial_host(), true)
                .unwrap()
                .name,
            "Rack console copy"
        );
    }

    #[test]
    fn gives_serial_hosts_saved_before_break_durations_the_250_ms_default() {
        let mut host = serial_host();
        if let SavedHostSessionProfile::Serial(profile) = &mut host.profile {
            profile.break_duration_ms = None;
        }
        assert_eq!(
            native_draft_from_profile(&host, false)
                .unwrap()
                .break_duration_ms,
            "250"
        );
    }

    #[test]
    fn validates_the_fields_of_the_active_kind_only() {
        let mut draft = blank_native_draft("", "");
        assert!(
            native_draft_problem(&draft, NativeKind::Telnet)
                .unwrap()
                .to_lowercase()
                .contains("name")
        );
        draft.name = "Router".into();
        assert!(
            native_draft_problem(&draft, NativeKind::Telnet)
                .unwrap()
                .contains("hostname or IP")
        );
        draft.host = "router.example.test".into();
        assert_eq!(native_draft_problem(&draft, NativeKind::Telnet), None);
        draft.port = "70000".into();
        assert!(
            native_draft_problem(&draft, NativeKind::Telnet)
                .unwrap()
                .contains("Port")
        );
        // The empty serial path never blocks saving a telnet host.
        draft.port = "23".into();
        assert!(
            native_draft_problem(&draft, NativeKind::Serial)
                .unwrap()
                .contains("serial port")
        );
    }

    #[test]
    fn bounds_the_serial_break_duration() {
        let draft = NativeHostDraft {
            name: "Console".into(),
            path: "/dev/ttyUSB0".into(),
            ..blank_native_draft("", "")
        };
        assert_eq!(native_draft_problem(&draft, NativeKind::Serial), None);
        for value in ["0", "10001", "2.5", ""] {
            let draft = NativeHostDraft {
                break_duration_ms: value.into(),
                ..draft.clone()
            };
            assert!(
                native_draft_problem(&draft, NativeKind::Serial)
                    .unwrap()
                    .contains("Break duration"),
                "{value}"
            );
        }
        let max = NativeHostDraft {
            break_duration_ms: "10000".into(),
            ..draft.clone()
        };
        assert_eq!(native_draft_problem(&max, NativeKind::Serial), None);
        // Telnet hosts have no break duration to get wrong.
        let telnet = NativeHostDraft {
            host: "router".into(),
            break_duration_ms: "0".into(),
            ..draft
        };
        assert_eq!(native_draft_problem(&telnet, NativeKind::Telnet), None);
    }

    #[test]
    fn rejects_empty_keywords_and_invalid_regex_highlighting_rules() {
        let rule = KeywordHighlightRule {
            id: "rule".into(),
            name: None,
            keyword: "(up".into(),
            foreground: "#ffffff".into(),
            background: None,
            case_sensitive: false,
            whole_word: false,
            regex: None,
        };
        let with_rules = |rules: Vec<KeywordHighlightRule>| NativeHostDraft {
            keyword_highlights: HostKeywordHighlightConfig {
                inherit_global: true,
                profile_id: None,
                rules,
            },
            ..named("Router", "router.example.test")
        };
        assert_eq!(
            native_draft_problem(&with_rules(vec![rule.clone()]), NativeKind::Telnet),
            None
        );
        let regex = KeywordHighlightRule {
            regex: Some(true),
            ..rule.clone()
        };
        assert!(
            native_draft_problem(&with_rules(vec![regex]), NativeKind::Telnet)
                .unwrap()
                .contains("Invalid regular expression")
        );
        let empty = KeywordHighlightRule {
            keyword: String::new(),
            ..rule
        };
        assert!(
            native_draft_problem(&with_rules(vec![empty]), NativeKind::Telnet)
                .unwrap()
                .contains("needs a keyword")
        );
    }

    #[test]
    fn maps_only_the_active_kind_into_the_save_payload() {
        let mut draft = blank_native_draft("", "");
        draft.name = " Router ".into();
        draft.host = " router.example.test ".into();
        draft.port = "2323".into();
        assert_eq!(
            native_draft_to_input(&draft, NativeKind::Telnet, Some("existing-id")),
            SavedHostProfileInput {
                id: Some("existing-id".into()),
                name: "Router".into(),
                profile: SavedHostSessionProfile::Telnet(TelnetProfile {
                    profile_id: None,
                    host: "router.example.test".into(),
                    port: 2323,
                }),
            }
        );
        assert_eq!(
            serde_json::to_value(native_draft_to_input(
                &draft,
                NativeKind::Telnet,
                Some("existing-id")
            ))
            .unwrap(),
            serde_json::json!({
                "id": "existing-id",
                "name": "Router",
                "profile": { "kind": "telnet", "host": "router.example.test", "port": 2323 },
            })
        );
    }

    #[test]
    fn saves_the_serial_line_settings_with_the_break_duration() {
        let mut draft = native_draft_from_profile(&serial_host(), false).unwrap();
        draft.break_duration_ms = "1000".into();
        let input = native_draft_to_input(&draft, NativeKind::Serial, Some("serial-console"));
        assert_eq!(
            serde_json::to_value(&input.profile).unwrap(),
            serde_json::json!({
                "kind": "serial",
                "path": "/dev/ttyUSB0",
                "baudRate": 9600,
                "dataBits": 7,
                "stopBits": 2,
                "parity": "even",
                "flowControl": "hardware",
                "breakDurationMs": 1000,
            })
        );
    }

    #[test]
    fn clears_untouched_metadata_and_keeps_configured_values() {
        assert_eq!(
            serde_json::to_value(native_draft_metadata_patch(&blank_native_draft("", ""))).unwrap(),
            serde_json::json!({
                "group": null,
                "color": null,
                "terminalScheme": null,
                "terminalFontColor": null,
                "terminalBackgroundColor": null,
                "keywordHighlights": null,
                "commandButtonGroup": null,
                "pasteLineDelayMs": null,
                "pasteCharDelayMs": null,
                "loginSequence": null,
            })
        );
        let host = serial_host();
        let draft = native_draft_from_profile(&host, false).unwrap();
        assert_eq!(
            serde_json::to_value(native_draft_metadata_patch(&draft)).unwrap(),
            serde_json::json!({
                "group": "Lab",
                "color": null,
                "terminalScheme": "gruvbox-dark",
                "terminalFontColor": "#ebdbb2",
                "terminalBackgroundColor": "#282828",
                "keywordHighlights": serde_json::to_value(&host.metadata.keyword_highlights).unwrap(),
                "commandButtonGroup": "command-group-console",
                "pasteLineDelayMs": 300,
                "pasteCharDelayMs": null,
                "loginSequence": null,
            })
        );
    }

    #[test]
    fn round_trips_the_login_sequence_inherited_none_or_own_steps() {
        let steps = vec![
            LoginSequenceStep::Send(LoginSendStep {
                id: "s1".into(),
                text: String::new(),
                enter: true,
            }),
            LoginSequenceStep::Wait(LoginWaitStep {
                id: "s2".into(),
                pattern: "login:".into(),
                regex: None,
                timeout_seconds: 20,
            }),
            LoginSequenceStep::Secret(LoginSecretStep {
                id: "s3".into(),
                secret_id: "vault-1".into(),
                enter: true,
            }),
        ];
        let with_sequence = |sequence: LoginSequence| {
            let mut host = serial_host();
            host.metadata.login_sequence = Some(sequence);
            native_draft_from_profile(&host, false).unwrap()
        };
        let own = with_sequence(LoginSequence {
            steps: steps.clone(),
        });
        assert_eq!(own.login_sequence.mode, LoginSequenceMode::Custom);
        assert_eq!(own.login_sequence.steps, steps);
        assert_eq!(
            native_draft_metadata_patch(&own).login_sequence,
            Patch::Set(LoginSequence {
                steps: steps.clone()
            })
        );

        let none = with_sequence(LoginSequence { steps: Vec::new() });
        assert_eq!(none.login_sequence.mode, LoginSequenceMode::None);
        assert_eq!(
            native_draft_metadata_patch(&none).login_sequence,
            Patch::Set(LoginSequence { steps: Vec::new() })
        );
        // Steps hidden behind another mode are kept for switching back, but not saved.
        let hidden = NativeHostDraft {
            login_sequence: LoginSequenceDraft {
                mode: LoginSequenceMode::Inherit,
                steps,
            },
            ..own
        };
        assert_eq!(
            native_draft_metadata_patch(&hidden).login_sequence,
            Patch::Clear
        );
    }

    #[test]
    fn blocks_saving_an_unfinished_login_step() {
        let draft = NativeHostDraft {
            login_sequence: LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps: vec![LoginSequenceStep::Wait(LoginWaitStep {
                    id: "w".into(),
                    pattern: "[".into(),
                    regex: Some(true),
                    timeout_seconds: 10,
                })],
            },
            ..named("Router", "router.example.test")
        };
        let problem = native_draft_problem(&draft, NativeKind::Telnet).unwrap();
        assert!(problem.starts_with("Login sequence step 1: "), "{problem}");
        assert!(problem.contains("Invalid regular expression"), "{problem}");
        let empty = NativeHostDraft {
            login_sequence: LoginSequenceDraft {
                mode: LoginSequenceMode::Custom,
                steps: Vec::new(),
            },
            ..draft
        };
        assert_eq!(
            native_draft_problem(&empty, NativeKind::Telnet).as_deref(),
            Some("Add a login step, or choose no login sequence.")
        );
    }

    #[test]
    fn carries_the_row_color_chosen_in_the_editor() {
        let draft = NativeHostDraft {
            color: Some("#4285f4".into()),
            ..blank_native_draft("", "")
        };
        assert_eq!(
            native_draft_metadata_patch(&draft).color,
            Patch::Set("#4285f4".into())
        );
        let mut host = serial_host();
        host.metadata.color = Some("#ef5350".into());
        assert_eq!(
            native_draft_from_profile(&host, false)
                .unwrap()
                .color
                .as_deref(),
            Some("#ef5350")
        );
    }

    #[test]
    fn splits_quick_connect_targets() {
        assert_eq!(
            parse_host_target(" ops@edge01:2222 "),
            HostTarget {
                host: "edge01".into(),
                port: Some("2222".into()),
                user: Some("ops".into())
            }
        );
        assert_eq!(parse_host_target("a:b:22").host, "a:b");
        assert_eq!(parse_host_target("host:123456").port, None);
        assert_eq!(parse_host_target("@host").user, None);
    }
}
