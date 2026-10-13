//! The NETCONF client against a fake server on the far end of an in-memory
//! channel (port of `tests/unit/server/netconf.test.ts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::management::NetconfBase;
use muxus_mgmt::netconf::NetconfClient;
use muxus_mgmt::netconf::framing::{NetconfDecoder, encode_chunked, encode_end_of_message};
use regex::Regex;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, WriteHalf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type Reply = Box<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

struct FakeServer {
    /// Every message the client sent, the hello first.
    sent: Arc<Mutex<Vec<String>>>,
    /// Send a message to the client in the negotiated framing.
    send: mpsc::UnboundedSender<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FakeServer {
    /// The server's end of the channel goes away with it.
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// A NETCONF server on the far end of an in-memory channel.
fn fake_server(base11: bool, reply: Reply) -> (DuplexStream, FakeServer) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (mut reader, writer) = tokio::io::split(server);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (send, mut outgoing) = mpsc::unbounded_channel::<String>();
    let chunked = Arc::new(Mutex::new(false));
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let write = {
        let chunked = chunked.clone();
        let writer = writer.clone();
        move |xml: String| {
            let chunked = *chunked.lock().unwrap();
            let writer = writer.clone();
            async move {
                let frame = if chunked {
                    encode_chunked(&xml).unwrap()
                } else {
                    encode_end_of_message(&xml)
                };
                let mut writer: tokio::sync::MutexGuard<'_, WriteHalf<DuplexStream>> =
                    writer.lock().await;
                let _ = writer.write_all(&frame).await;
            }
        }
    };
    let capabilities = if base11 {
        "<capability>urn:ietf:params:netconf:base:1.0</capability><capability>urn:ietf:params:netconf:base:1.1</capability>"
    } else {
        "<capability>urn:ietf:params:netconf:base:1.0</capability>"
    };
    let hello = format!(
        "<hello xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><capabilities>{capabilities}</capabilities><session-id>9</session-id></hello>"
    );
    let mut tasks = Vec::new();
    {
        let write = write.clone();
        tasks.push(tokio::spawn(async move {
            write(hello).await;
            while let Some(xml) = outgoing.recv().await {
                write(xml).await;
            }
        }));
    }
    {
        let sent = sent.clone();
        let send = send.clone();
        tasks.push(tokio::spawn(async move {
            let id_pattern = Regex::new(r#"message-id="([^"]+)""#).unwrap();
            let body_pattern = Regex::new(r"(?s)<rpc[^>]*>(.*)</rpc>").unwrap();
            let mut decoder = NetconfDecoder::default();
            let mut buffer = vec![0; 4096];
            let mut hello_seen = false;
            loop {
                let Ok(read) = reader.read(&mut buffer).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                for message in decoder.push(&buffer[..read]).unwrap() {
                    sent.lock().unwrap().push(message.clone());
                    if !hello_seen {
                        hello_seen = true;
                        if base11 && message.contains("base:1.1") {
                            decoder.use_chunked();
                            *chunked.lock().unwrap() = true;
                        }
                        continue;
                    }
                    let id = id_pattern
                        .captures(&message)
                        .map(|c| c[1].to_owned())
                        .unwrap_or_default();
                    let body = body_pattern
                        .captures(&message)
                        .map(|c| c[1].to_owned())
                        .unwrap_or_default();
                    if let Some(answer) = reply(&body, &id) {
                        let _ = send.send(answer);
                    }
                }
            }
        }));
    }
    drop(writer);
    (client, FakeServer { sent, send, tasks })
}

#[tokio::test]
async fn negotiates_chunked_framing_and_matches_replies_to_their_rpcs_out_of_order() {
    let held: Arc<Mutex<Vec<String>>> = Arc::default();
    let keep = held.clone();
    let (channel, server) = fake_server(
        true,
        Box::new(move |body, id| {
            let reply = format!(
                "<rpc-reply message-id=\"{id}\" xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><data>{body}</data></rpc-reply>"
            );
            if body.contains("slow") {
                keep.lock().unwrap().push(reply);
                return None;
            }
            Some(reply)
        }),
    );
    let (client, _notifications) = NetconfClient::open(channel).await.unwrap();
    assert_eq!(client.base, NetconfBase::V1_1);
    assert_eq!(client.session_id, "9");
    let client = Arc::new(client);
    let slow = {
        let client = client.clone();
        tokio::spawn(async move { client.rpc("<slow/>", None, None).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let fast = client.rpc("<fast/>", None, None).await.unwrap();
    assert!(fast.xml.contains("<fast/>"));
    for reply in held.lock().unwrap().drain(..) {
        server.send.send(reply).unwrap();
    }
    let slow = slow.await.unwrap().unwrap();
    assert!(slow.xml.contains("<slow/>"));
    assert_eq!(slow.message_id, "1");
    assert!(server.sent.lock().unwrap()[0].contains("urn:ietf:params:netconf:base:1.1"));
}

#[tokio::test]
async fn stays_on_end_of_message_framing_with_a_base_1_0_server_and_delivers_notifications() {
    let (channel, server) = fake_server(
        false,
        Box::new(|_body, id| Some(format!("<rpc-reply message-id=\"{id}\"><ok/></rpc-reply>"))),
    );
    let (client, mut notifications) = NetconfClient::open(channel).await.unwrap();
    assert_eq!(client.base, NetconfBase::V1_0);
    server
        .send
        .send(
            "<notification xmlns=\"urn:ietf:params:xml:ns:netconf:notification:1.0\"><eventTime>2026-10-10T10:00:00Z</eventTime><x/></notification>"
                .into(),
        )
        .unwrap();
    assert!(
        client
            .rpc("<get/>", None, None)
            .await
            .unwrap()
            .xml
            .contains("<ok/>")
    );
    let notification = notifications.recv().await.unwrap();
    assert_eq!(
        notification.event_time.as_deref(),
        Some("2026-10-10T10:00:00Z")
    );
}

#[tokio::test]
async fn fails_pending_rpcs_when_the_channel_closes_and_honours_cancellation() {
    let (channel, server) = fake_server(true, Box::new(|_, _| None));
    let (client, _notifications) = NetconfClient::open(channel).await.unwrap();
    let client = Arc::new(client);
    let cancel = CancellationToken::new();
    let cancelled = {
        let client = client.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { client.rpc("<a/>", None, Some(&cancel)).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();
    assert!(cancelled.await.unwrap().unwrap_err().cancelled);
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.rpc("<b/>", None, None).await })
    };
    let closed = client.closed();
    tokio::time::sleep(Duration::from_millis(10)).await;
    drop(server);
    // Dropping the fake server's ends closes the channel.
    let error = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the RPC failed when the channel closed")
        .unwrap()
        .unwrap_err();
    assert!(!error.message.is_empty());
    assert!(
        !tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        client
            .rpc("<c/>", None, None)
            .await
            .unwrap_err()
            .message
            .contains("closed")
    );
}

#[tokio::test]
async fn times_out_an_rpc_without_an_answer() {
    let (channel, _server) = fake_server(false, Box::new(|_, _| None));
    let (client, _notifications) = NetconfClient::open(channel).await.unwrap();
    let error = client
        .rpc("<get/>", Some(Duration::from_millis(1500)), None)
        .await
        .unwrap_err();
    assert_eq!(error.message, "No reply within 2 seconds.");
}

#[tokio::test]
async fn rejects_a_server_that_does_not_start_with_a_hello() {
    let (client, mut server) = tokio::io::duplex(4096);
    server
        .write_all(&encode_end_of_message("<rpc-reply/>"))
        .await
        .unwrap();
    let Err(error) = NetconfClient::open(client).await else {
        panic!("a server without a hello was accepted");
    };
    assert!(error.message.contains("hello"));
    let (client, server) = tokio::io::duplex(4096);
    drop(server);
    let Err(error) = NetconfClient::open(client).await else {
        panic!("a closed channel was accepted");
    };
    assert!(!error.message.is_empty());
}
