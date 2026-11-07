//! Saved web pages (`client/src/web-hosts.ts`). Without a gateway the page opens straight
//! away. With one, Muxus connects to the gateway (asking for passwords and host keys like a
//! tunnel does), starts a SOCKS5 forward on a free local port, and opens the page in a Muxus
//! browser window that sends everything through it. The window owns the forward and stops it
//! when it closes.
//!
//! The window itself is `muxus_webview`'s; a build without it opens pages the way the browser
//! build of Muxus does ([`browser_proxy_notice`]).

use std::future::Future;

use muxus_core::api::{ForwardRequest, ForwardStatus, ForwardType, SavedHostProfile};
use muxus_core::protocol::{
    SavedHostSessionProfile, SshGateway, SshProfile, TerminalClientMessage,
};
use muxus_session::Backend;
use serde_json::{Value, json};

use crate::features::forwarding::start::{DialAnswer, DialPrompt, dial_connection};
use crate::hosts::saved::saved_host_display_name;

/// A SOCKS5 forward started for a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageProxy {
    pub forward_id: String,
    pub port: u16,
}

/// The page a saved host opens: its address and the gateway it goes through.
pub fn web_page(host: &SavedHostProfile) -> Option<(&str, Option<&SshGateway>)> {
    match &host.profile {
        SavedHostSessionProfile::Web(profile) => {
            Some((profile.url.as_str(), profile.ssh_gateway.as_ref()))
        }
        _ => None,
    }
}

/// What the page window is asked to show (`bridge.openWebWindow({ key, url, title, … })`).
pub fn window_request(host: &SavedHostProfile, proxy: Option<&PageProxy>) -> Option<Value> {
    let (url, _) = web_page(host)?;
    let mut request = json!({
        "key": host.id,
        "url": url,
        "title": saved_host_display_name(host),
    });
    if let Some(proxy) = proxy {
        request["proxy"] = muxus_webview::proxy_url(proxy.port).into();
        request["forwardId"] = proxy.forward_id.clone().into();
    }
    Some(request)
}

/// The error for a page the window refused.
pub fn refused_message(host: &SavedHostProfile) -> String {
    let url = web_page(host).map(|(url, _)| url).unwrap_or_default();
    format!(
        "“{}” cannot be opened: {url} is not an address a browser window can show.",
        saved_host_display_name(host)
    )
}

/// The dial for a gateway: the saved SSH host or ssh_config alias it names.
pub fn gateway_dial(gateway: &SshGateway) -> TerminalClientMessage {
    let mut profile = SshProfile::new(gateway.target.clone());
    profile.profile_id = gateway.profile_id.clone();
    TerminalClientMessage::Dial { profile }
}

/// The SOCKS5 forward a page goes through: on a free local port, independent of terminal tabs
/// on the same connection, so closing one does not stop the page's proxy.
pub fn proxy_forward_request(conn_id: &str) -> ForwardRequest {
    ForwardRequest {
        conn_id: conn_id.to_owned(),
        kind: ForwardType::Dynamic,
        bind_port: 0,
        target_host: None,
        target_port: None,
        tunnel_id: None,
        independent: Some(true),
    }
}

/// `startProxy`: dial the gateway and start a SOCKS5 forward on a free local port. `ask`
/// answers the gateway's sign-in questions; a dial that fails takes its questions with it.
pub async fn start_proxy<A, F>(
    backend: &Backend,
    gateway: &SshGateway,
    ask: A,
) -> Result<PageProxy, String>
where
    A: FnMut(DialPrompt) -> F,
    F: Future<Output = DialAnswer>,
{
    let (dialed, conn_id) = dial_connection(backend, gateway_dial(gateway), |_| {}, ask).await?;
    let started = backend
        .start_forward(proxy_forward_request(&conn_id))
        .await
        .map_err(|error| error.message);
    // The forward holds its own lease on the connection now.
    dialed.close();
    let started = started?;
    Ok(PageProxy {
        forward_id: started.id,
        port: started.bind_port,
    })
}

/// Whether a page's forward still runs: it may have been stopped from the forwarding panel,
/// or lost with its gateway.
pub async fn forward_running(backend: &Backend, forward_id: &str) -> bool {
    backend.forwards(None).await.is_ok_and(|forwards| {
        forwards
            .iter()
            .any(|forward| forward.id == forward_id && forward.status == ForwardStatus::Active)
    })
}

/// Without a page window a browser cannot be given a proxy: say where the page's proxy runs.
pub fn browser_proxy_notice(title: &str, port: u16, url: &str) -> String {
    format!(
        "SOCKS5 proxy for “{title}” is running on 127.0.0.1:{port}. Set it as your browser's SOCKS5 proxy (with remote DNS) and open {url}. Stop it in the forwarding panel."
    )
}

#[cfg(test)]
mod tests {
    use muxus_core::api::{OpenSshProfileMetadata, SavedHostKind};
    use muxus_core::protocol::WebProfile;

    use super::*;

