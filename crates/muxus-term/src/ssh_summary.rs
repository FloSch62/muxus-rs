//! The connect summary printed above an SSH session's first output (port of
//! `client/src/terminal/ssh-session-summary.ts`).

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const MUTED: &str = "\x1b[90m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";

/// More forwards than this are counted instead of listed.
const LISTED_FORWARDS: usize = 4;

pub use muxus_core::api::{ConfigForward, ForwardType};
pub use muxus_core::protocol::{
    AgentForwardingState, CompressionState, SshSessionSummary, X11State,
};

fn on() -> String {
    format!("{GREEN}✔{RESET}")
}

fn off(reason: &str) -> String {
    format!("{RED}✘{RESET}  {MUTED}({reason}){RESET}")
}

fn auth_method_label(method: &str) -> &str {
    match method {
        "none" => "none",
        "agent" => "public key (SSH agent)",
        "publickey" => "public key",
        "password" => "password",
        "keyboard-interactive" => "keyboard-interactive",
        "hostbased" => "host-based",
        other => other,
    }
}

/// The login methods, naming the key's signature algorithm when it is known.
fn authentication(summary: &SshSessionSummary) -> String {
    summary
        .auth_methods
        .iter()
        .map(|method| {
            let label = auth_method_label(method);
            let Some(algorithm) = summary
                .auth_key_algorithm
                .as_deref()
                .filter(|a| !a.is_empty())
            else {
                return label.to_string();
            };
            if method != "agent" && method != "publickey" {
                return label.to_string();
            }
            // FIDO2 keys (sk-ssh-ed25519@openssh.com, sk-ecdsa-…) are hardware keys.
            let kind = if algorithm.starts_with("sk-") {
                label.replacen("public key", "security key", 1)
            } else {
                label.to_string()
            };
            format!("{kind}  {MUTED}({algorithm}){RESET}")
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn host_port(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn route(summary: &SshSessionSummary) -> String {
    let mut hops = summary.jump_hosts.clone();
    if summary.proxy_command {
        match hops.first_mut() {
            Some(first) => *first = format!("{first} (ProxyCommand)"),
            None => hops.push("ProxyCommand".into()),
        }
    }
    let path = if hops.is_empty() {
        "direct".to_string()
    } else {
        format!("via {}", hops.join(" → "))
    };
    if summary.shared {
        format!("{path}  {MUTED}(reusing an open connection){RESET}")
    } else {
        path
    }
}

fn forward_spec(forward: &ConfigForward) -> String {
    let flag = match forward.kind {
        ForwardType::Local => "-L",
        ForwardType::Remote => "-R",
        ForwardType::Dynamic => "-D",
    };
    match (
        forward.kind,
        forward.target_host.as_deref().filter(|h| !h.is_empty()),
        forward.target_port,
    ) {
        (ForwardType::Dynamic, _, _) | (_, None, _) | (_, _, None) => {
            format!("{flag} {}", forward.bind_port)
        }
        (_, Some(host), Some(port)) => {
            let target = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_string()
            };
            format!("{flag} {}:{target}:{port}", forward.bind_port)
        }
    }
}

fn forwards(list: &[ConfigForward]) -> String {
    let listed = list
        .iter()
        .take(LISTED_FORWARDS)
        .map(forward_spec)
        .collect::<Vec<_>>()
        .join(", ");
    if list.len() > LISTED_FORWARDS {
        format!(
            "{listed}  {MUTED}(+{} more){RESET}",
            list.len() - LISTED_FORWARDS
        )
    } else {
        listed
    }
}

/// The connect summary printed above an SSH session's first output: how the host was reached,
/// what the connection negotiated and which optional features are active. Ends with a line break.
pub fn format_ssh_session_summary(summary: &SshSessionSummary) -> String {
    let mut rows: Vec<(&str, String)> = vec![("Route", route(summary))];
    if let Some(server) = summary.server_software.as_deref().filter(|s| !s.is_empty()) {
        rows.push(("Server", server.to_string()));
    }
    if !summary.auth_methods.is_empty() {
        rows.push(("Authentication", authentication(summary)));
    }
    if let Some(cipher) = summary.cipher.as_deref().filter(|c| !c.is_empty()) {
        let value = match summary.kex.as_deref().filter(|k| !k.is_empty()) {
            Some(kex) => format!("{cipher}  {MUTED}({kex}){RESET}"),
            None => cipher.to_string(),
        };
        rows.push(("Encryption", value));
    }
    rows.push((
        "Compression",
        match summary.compression {
            CompressionState::On => on(),
            CompressionState::Unsupported => off("not supported by the server"),
            CompressionState::Off => off("disabled"),
        },
    ));
    rows.push((
        "SFTP browser",
        if summary.sftp {
            on()
        } else {
            off("disabled for this host")
        },
    ));
    rows.push((
        "X11 forwarding",
        match summary.x11 {
            X11State::On => on(),
            X11State::Refused => off("refused by the server"),
            X11State::NoServer => off("no local X server"),
            X11State::Off => off("disabled"),
        },
    ));
    rows.push((
        "Agent forwarding",
        match summary.agent_forwarding {
            AgentForwardingState::On => on(),
            AgentForwardingState::NoAgent => off("no SSH agent available"),
            AgentForwardingState::Off => off("disabled"),
        },
    ));
    if !summary.forwards.is_empty() {
        rows.push(("Port forwards", forwards(&summary.forwards)));
    }

    let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let target = format!(
        "{}@{}",
        summary.user,
        host_port(&summary.host, summary.port)
    );
    let mut lines = vec![format!("{BOLD}➤ SSH session to {MAGENTA}{target}{RESET}")];
    lines.extend(
        rows.iter()
            .map(|(label, value)| format!("  {MUTED}•{RESET} {label:<width$} : {value}")),
    );
    lines.iter().map(|line| format!("{line}\r\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> SshSessionSummary {
        SshSessionSummary {
            user: "admin".into(),
            host: "100.124.182.28".into(),
            port: 22,
            jump_hosts: vec![],
            proxy_command: false,
            shared: false,
            server_software: Some("OpenSSH_9.6p1".into()),
            auth_methods: vec!["agent".into()],
            auth_key_algorithm: None,
            cipher: Some("chacha20-poly1305@openssh.com".into()),
            kex: Some("curve25519-sha256".into()),
            compression: CompressionState::Off,
            sftp: true,
            x11: X11State::Off,
            agent_forwarding: AgentForwardingState::Off,
            forwards: vec![],
        }
    }

    /// The summary as it reads on screen, one entry per line.
    fn lines(value: &SshSessionSummary) -> Vec<String> {
        let text = format_ssh_session_summary(value);
        assert!(text.ends_with("\r\n"));
        let plain = regex::Regex::new("\x1b\\[[0-9;]*m")
            .unwrap()
            .replace_all(&text, "")
            .into_owned();
        let mut lines: Vec<String> = plain.split("\r\n").map(String::from).collect();
        lines.pop();
        lines
    }

    #[test]
    fn lists_the_route_server_login_encryption_and_feature_state() {
        assert_eq!(
            lines(&summary()),
            [
                "➤ SSH session to admin@100.124.182.28",
                "  • Route            : direct",
                "  • Server           : OpenSSH_9.6p1",
                "  • Authentication   : public key (SSH agent)",
                "  • Encryption       : chacha20-poly1305@openssh.com  (curve25519-sha256)",
                "  • Compression      : ✘  (disabled)",
                "  • SFTP browser     : ✔",
                "  • X11 forwarding   : ✘  (disabled)",
                "  • Agent forwarding : ✘  (disabled)",
            ]
        );
    }

    #[test]
    fn marks_active_features_and_colors_the_marks() {
        let active = SshSessionSummary {
            compression: CompressionState::On,
            x11: X11State::On,
            agent_forwarding: AgentForwardingState::On,
            ..summary()
        };
        let text = lines(&active);
        for expected in [
            "  • Compression      : ✔",
            "  • X11 forwarding   : ✔",
            "  • Agent forwarding : ✔",
        ] {
            assert!(text.iter().any(|line| line == expected), "{expected}");
        }
        assert!(format_ssh_session_summary(&active).contains("\x1b[32m✔\x1b[0m"));
        assert!(format_ssh_session_summary(&summary()).contains("\x1b[31m✘\x1b[0m"));
    }

    #[test]
    fn explains_why_a_requested_feature_is_off() {
        let text = lines(&SshSessionSummary {
            compression: CompressionState::Unsupported,
            sftp: false,
            x11: X11State::Refused,
            agent_forwarding: AgentForwardingState::NoAgent,
            ..summary()
        });
        for expected in [
            "  • Compression      : ✘  (not supported by the server)",
            "  • SFTP browser     : ✘  (disabled for this host)",
            "  • X11 forwarding   : ✘  (refused by the server)",
            "  • Agent forwarding : ✘  (no SSH agent available)",
        ] {
            assert!(text.iter().any(|line| line == expected), "{expected}");
        }
        assert!(
            lines(&SshSessionSummary {
                x11: X11State::NoServer,
                ..summary()
            })
            .contains(&"  • X11 forwarding   : ✘  (no local X server)".to_string())
        );
    }

    #[test]
    fn shows_jump_hosts_proxycommand_and_a_reused_connection_on_the_route() {
        let hops = |jumps: &[&str], proxy: bool, shared: bool| {
            lines(&SshSessionSummary {
                jump_hosts: jumps.iter().map(|s| s.to_string()).collect(),
                proxy_command: proxy,
                shared,
                ..summary()
            })[1]
                .clone()
        };
        assert_eq!(
            hops(&["bastion", "inner"], false, false),
            "  • Route            : via bastion → inner"
        );
        assert_eq!(
            hops(&["bastion"], true, false),
            "  • Route            : via bastion (ProxyCommand)"
        );
        assert_eq!(
            hops(&[], true, true),
            "  • Route            : via ProxyCommand  (reusing an open connection)"
        );
    }

    #[test]
    fn adds_a_non_default_port_to_the_target_bracketing_ipv6_addresses() {
        assert_eq!(
            lines(&SshSessionSummary {
                port: 2222,
                ..summary()
            })[0],
            "➤ SSH session to admin@100.124.182.28:2222"
        );
        assert_eq!(
            lines(&SshSessionSummary {
                host: "fd00::5".into(),
                port: 2222,
                ..summary()
            })[0],
            "➤ SSH session to admin@[fd00::5]:2222"
        );
        assert_eq!(
            lines(&SshSessionSummary {
                host: "fd00::5".into(),
                ..summary()
            })[0],
            "➤ SSH session to admin@fd00::5"
        );
    }

    #[test]
    fn joins_the_methods_of_a_multi_factor_login() {
        let text = lines(&SshSessionSummary {
            auth_methods: vec!["publickey".into(), "keyboard-interactive".into()],
            ..summary()
        });
        assert!(
            text.contains(&"  • Authentication   : public key + keyboard-interactive".to_string())
        );
    }

    #[test]
    fn names_the_key_algorithm_calling_a_fido2_key_a_security_key() {
        let with_key = |methods: &[&str], algorithm: &str| {
            lines(&SshSessionSummary {
                auth_methods: methods.iter().map(|m| m.to_string()).collect(),
                auth_key_algorithm: Some(algorithm.into()),
                ..summary()
            })
        };
        assert!(
            with_key(&["agent"], "sk-ssh-ed25519@openssh.com").contains(
                &"  • Authentication   : security key (SSH agent)  (sk-ssh-ed25519@openssh.com)"
                    .to_string()
            )
        );
        assert!(with_key(&["publickey", "keyboard-interactive"], "sk-ecdsa-sha2-nistp256@openssh.com").contains(
            &"  • Authentication   : security key  (sk-ecdsa-sha2-nistp256@openssh.com) + keyboard-interactive".to_string()
        ));
        assert!(with_key(&["agent"], "rsa-sha2-512").contains(
            &"  • Authentication   : public key (SSH agent)  (rsa-sha2-512)".to_string()
        ));
        assert!(
            with_key(&["password"], "ssh-ed25519")
                .contains(&"  • Authentication   : password".to_string())
        );
    }

    #[test]
    fn leaves_out_what_the_server_did_not_report() {
        let text = lines(&SshSessionSummary {
            server_software: None,
            auth_methods: vec![],
            cipher: None,
            kex: None,
            ..summary()
        });
        assert!(!text.iter().any(|line| line.contains("Server")
            || line.contains("Authentication")
            || line.contains("Encryption")));
    }

    #[test]
    fn lists_running_port_forwards_in_ssh_option_form_and_counts_the_rest() {
        let forwards = vec![
            ConfigForward {
                kind: ForwardType::Local,
                bind_port: 8080,
                target_host: Some("db".into()),
                target_port: Some(5432),
            },
            ConfigForward {
                kind: ForwardType::Remote,
                bind_port: 9000,
                target_host: Some("::1".into()),
                target_port: Some(3000),
            },
            ConfigForward {
                kind: ForwardType::Dynamic,
                bind_port: 1080,
                target_host: None,
                target_port: None,
            },
        ];
        assert_eq!(
            lines(&SshSessionSummary {
                forwards,
                ..summary()
            })
            .last()
            .unwrap(),
            "  • Port forwards    : -L 8080:db:5432, -R 9000:[::1]:3000, -D 1080"
        );
        let many = (0..6)
            .map(|index| ConfigForward {
                kind: ForwardType::Dynamic,
                bind_port: 1080 + index,
                target_host: None,
                target_port: None,
            })
            .collect();
        assert_eq!(
            lines(&SshSessionSummary {
                forwards: many,
                ..summary()
            })
            .last()
            .unwrap(),
            "  • Port forwards    : -D 1080, -D 1081, -D 1082, -D 1083  (+2 more)"
        );
    }

    #[test]
    fn reads_the_summary_the_session_sends() {
        let json = serde_json::to_value(summary()).unwrap();
        assert_eq!(json["agentForwarding"], "off");
        assert_eq!(json["jumpHosts"], serde_json::json!([]));
        let back: SshSessionSummary = serde_json::from_value(json).unwrap();
        assert_eq!(back, summary());
    }
}
