//! Shared session lists read from a real web server: the nginx of
//! `hack/e2e/containers/portal`, over http, over https with a certificate from an authority
//! this computer trusts, and over https with one nobody vouches for.
//!
//! ```bash
//! docker build -t muxusrs-e2e-portal hack/e2e/containers/portal
//! docker run -d --name muxusrs-e2e-portal -p 127.0.0.1:28080:80 -p 127.0.0.1:28443:443 \
//!   -p 127.0.0.1:28444:444 muxusrs-e2e-portal
//! mkdir -p target/e2e-portal && curl -s -o target/e2e-portal/ca.pem http://127.0.0.1:28080/ca.pem
//! SSL_CERT_FILE=$PWD/target/e2e-portal/ca.pem MUXUS_E2E_PORTAL=127.0.0.1 \
//!   cargo test -p muxus-session --test shared_sessions_portal -- --ignored
//! docker rm -f muxusrs-e2e-portal
//! ```
//!
//! `SSL_CERT_FILE` stands in for the operating system's certificate store, where a company
//! puts the authority its portals use.

mod support;

use muxus_core::api::{SavedHostProfile, SharedSessionFetchResponse, SharedSessionSource};
use muxus_session::ApiError;
use serde_json::json;
use support::{Sandbox, body, start};

const HTTP: u16 = 28080;
const TRUSTED: u16 = 28443;
const UNTRUSTED: u16 = 28444;

fn host() -> String {
    std::env::var("MUXUS_E2E_PORTAL").expect("MUXUS_E2E_PORTAL names the host the portal is on")
}

async fn follow(sandbox: &Sandbox, name: &str, url: String) -> SharedSessionSource {
    sandbox
        .backend
        .save_shared_session_source(body(json!({
            "name": name, "url": url, "folder": name, "refreshMinutes": 60,
        })))
        .await
        .expect("the link is stored")
}

async fn check(sandbox: &Sandbox, source: &SharedSessionSource) -> Result<(), ApiError> {
    sandbox
        .backend
        .sync_shared_sessions(&source.id, true)
        .await
        .map(|_| ())
}

async fn hosts(sandbox: &Sandbox) -> Vec<SavedHostProfile> {
    sandbox.backend.saved_hosts().await.unwrap().profiles
}

#[tokio::test]
#[ignore = "needs the portal container"]
async fn follows_the_list_a_portal_publishes_through_its_redirects() {
    let sandbox = start().await;
    let source = follow(&sandbox, "Lab", format!("http://{}:{HTTP}/me", host())).await;

    let result = sandbox
        .backend
        .sync_shared_sessions(&source.id, true)
        .await
        .unwrap()
        .unwrap();

    // Three sessions, and the gateway the desktop goes through as a jump host.
    assert_eq!(result.added, 4);
    assert_eq!(result.source.host_count, 4);
    assert!(result.source.last_synced_at.is_some());
    assert_eq!(result.source.last_error, None);
    let hosts = hosts(&sandbox).await;
    let mut placed: Vec<(&str, &str)> = hosts
        .iter()
        .map(|host| {
            (
                host.name.as_str(),
                host.metadata.group.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    placed.sort();
    // The folder name arrived in the Windows code page and reads as written.
    assert_eq!(
        placed,
        [
            ("desk", "Lab/Zürich"),
            ("edge", "Lab"),
            ("jump@bastion.lab.example.net:2222", "Lab/Jump hosts"),
            ("node0", "Lab/Zürich"),
        ]
    );
    assert!(
        hosts
            .iter()
            .all(|host| host.metadata.shared_source_id.as_deref() == Some(source.id.as_str()))
    );

    // Checked again, nothing changed.
    let again = sandbox
        .backend
        .sync_shared_sessions(&source.id, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (again.added, again.updated, again.removed, again.unchanged),
        (0, 0, 0, 4)
    );
}

#[tokio::test]
#[ignore = "needs the portal container"]
async fn says_what_the_portal_answered_instead_of_a_list() {
    let sandbox = start().await;
    let address = format!("{}:{HTTP}", host());

    let private = follow(
        &sandbox,
        "Private",
        format!("http://{address}/private/token-0123456789/list"),
    )
    .await;
    let error = check(&sandbox, &private).await.unwrap_err();
    assert_eq!(error.status, 502);
    assert_eq!(error.message, format!("{address} answered 403 Forbidden."));

    let gone = follow(&sandbox, "Gone", format!("http://{address}/lists/gone")).await;
    let error = check(&sandbox, &gone).await.unwrap_err();
    assert_eq!(error.message, format!("{address} answered 404 Not Found."));

    let circles = follow(&sandbox, "Circles", format!("http://{address}/loop")).await;
    let error = check(&sandbox, &circles).await.unwrap_err();
    assert_eq!(error.message, "The link redirects too many times.");

    let page = follow(&sandbox, "Page", format!("http://{address}/signin")).await;
    let error = check(&sandbox, &page).await.unwrap_err();
    assert_eq!(
        error.message,
        "The link returned a web page instead of a session list. It may need a sign-in."
    );

    // Each link keeps what went wrong, and none of it repeats the personal part.
    let sources = sandbox
        .backend
        .shared_session_sources()
        .await
        .unwrap()
        .sources;
    assert_eq!(sources.len(), 4);
    for source in &sources {
        let said = source.last_error.as_deref().expect("the problem is kept");
        assert!(!said.contains("token-0123456789"), "{said}");
        assert_eq!(source.host_count, 0);
    }
    assert!(hosts(&sandbox).await.is_empty());
}

#[tokio::test]
#[ignore = "needs the portal container, and its authority in SSL_CERT_FILE"]
async fn reads_a_list_over_https_from_a_portal_with_a_company_certificate() {
    let sandbox = start().await;
    let source = follow(
        &sandbox,
        "Team",
        format!(
            "https://{}:{TRUSTED}/lists/team.muxus-sessions.json",
            host()
        ),
    )
    .await;

    let fetched = sandbox
        .backend
        .fetch_shared_sessions(&source.id, true)
        .await
        .unwrap();
    assert!(matches!(
        &fetched,
        SharedSessionFetchResponse::Fetched { content } if content.contains("muxus-shared-sessions")
    ));
    let result = sandbox
        .backend
        .sync_shared_sessions(&source.id, true)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(result.added, 3);
    let hosts = hosts(&sandbox).await;
    let bastion = hosts.iter().find(|host| host.name == "bastion").unwrap();
    let leaf = hosts.iter().find(|host| host.name == "leaf-01").unwrap();
    assert_eq!(bastion.metadata.group.as_deref(), Some("Team/Jump hosts"));
    assert_eq!(leaf.metadata.group.as_deref(), Some("Team/Fabric"));
    assert_eq!(leaf.metadata.color.as_deref(), Some("#a855f7"));
    assert_eq!(
        serde_json::to_value(&leaf.profile).unwrap()["proxyJump"],
        json!([format!("muxus-host:{}", bastion.id)])
    );
}

#[tokio::test]
#[ignore = "needs the portal container"]
async fn refuses_a_certificate_nobody_vouches_for() {
    let sandbox = start().await;
    let address = format!("{}:{UNTRUSTED}", host());
    let source = follow(
        &sandbox,
        "Untrusted",
        format!("https://{address}/lists/lab.mxtsessions"),
    )
    .await;

    let error = check(&sandbox, &source).await.unwrap_err();

    assert_eq!(error.status, 502);
    assert_eq!(
        error.message,
        format!("The certificate of {address} is not trusted (UnknownIssuer).")
    );
    assert!(hosts(&sandbox).await.is_empty());
}
