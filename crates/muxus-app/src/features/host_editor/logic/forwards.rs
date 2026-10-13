//! Port forward rules: their one-line description and the live diagram
//! (`client/src/components/ForwardRuleForm.tsx`, `ForwardDiagram.tsx`).

use muxus_core::api::{ConfigForward, ForwardType};
use muxus_core::js::js_trim;

use super::js::{port_ok, to_port};

/// `FORWARD_FLAG`.
pub fn forward_flag(kind: ForwardType) -> &'static str {
    match kind {
        ForwardType::Local => "-L",
        ForwardType::Remote => "-R",
        ForwardType::Dynamic => "-D",
    }
}

/// `describeForward`.
pub fn describe_forward(forward: &ConfigForward) -> String {
    let target_host = forward.target_host.as_deref().unwrap_or("undefined");
    let target_port = forward
        .target_port
        .map(|p| p.to_string())
        .unwrap_or_else(|| "undefined".into());
    match forward.kind {
        ForwardType::Dynamic => format!("SOCKS5 on 127.0.0.1:{}", forward.bind_port),
        ForwardType::Local => {
            format!(
                "127.0.0.1:{} → {target_host}:{target_port}",
                forward.bind_port
            )
        }
        ForwardType::Remote => format!(
            "remote :{} → {target_host}:{target_port}",
            forward.bind_port
        ),
    }
}

/// The three fields of a rule being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRuleDraft {
    pub kind: ForwardType,
    pub bind_port: String,
    pub target_host: String,
    pub target_port: String,
}

impl Default for ForwardRuleDraft {
    fn default() -> Self {
        Self {
            kind: ForwardType::Local,
            bind_port: String::new(),
            target_host: "localhost".into(),
            target_port: String::new(),
        }
    }
}

impl ForwardRuleDraft {
    /// Whether "Add" may submit the rule.
    pub fn valid(&self) -> bool {
        port_ok(&self.bind_port)
            && (self.kind == ForwardType::Dynamic
                || (!js_trim(&self.target_host).is_empty() && port_ok(&self.target_port)))
    }

    /// The rule to add, when valid.
    pub fn rule(&self) -> Option<ConfigForward> {
        if !self.valid() {
            return None;
        }
        let bind_port = to_port(&self.bind_port)?;
        Some(if self.kind == ForwardType::Dynamic {
            ConfigForward {
                kind: self.kind,
                bind_port,
                target_host: None,
                target_port: None,
            }
        } else {
            ConfigForward {
                kind: self.kind,
                bind_port,
                target_host: Some(js_trim(&self.target_host).to_owned()),
                target_port: to_port(&self.target_port),
            }
        })
    }

    /// After a submit the ports clear; the type and target host stay.
    pub fn after_submit(&mut self) {
        self.bind_port.clear();
        self.target_port.clear();
    }

    /// The listen-port field's label.
    pub fn bind_label(&self) -> &'static str {
        if self.kind == ForwardType::Remote {
            "Port on server"
        } else {
            "Local port"
        }
    }
}

/// Which icon a diagram node shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramIcon {
    Computer,
    Server,
    Globe,
}

