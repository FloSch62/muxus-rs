//! The forwarding panel against a live SSH server, in a headless window.

mod support;

use std::time::Duration;

use gpui::TestAppContext;
use muxus_app::features::forwarding::panel::{ForwardingPanel, ForwardingPanelEvent};
use muxus_app::features::forwarding::start::{DialAnswer, DialPrompt};
use muxus_core::api::{ForwardStatus, ForwardType, TunnelInput};
use muxus_core::protocol::AuthPromptResponse;
use support::{Fixture, allow_backend_threads, init_ui, wait_until};

/// A web server on loopback answering every request with `body`.
fn serve_http(runtime: &tokio::runtime::Runtime, body: &'static str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    runtime.spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    port
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_get(port: u16) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out
}

/// The tunnel editor told its owner `Close` before `Saved`; the panel drops its
/// subscription on `Close`, so "Save & start" saved the tunnel but never listed or started
/// it. Now the saved tunnel shows, starts, and carries traffic.
#[gpui::test]
fn save_and_start_lists_starts_and_carries_traffic(cx: &mut TestAppContext) {
    let fx = Fixture::start(|_| {});
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let web = serve_http(&fx.runtime, "tunnel-ok");
    let bind = free_port();
    let record = fx
        .runtime
        .block_on(fx.backend.save_tunnel(TunnelInput {
            id: None,
            name: Some("web".into()),
            target: "box".into(),
            kind: ForwardType::Local,
            bind_port: bind,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(web),
            ssh_options: None,
            auto_start: Some(false),
            auto_reconnect: Some(false),
        }))
        .expect("the tunnel saves");

    let backend = fx.backend.clone();
    let host = cx.add_window(move |_, cx| ForwardingPanel::new(backend, cx));
    let panel = host.update(cx, |_, _, cx| cx.entity()).unwrap();
    // Answer the dial's questions as the window's prompt dialogs would.
    cx.update(|cx| {
        cx.subscribe(&panel, |_, event: &ForwardingPanelEvent, _| {
            if let ForwardingPanelEvent::Prompt { prompt, reply } = event {
                let answer = match prompt {
                    DialPrompt::HostKey(_) => DialAnswer::HostKey(true),
                    DialPrompt::Auth(_) => DialAnswer::Auth(Some(AuthPromptResponse {
                        answers: vec!["pw".into()],
                        remember_password: None,
                        skipped: None,
                    })),
                };
                let _ = reply.try_send(answer);
            }
        })
        .detach();
    });
    wait_until(cx, Duration::from_secs(10), "the tunnel list", |cx| {
        panel.read_with(cx, |panel, _| panel.tunnels().len() == 1)
    });

    // Edit the tunnel and press "Save & start".
    host.update(cx, |panel, window, cx| {
        panel.edit_tunnel(Some(record.clone()), window, cx)
    })
    .unwrap();
    let editor = panel
        .read_with(cx, |panel, _| panel.editor())
        .expect("the editor opens");
    wait_until(cx, Duration::from_millis(500), "the editor", |_| true);
    editor.update(cx, |editor, cx| editor.save(true, cx));

    wait_until(cx, Duration::from_secs(20), "the tunnel to run", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.forwards().iter().any(|forward| {
                forward.tunnel_id.as_deref() == Some(record.id.as_str())
                    && forward.status == ForwardStatus::Active
            })
        })
    });
    assert!(panel.read_with(cx, |panel, _| panel.editor().is_none()));
    let reply = std::thread::spawn(move || http_get(bind)).join().unwrap();
    assert!(reply.ends_with("tunnel-ok"), "{reply}");
}