    fn page(gateway: Option<SshGateway>) -> SavedHostProfile {
        SavedHostProfile {
            id: "w1".into(),
            kind: SavedHostKind::Web,
            name: "Grafana".into(),
            profile: SavedHostSessionProfile::Web(WebProfile {
                profile_id: None,
                url: "https://grafana.internal:3000".into(),
                ssh_gateway: gateway,
            }),
            metadata: OpenSshProfileMetadata::default(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn asks_the_window_for_the_page_and_its_proxy() {
        assert_eq!(
            window_request(&page(None), None),
            Some(
                json!({ "key": "w1", "url": "https://grafana.internal:3000", "title": "Grafana" })
            )
        );
        let proxy = PageProxy {
            forward_id: "f1".into(),
            port: 41000,
        };
        let request = window_request(&page(None), Some(&proxy)).unwrap();
        assert_eq!(
            muxus_webview::parse_web_window_request(&request)
                .unwrap()
                .proxy
                .as_deref(),
            Some("socks5://127.0.0.1:41000")
        );
        assert_eq!(request["forwardId"], "f1");
    }

    #[test]
    fn dials_the_gateway_and_asks_for_an_independent_socks_proxy_on_any_port() {
        let gateway = SshGateway {
            target: "bastion-a".into(),
            profile_id: Some("p2".into()),
        };
        assert_eq!(
            serde_json::to_value(gateway_dial(&gateway)).unwrap(),
            json!({ "op": "dial", "profile": { "kind": "ssh", "target": "bastion-a", "profileId": "p2" } })
        );
        assert_eq!(
            serde_json::to_value(proxy_forward_request("c1")).unwrap(),
            json!({ "connId": "c1", "type": "dynamic", "bindPort": 0, "independent": true })
        );
    }

    #[test]
    fn words_the_errors_and_notices() {
        assert_eq!(
            refused_message(&page(None)),
            "“Grafana” cannot be opened: https://grafana.internal:3000 is not an address a browser window can show."
        );
        assert!(browser_proxy_notice("Grafana", 41000, "https://g").contains("127.0.0.1:41000"));
    }

    /// The gateway signs in through the prompts, and the page's proxy carries a connection to
    /// a name the gateway resolves.
    #[test]
    fn starts_a_socks_proxy_through_the_gateway() {
        use std::time::Duration;

        use muxus_core::api::ForwardLifecycle;
        use muxus_core::protocol::AuthPromptResponse;
        use muxus_session::{BackendOptions, RuntimeChoice};
        use muxus_ssh::testing::server::{ServerSpec, start as start_sshd};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(start_sshd(
            ServerSpec::new()
                .password("pw")
                .methods(&[russh::MethodKind::Password]),
        ));
        let echo = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        runtime.spawn(async move {
            while let Ok((mut socket, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 64];
                    while let Ok(read) = socket.read(&mut buffer).await {
                        if read == 0 || socket.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let home = scratch.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            format!(
                "Host bastion\n  HostName 127.0.0.1\n  User ops\n  Port {}\n",
                server.port()
            ),
        )
        .unwrap();
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime.block_on(Backend::start(options)).unwrap();

        let gateway = SshGateway {
            target: "bastion".into(),
            profile_id: None,
        };
        let proxy = runtime
            .block_on(start_proxy(&backend, &gateway, |prompt| async move {
                match prompt {
                    DialPrompt::HostKey(_) => DialAnswer::HostKey(true),
                    DialPrompt::Auth(_) => DialAnswer::Auth(Some(AuthPromptResponse {
                        answers: vec!["pw".into()],
                        remember_password: None,
                        skipped: None,
                    })),
                }
            }))
            .unwrap();
        assert!(proxy.port > 0);
        let forwards = runtime.block_on(backend.forwards(None)).unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(forwards[0].id, proxy.forward_id);
        assert_eq!(forwards[0].bind_port, proxy.port);
        assert_eq!(forwards[0].lifecycle, ForwardLifecycle::Independent);
        assert!(runtime.block_on(forward_running(&backend, &proxy.forward_id)));

        // SOCKS5 CONNECT by name: `localhost` resolves on the gateway's side.
        let echoed = runtime.block_on(async {
            let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port))
                .await
                .unwrap();
            socket.write_all(&[5, 1, 0]).await.unwrap();
            let mut choice = [0u8; 2];
            socket.read_exact(&mut choice).await.unwrap();
            assert_eq!(choice, [5, 0]);
            let mut request = vec![5, 1, 0, 3, 9];
            request.extend_from_slice(b"localhost");
            request.extend_from_slice(&echo_port.to_be_bytes());
            socket.write_all(&request).await.unwrap();
            let mut reply = [0u8; 10];
            tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut reply))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(reply[1], 0, "SOCKS reply {reply:?}");
            socket.write_all(b"ping").await.unwrap();
            let mut buffer = [0u8; 4];
            tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            buffer
        });
        assert_eq!(&echoed, b"ping");

        // The window closing stops it.
        runtime
            .block_on(backend.stop_forward(&proxy.forward_id))
            .unwrap();
        assert!(!runtime.block_on(forward_running(&backend, &proxy.forward_id)));
        runtime.block_on(backend.shutdown());
    }
}
