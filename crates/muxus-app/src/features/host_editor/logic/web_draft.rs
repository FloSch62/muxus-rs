//! Saved web pages (`client/src/components/host-editor/web-draft.ts`).

use muxus_core::api::{OpenSshMetadataPatch, SavedHostProfile, SavedHostProfileInput};
use muxus_core::js::js_trim;
use muxus_core::protocol::{SavedHostSessionProfile, SshGateway, WebProfile};

use super::native_draft::{optional_patch, text_patch};

/// Form state for a saved web page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebHostDraft {
    pub name: String,
    pub url: String,
    pub group: String,
    pub color: Option<String>,
    /// SSH host whose SOCKS5 proxy carries the page; `None` connects directly.
    pub ssh_gateway: Option<SshGateway>,
}

/// `blankWebDraft(prefillTarget, group, sshGateway)`.
pub fn blank_web_draft(
    prefill_target: &str,
    group: &str,
    ssh_gateway: Option<SshGateway>,
) -> WebHostDraft {
    let target = js_trim(prefill_target);
    WebHostDraft {
        name: String::new(),
        url: if target.is_empty() {
            String::new()
        } else {
            normalize_web_url(target)
        },
        group: group.to_owned(),
        color: None,
        ssh_gateway,
    }
}

/// `webDraftFromProfile`; `None` for other kinds (the TypeScript throws).
pub fn web_draft_from_profile(saved: &SavedHostProfile, duplicate: bool) -> Option<WebHostDraft> {
    let SavedHostSessionProfile::Web(profile) = &saved.profile else {
        return None;
    };
    Some(WebHostDraft {
        name: if duplicate {
            format!("{} copy", saved.name)
        } else {
            saved.name.clone()
        },
        url: profile.url.clone(),
        group: saved.metadata.group.clone().unwrap_or_default(),
        color: saved.metadata.color.clone(),
        ssh_gateway: profile.ssh_gateway.clone(),
    })
}

/// `intranet:8080/app` → `http://intranet:8080/app`; an explicit scheme
/// (`/^[a-z][a-z0-9+.-]*:\/\//i`) is kept.
pub fn normalize_web_url(value: &str) -> String {
    let text = js_trim(value);
    let has_scheme = text.split_once("://").is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    if text.is_empty() || has_scheme {
        text.to_owned()
    } else {
        format!("http://{text}")
    }
}

/// `webDraftProblem`: why the page cannot be saved yet.
pub fn web_draft_problem(draft: &WebHostDraft) -> Option<String> {
    if js_trim(&draft.name).is_empty() {
        return Some("A name is required — it labels this page in Muxus.".into());
    }
    let url = normalize_web_url(&draft.url);
    if url.is_empty() {
        return Some("Enter the address of the page.".into());
    }
    let Ok(parsed) = url::Url::parse(&url) else {
        return Some("This is not a valid address.".into());
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Some("Only http:// and https:// addresses can be opened.".into());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Some("The address needs a host name.".into());
    }
    None
}

/// `webDraftToInput`.
pub fn web_draft_to_input(
    draft: &WebHostDraft,
    existing_id: Option<&str>,
) -> SavedHostProfileInput {
    SavedHostProfileInput {
        id: existing_id.map(str::to_owned),
        name: js_trim(&draft.name).to_owned(),
        profile: SavedHostSessionProfile::Web(WebProfile {
            profile_id: None,
            url: normalize_web_url(&draft.url),
            ssh_gateway: draft.ssh_gateway.clone(),
        }),
    }
}

/// `webDraftMetadataPatch`: Muxus-only metadata written right after the page saves.
pub fn web_draft_metadata_patch(draft: &WebHostDraft) -> OpenSshMetadataPatch {
    OpenSshMetadataPatch {
        group: text_patch(&draft.group),
        color: optional_patch(&draft.color),
        ..OpenSshMetadataPatch::default()
    }
}

#[cfg(test)]
mod tests {
    //! Port of tests/unit/client/web-host-draft.test.ts.

    use muxus_core::api::{OpenSshProfileMetadata, SavedHostKind};

    use super::*;

    #[test]
    fn adds_http_to_an_address_without_a_scheme() {
        assert_eq!(
            normalize_web_url("grafana.internal:3000/d/x"),
            "http://grafana.internal:3000/d/x"
        );
        assert_eq!(normalize_web_url("https://ilo.lab"), "https://ilo.lab");
        assert_eq!(normalize_web_url("  "), "");
    }

    #[test]
    fn needs_a_name_and_an_http_s_address() {
        let draft = WebHostDraft {
            name: "Grafana".into(),
            url: "grafana.internal".into(),
            ..blank_web_draft("", "", None)
        };
        assert_eq!(web_draft_problem(&draft), None);
        let problem = |draft: WebHostDraft| web_draft_problem(&draft).unwrap();
        assert!(
            problem(WebHostDraft {
                name: " ".into(),
                ..draft.clone()
            })
            .contains("name")
        );
        assert!(
            problem(WebHostDraft {
                url: String::new(),
                ..draft.clone()
            })
            .contains("address")
        );
        assert!(
            problem(WebHostDraft {
                url: "ftp://files.lab".into(),
                ..draft.clone()
            })
            .contains("http")
        );
        assert_eq!(
            problem(WebHostDraft {
                url: "http://exa mple".into(),
                ..draft
            }),
            "This is not a valid address."
        );
    }

    #[test]
    fn saves_the_gateway_with_a_normalized_address() {
        let input = web_draft_to_input(
            &WebHostDraft {
                name: " Grafana ".into(),
                url: "grafana.internal:3000".into(),
                ssh_gateway: Some(SshGateway {
                    target: "bastion-a".into(),
                    profile_id: None,
                }),
                ..blank_web_draft("", "", None)
            },
            None,
        );
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            serde_json::json!({
                "name": "Grafana",
                "profile": {
                    "kind": "web",
                    "url": "http://grafana.internal:3000",
                    "sshGateway": { "target": "bastion-a" },
                },
            })
        );
    }

    #[test]
    fn round_trips_a_saved_page() {
        let profile = SavedHostSessionProfile::Web(WebProfile {
            profile_id: None,
            url: "https://grafana.internal".into(),
            ssh_gateway: Some(SshGateway {
                target: "bastion-a".into(),
                profile_id: Some("p2".into()),
            }),
        });
        let saved = SavedHostProfile {
            id: "w1".into(),
            kind: SavedHostKind::Web,
            name: "Grafana".into(),
            profile: profile.clone(),
            metadata: OpenSshProfileMetadata {
                group: Some("Monitoring".into()),
                ..Default::default()
            },
            created_at: String::new(),
            updated_at: String::new(),
        };
        let draft = web_draft_from_profile(&saved, true).unwrap();
        assert_eq!(draft.name, "Grafana copy");
        assert_eq!(draft.group, "Monitoring");
        assert_eq!(web_draft_to_input(&draft, Some("w1")).profile, profile);
    }

    #[test]
    fn starts_from_a_typed_address_and_a_gateway() {
        let gateway = SshGateway {
            target: "bastion-a".into(),
            profile_id: None,
        };
        let draft = blank_web_draft(" intranet:8080 ", "Lab", Some(gateway.clone()));
        assert_eq!(draft.url, "http://intranet:8080");
        assert_eq!(draft.group, "Lab");
        assert_eq!(draft.ssh_gateway, Some(gateway));
    }
}