impl DiagramIcon {
    pub fn icon_name(self) -> &'static str {
        match self {
            DiagramIcon::Computer => "ComputerOutlined",
            DiagramIcon::Server => "DnsOutlined",
            DiagramIcon::Globe => "LanguageOutlined",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramNode {
    pub icon: DiagramIcon,
    pub title: String,
    pub sub: String,
}

/// The "what does this tunnel actually do?" picture: three nodes, the first
/// leg SSH-encrypted, and a plain-language summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardDiagramModel {
    pub nodes: [DiagramNode; 3],
    pub summary: String,
}

pub fn forward_diagram(
    kind: ForwardType,
    bind_port: &str,
    target_host: &str,
    target_port: &str,
    server_label: &str,
) -> ForwardDiagramModel {
    let or_dots = |text: &str| {
        if text.is_empty() {
            "…".to_owned()
        } else {
            text.to_owned()
        }
    };
    let port = or_dots(bind_port);
    let target = format!("{}:{}", or_dots(target_host), or_dots(target_port));
    let server = if server_label.is_empty() {
        "SSH server".to_owned()
    } else {
        server_label.to_owned()
    };
    let node = |icon, title: &str, sub: String| DiagramNode {
        icon,
        title: title.to_owned(),
        sub,
    };
    match kind {
        ForwardType::Local => ForwardDiagramModel {
            nodes: [
                node(
                    DiagramIcon::Computer,
                    "This computer",
                    format!("listens on 127.0.0.1:{port}"),
                ),
                node(DiagramIcon::Server, &server, "SSH server".into()),
                node(DiagramIcon::Globe, &target, "destination".into()),
            ],
            summary: format!(
                "Apps connecting to 127.0.0.1:{port} on this computer reach {target} through {server}."
            ),
        },
        ForwardType::Remote => ForwardDiagramModel {
            nodes: [
                node(DiagramIcon::Server, &server, format!("listens on :{port}")),
                node(DiagramIcon::Computer, "This computer", "SSH client".into()),
                node(DiagramIcon::Globe, &target, "destination".into()),
            ],
            summary: format!(
                "Connections to port {port} on {server} reach {target} from this computer."
            ),
        },
        ForwardType::Dynamic => ForwardDiagramModel {
            nodes: [
                node(
                    DiagramIcon::Computer,
                    "This computer",
                    format!("SOCKS5 on 127.0.0.1:{port}"),
                ),
                node(DiagramIcon::Server, &server, "SSH server".into()),
                node(
                    DiagramIcon::Globe,
                    "Any destination",
                    "chosen per request".into(),
                ),
            ],
            summary: format!(
                "Point apps at the SOCKS5 proxy 127.0.0.1:{port} to tunnel any destination through {server}."
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_rules_like_the_forward_list() {
        let local = ConfigForward {
            kind: ForwardType::Local,
            bind_port: 15432,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(5432),
        };
        assert_eq!(describe_forward(&local), "127.0.0.1:15432 → 127.0.0.1:5432");
        assert_eq!(
            describe_forward(&ConfigForward {
                kind: ForwardType::Remote,
                ..local.clone()
            }),
            "remote :15432 → 127.0.0.1:5432"
        );
        assert_eq!(
            describe_forward(&ConfigForward {
                kind: ForwardType::Dynamic,
                bind_port: 1080,
                target_host: None,
                target_port: None
            }),
            "SOCKS5 on 127.0.0.1:1080"
        );
        assert_eq!(forward_flag(ForwardType::Remote), "-R");
    }

    #[test]
    fn validates_and_builds_the_rule() {
        let mut draft = ForwardRuleDraft::default();
        assert!(!draft.valid());
        draft.bind_port = "8080".into();
        assert!(!draft.valid());
        draft.target_port = "80".into();
        assert_eq!(
            draft.rule(),
            Some(ConfigForward {
                kind: ForwardType::Local,
                bind_port: 8080,
                target_host: Some("localhost".into()),
                target_port: Some(80),
            })
        );
        draft.kind = ForwardType::Dynamic;
        draft.target_port.clear();
        assert_eq!(draft.rule().unwrap().target_host, None);
        draft.after_submit();
        assert_eq!(draft.bind_port, "");
        assert_eq!(draft.target_host, "localhost");
    }

    #[test]
    fn draws_the_documented_local_forward() {
        let model = forward_diagram(ForwardType::Local, "", "localhost", "", "db-primary");
        assert_eq!(model.nodes[0].sub, "listens on 127.0.0.1:…");
        assert_eq!(model.nodes[2].title, "localhost:…");
        assert_eq!(
            model.summary,
            "Apps connecting to 127.0.0.1:… on this computer reach localhost:… through db-primary."
        );
        let remote = forward_diagram(ForwardType::Remote, "80", "", "", "");
        assert_eq!(remote.nodes[0].title, "SSH server");
        assert_eq!(remote.nodes[0].sub, "listens on :80");
    }
}
