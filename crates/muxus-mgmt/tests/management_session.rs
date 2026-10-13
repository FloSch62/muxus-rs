//! Management sessions end to end over their message channels (the
//! behaviour of `server/src/management/management-socket.ts`): login,
//! requests, subscriptions, gNOI calls, NETCONF over a fake SSH lease.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::management::{
    GnmiDataType, ManagementResult, ManagementSessionInfo, ManagementStreamMessage, NetconfBase,
};
use muxus_core::protocol::{
    AuthPromptResponse, ExitReason, ManagementClientMessage, ManagementProfile,
    ManagementServerMessage, NetconfProfile, SshGateway,
};
use muxus_mgmt::dial::DialError;
use muxus_mgmt::error::code;
use muxus_mgmt::netconf::BoxedChannel;
use muxus_mgmt::netconf::framing::{NetconfDecoder, encode_chunked, encode_end_of_message};
use muxus_mgmt::proto::gnmi;
use muxus_mgmt::session::{
    BoxFuture, BoxedStream, ManagementEnv, ManagementSession, SessionIo, SshLease, TrustedIdentity,
};
use prost::Message;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

#[derive(Default)]
struct TestEnv {
    recorded: Mutex<Vec<String>>,
    lease: Mutex<Option<Arc<FakeLease>>>,
    trusted: Mutex<Vec<TrustedIdentity>>,
}

impl ManagementEnv for TestEnv {
    fn trusted_identity(&self, host: &str, port: u16, gateway: &str) -> Option<String> {
        self.trusted
            .lock()
            .unwrap()
            .iter()
            .find(|identity| {
                identity.host == host && identity.port == port && identity.gateway == gateway
            })
            .map(|identity| identity.fingerprint.clone())
    }

    fn trust_identity(&self, identity: TrustedIdentity) {
        self.trusted.lock().unwrap().push(identity);
    }

    fn record_connection(&self, profile_id: &str) {
        self.recorded.lock().unwrap().push(profile_id.to_owned());
    }

    fn saved_profile(&self, profile_id: &str) -> Option<ManagementProfile> {
        (profile_id == "saved").then(|| {
            serde_json::from_value(json!({"kind": "netconf", "host": "r1", "profileId": "saved"}))
                .unwrap()
        })
    }

    fn connect_netconf<'a>(
        &'a self,
        _profile: &'a NetconfProfile,
        io: SessionIo,
    ) -> BoxFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async move {
            io.status("Authenticated.", true);
            let lease = self.lease.lock().unwrap().clone().ok_or("no SSH here")?;
            Ok(lease as Arc<dyn SshLease>)
        })
    }

    fn connect_ssh_gateway<'a>(
        &'a self,
        _gateway: &'a SshGateway,
        _io: SessionIo,
    ) -> BoxFuture<'a, Result<Arc<dyn SshLease>, String>> {
        Box::pin(async { Err("Permission denied (publickey).".to_owned()) })
    }
}

struct Session {
    input: async_channel::Sender<ManagementClientMessage>,
    output: async_channel::Receiver<ManagementServerMessage>,
}

impl Session {
    fn start(env: Arc<TestEnv>) -> Self {
        let (input, output) = ManagementSession::start(env);
        Self { input, output }
    }

    async fn send(&self, message: serde_json::Value) {
        let message: ManagementClientMessage = serde_json::from_value(message).unwrap();
        self.input.send(message).await.unwrap();
    }

    /// The next message that is not a status line.
    async fn next(&self) -> ManagementServerMessage {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), self.output.recv())
                .await
                .expect("the session went quiet")
                .expect("the session ended");
            if !matches!(message, ManagementServerMessage::Status { .. }) {
                return message;
            }
        }
    }

    async fn stream(&self) -> ManagementStreamMessage {
        match self.next().await {
            ManagementServerMessage::Stream(message) => message,
            other => panic!("expected a stream message, got {other:?}"),
        }
    }
}

fn header(call: &common::Call, name: &str) -> Option<String> {
    call.headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// A gNMI target that wants admin/secret and serves a little data.
async fn gnmi_target() -> u16 {
    common::listen(|mut call| async move {
        let path = call.path.clone();
        if path.contains("ServerReflection") {
            call.reply(&[], code::UNIMPLEMENTED, None);
            return;
        }
        if header(&call, "password").as_deref() != Some("secret") {
            call.reply(&[], code::UNAUTHENTICATED, Some("bad login"));
            return;
        }
        match path.as_str() {
            "/gnmi.gNMI/Capabilities" => {
                call.ended().await;
                let response = gnmi::CapabilityResponse {
                    supported_encodings: vec![4],
                    g_nmi_version: "0.10.0".into(),
                    ..Default::default()
                };
                call.reply(&[&response.encode_to_vec()], 0, None);
            }
            "/gnmi.gNMI/Get" => {
                call.ended().await;
                let response = gnmi::GetResponse {
                    notification: vec![gnmi::Notification {
                        timestamp: 1,
                        update: vec![gnmi::Update {
                            path: Some(
                                muxus_mgmt::gnmi::values::path_text_to_proto("/system/name")
                                    .unwrap(),
                            ),
                            val: Some(gnmi::TypedValue {
                                value: Some(gnmi::typed_value::Value::JsonIetfVal(
                                    b"\"leaf1\"".to_vec(),
                                )),
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                call.reply(&[&response.encode_to_vec()], 0, None);
            }
            "/gnmi.gNMI/Subscribe" => {
                let _request = call.next_request().await;
                for value in [1u64, 2, 3] {
                    let update = gnmi::SubscribeResponse {
                        response: Some(gnmi::subscribe_response::Response::Update(
                            gnmi::Notification {
                                timestamp: value as i64,
                                update: vec![gnmi::Update {
                                    path: Some(
                                        muxus_mgmt::gnmi::values::path_text_to_proto("/c").unwrap(),
                                    ),
                                    val: Some(gnmi::TypedValue {
                                        value: Some(gnmi::typed_value::Value::UintVal(value)),
                                    }),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    };
                    call.send(&update.encode_to_vec());
                }
                let sync = gnmi::SubscribeResponse {
                    response: Some(gnmi::subscribe_response::Response::SyncResponse(true)),
                    ..Default::default()
                };
                call.send(&sync.encode_to_vec());
                // Open until the client cancels.
                call.ended().await;
            }
            "/gnoi.system.System/Time" => {
                call.ended().await;
                let response = muxus_mgmt::services::json::encode_json(
                    &muxus_mgmt::proto::pool()
                        .get_message_by_name("gnoi.system.TimeResponse")
                        .unwrap(),
                    &json!({"time": "1760000000123456789"}),
                )
                .unwrap();
                call.reply(&[&response], 0, None);
            }
            "/gnoi.system.System/Ping" => {
                let _request = call.next_request().await;
                for sequence in 1..=3 {
                    let reply = muxus_mgmt::services::json::encode_json(
                        &muxus_mgmt::proto::pool()
                            .get_message_by_name("gnoi.system.PingResponse")
                            .unwrap(),
                        &json!({"source": "10.0.0.2", "sequence": sequence, "time": 1500}),
                    )
                    .unwrap();
                    call.send(&reply);
                }
                call.finish(0, None);
            }
            _ => call.reply(&[], code::UNIMPLEMENTED, None),
        }
    })
    .await
}

async fn login(session: &Session, port: u16) {
    session
        .send(json!({"op": "connect", "profile": {
            "kind": "gnmi", "host": "127.0.0.1", "port": port, "username": "admin", "tls": "plaintext"
        }}))
        .await;
    let ManagementServerMessage::AuthPrompt(prompt) = session.next().await else {
        panic!("no login prompt");
    };
    assert_eq!(prompt.name.as_deref(), Some("gNMI login"));
    assert_eq!(prompt.prompts.len(), 1);
    session
        .send(json!({"op": "auth-response", "answers": ["wrong"]}))
        .await;
    // The device turns the first password down; the prompt says so.
    let ManagementServerMessage::AuthPrompt(retry) = session.next().await else {
        panic!("no second login prompt");
    };
    assert_eq!(
        retry.instructions.as_deref(),
        Some("The device rejected the login.")
    );
    session
        .send(json!({"op": "auth-response", "answers": ["secret"]}))
        .await;
}

#[tokio::test]
async fn logs_in_runs_requests_and_streams_a_subscription() {
    let port = gnmi_target().await;
    let env = Arc::new(TestEnv::default());
    let session = Session::start(env.clone());
    login(&session, port).await;
    let ManagementServerMessage::Ready { info, .. } = session.next().await else {
        panic!("not ready");
    };
    let ManagementSessionInfo::Gnmi(info) = info else {
        panic!("not a gNMI session");
    };
    assert_eq!(info.version, "0.10.0");
    assert_eq!(info.address, format!("admin@127.0.0.1:{port}"));
    assert_eq!(info.services, None);
    assert!(env.recorded.lock().unwrap().is_empty());

    session
        .send(json!({"op": "gnmi-get", "id": "g1", "paths": ["/system/name"], "type": "state"}))
        .await;
    let ManagementStreamMessage::Result {
        id, result, bytes, ..
    } = session.stream().await
    else {
        panic!("no Get result");
    };
    assert_eq!(id, "g1");
    assert!(bytes > 0);
    let ManagementResult::GnmiGet { notifications } = result else {
        panic!("not a Get result");
    };
    assert_eq!(notifications[0].updates[0].value.value, json!("leaf1"));

    session
        .send(json!({"op": "gnmi-subscribe", "id": "s1", "mode": "stream",
            "subscriptions": [{"path": "/c", "mode": "sample", "sampleIntervalMs": 1000}]}))
        .await;
    let mut values = Vec::new();
    loop {
        match session.stream().await {
            ManagementStreamMessage::GnmiNotifications {
                id, notifications, ..
            } => {
                assert_eq!(id, "s1");
                values.extend(
                    notifications
                        .into_iter()
                        .map(|n| n.updates[0].value.value.clone()),
                );
            }
            ManagementStreamMessage::GnmiSync { id } => {
                assert_eq!(id, "s1");
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(values, vec![json!(1), json!(2), json!(3)]);
    session.send(json!({"op": "cancel", "id": "s1"})).await;
    assert_eq!(
        session.stream().await,
        ManagementStreamMessage::StreamEnd {
            id: "s1".into(),
            error: None
        }
    );

    session
        .send(json!({"op": "grpc-call", "id": "t1", "method": "/gnoi.system.System/Time"}))
        .await;
    let ManagementStreamMessage::Result { result, .. } = session.stream().await else {
        panic!("no Time result");
    };
    assert_eq!(
        result,
        ManagementResult::GrpcCall {
            method: "/gnoi.system.System/Time".into(),
            response: json!({"time": "1760000000123456789"})
                .as_object()
                .unwrap()
                .clone(),
        }
    );

    session
        .send(
            json!({"op": "grpc-stream", "id": "p1", "method": "/gnoi.system.System/Ping",
            "request": {"destination": "10.0.0.2", "count": 3}}),
        )
        .await;
    let mut pings = 0;
    loop {
        match session.stream().await {
            ManagementStreamMessage::GrpcMessages { messages, .. } => pings += messages.len(),
            ManagementStreamMessage::StreamEnd { id, error } => {
                assert_eq!(id, "p1");
                assert_eq!(error, None);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(pings, 3);

    // Methods outside the list never reach the device.
    session
        .send(json!({"op": "grpc-call", "id": "x1", "method": "/gnoi.file.File/Put"}))
        .await;
    let ManagementStreamMessage::Error { error, .. } = session.stream().await else {
        panic!("an unknown method was called");
    };
    assert_eq!(error.message, "Muxus does not call /gnoi.file.File/Put.");

    // So does a request that fails validation, and a malformed path.
    session
        .send(json!({"op": "gnmi-get", "id": "bad", "paths": []}))
        .await;
    let ManagementStreamMessage::Error { id, .. } = session.stream().await else {
        panic!("an invalid request ran");
    };
    assert_eq!(id, "bad");
    session
        .send(json!({"op": "gnmi-get", "id": "path", "paths": ["/a[b"]}))
        .await;
    let ManagementStreamMessage::Error { error, .. } = session.stream().await else {
        panic!("a malformed path ran");
    };
    assert!(error.message.starts_with("Invalid path: "));
    assert_eq!(error.code.as_deref(), Some("INVALID_ARGUMENT"));

    // Closing the input ends the session.
    session.input.close();
    while session.output.recv().await.is_ok() {}
}

#[tokio::test]
async fn fails_with_a_readable_message() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let session = Session::start(Arc::new(TestEnv::default()));
    session
        .send(json!({"op": "connect", "profile": {"kind": "gnmi", "host": "127.0.0.1", "port": port, "tls": "plaintext"}}))
        .await;
    assert_eq!(
        session.next().await,
        ManagementServerMessage::Exit {
            message: Some(
                "The remote computer refused the connection. Check the port and that the server runs."
                    .into()
            ),
            reason: ExitReason::Failed,
        }
    );
    // A gateway that turns the login down ends the session the same way.
    let session = Session::start(Arc::new(TestEnv::default()));
    session
        .send(json!({"op": "connect", "profile": {"kind": "gnmi", "host": "10.0.0.1", "sshGateway": {"target": "bastion"}}}))
        .await;
    assert_eq!(
        session.next().await,
        ManagementServerMessage::Exit {
            message: Some("Permission denied (publickey).".into()),
            reason: ExitReason::Failed,
        }
    );
    // A saved host that is gone.
    let session = Session::start(Arc::new(TestEnv::default()));
    session
        .send(json!({"op": "connect", "profile": {"kind": "netconf", "host": "r1", "profileId": "deleted"}}))
        .await;
    let ManagementServerMessage::Exit { message, .. } = session.next().await else {
        panic!("connected to a deleted host");
    };
    assert_eq!(
        message.as_deref(),
        Some("This saved host no longer exists.")
    );
}

/// An SSH lease whose `netconf` subsystem is a fake NETCONF server.
struct FakeLease {
    channel: Mutex<Option<tokio::io::DuplexStream>>,
    closed: watch::Sender<Option<String>>,
}

impl SshLease for FakeLease {
    fn open_direct_tcpip<'a>(
        &'a self,
        _host: &'a str,
        _port: u16,
    ) -> BoxFuture<'a, Result<BoxedStream, DialError>> {
        Box::pin(async { Err(DialError::from_channel_open("Connection refused", false)) })
    }

    fn open_subsystem<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<BoxedChannel, String>> {
        Box::pin(async move {
            assert_eq!(name, "netconf");
            let channel = self
                .channel
                .lock()
                .unwrap()
                .take()
                .ok_or("subsystem request failed")?;
            Ok(Box::new(channel) as BoxedChannel)
        })
    }

    fn address(&self) -> String {
        "admin@r1:830".into()
    }

    fn closed(&self) -> BoxFuture<'static, Option<String>> {
        let mut receiver = self.closed.subscribe();
        Box::pin(async move {
            let _ = receiver.wait_for(Option::is_some).await;
            receiver.borrow().clone()
        })
    }
}

/// A base:1.1 NETCONF server that answers every RPC with `<ok/>` and can push notifications.
fn netconf_server(stream: tokio::io::DuplexStream) -> mpsc::UnboundedSender<String> {
    let (notify, mut notifications) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let hello = "<hello xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><capabilities><capability>urn:ietf:params:netconf:base:1.1</capability><capability>urn:ietf:params:netconf:capability:candidate:1.0</capability></capabilities><session-id>4</session-id></hello>";
        writer
            .write_all(&encode_end_of_message(hello))
            .await
            .unwrap();
        let mut decoder = NetconfDecoder::default();
        let mut buffer = vec![0; 4096];
        let mut hello_seen = false;
        let message_id = regex::Regex::new(r#"message-id="([^"]+)""#).unwrap();
        loop {
            tokio::select! {
                read = reader.read(&mut buffer) => {
                    let Ok(read) = read else { return };
                    if read == 0 { return; }
                    for message in decoder.push(&buffer[..read]).unwrap() {
                        if !hello_seen {
                            hello_seen = true;
                            decoder.use_chunked();
                            continue;
                        }
                        let id = message_id
                            .captures(&message).map(|c| c[1].to_owned()).unwrap_or_default();
                        let reply = format!("<rpc-reply message-id=\"{id}\" xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><ok/></rpc-reply>");
                        writer.write_all(&encode_chunked(&reply).unwrap()).await.unwrap();
                    }
                }
                Some(xml) = notifications.recv() => {
                    writer.write_all(&encode_chunked(&xml).unwrap()).await.unwrap();
                }
            }
        }
    });
    notify
}

#[tokio::test]
async fn runs_netconf_over_an_ssh_lease_and_reports_when_it_drops() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let notify = netconf_server(server);
    let lease = Arc::new(FakeLease {
        channel: Mutex::new(Some(client)),
        closed: watch::channel(None).0,
    });
    let env = Arc::new(TestEnv {
        lease: Mutex::new(Some(lease.clone())),
        ..Default::default()
    });
    let session = Session::start(env.clone());
    session
        .send(json!({"op": "connect", "profile": {"kind": "netconf", "host": "ignored", "profileId": "saved"}}))
        .await;
    let ManagementServerMessage::Ready { info, profile } = session.next().await else {
        panic!("not ready");
    };
    let ManagementSessionInfo::Netconf(info) = info else {
        panic!("not a NETCONF session");
    };
    assert_eq!(info.base, NetconfBase::V1_1);
    assert_eq!(info.session_id, "4");
    assert_eq!(info.address, "admin@r1:830");
    // Saved hosts connect with what is stored, not what the tab sent.
    let ManagementProfile::Netconf(profile) = profile else {
        panic!("not a NETCONF profile");
    };
    assert_eq!(profile.host, "r1");
    assert_eq!(*env.recorded.lock().unwrap(), vec!["saved"]);

    session
        .send(json!({"op": "netconf-rpc", "id": "n1", "xml": "<lock><target><candidate/></target></lock>"}))
        .await;
    let ManagementStreamMessage::Result { result, .. } = session.stream().await else {
        panic!("no RPC result");
    };
    let ManagementResult::NetconfRpc { xml, message_id } = result else {
        panic!("not an RPC result");
    };
    assert!(xml.contains("<ok/>"));
    assert_eq!(message_id, "1");

    notify
        .send("<notification xmlns=\"urn:ietf:params:xml:ns:netconf:notification:1.0\"><eventTime>2026-10-12T10:00:00Z</eventTime><x/></notification>".into())
        .unwrap();
    let ManagementStreamMessage::NetconfNotification { event_time, .. } = session.stream().await
    else {
        panic!("no notification");
    };
    assert_eq!(event_time.as_deref(), Some("2026-10-12T10:00:00Z"));

    // gNMI requests on a NETCONF session are refused.
    session
        .send(json!({"op": "gnmi-get", "id": "g", "paths": ["/"]}))
        .await;
    let ManagementStreamMessage::Error { error, .. } = session.stream().await else {
        panic!("a gNMI request ran on NETCONF");
    };
    assert_eq!(error.message, "This is not a connected gNMI session.");

    lease.closed.send_replace(Some("Connection reset".into()));
    assert_eq!(
        session.next().await,
        ManagementServerMessage::Exit {
            message: Some("The SSH connection disconnected: Connection reset".into()),
            reason: ExitReason::Disconnected,
        }
    );
}

#[tokio::test]
async fn explains_a_server_without_a_netconf_subsystem() {
    let lease = Arc::new(FakeLease {
        channel: Mutex::new(None),
        closed: watch::channel(None).0,
    });
    let env = Arc::new(TestEnv {
        lease: Mutex::new(Some(lease)),
        ..Default::default()
    });
    let session = Session::start(env);
    session
        .send(json!({"op": "connect", "profile": {"kind": "netconf", "host": "r1"}}))
        .await;
    let ManagementServerMessage::Exit { message, reason } = session.next().await else {
        panic!("connected without a subsystem");
    };
    assert_eq!(reason, ExitReason::Failed);
    assert!(
        message
            .unwrap()
            .starts_with("The SSH server has no NETCONF subsystem.")
    );
}

#[tokio::test]
async fn waits_for_the_connection_before_running_requests() {
    let port = gnmi_target().await;
    let session = Session::start(Arc::new(TestEnv::default()));
    session
        .send(json!({"op": "connect", "profile": {
            "kind": "gnmi", "host": "127.0.0.1", "port": port, "username": "admin", "tls": "plaintext"
        }}))
        .await;
    // Sent while the login prompt is still open: it runs once the session is up.
    let request: ManagementClientMessage = serde_json::from_value(json!({
        "op": "gnmi-get", "id": "early", "paths": ["/system/name"], "type": GnmiDataType::State
    }))
    .unwrap();
    session.input.send(request).await.unwrap();
    let ManagementServerMessage::AuthPrompt(_) = session.next().await else {
        panic!("no login prompt");
    };
    session
        .send(
            serde_json::to_value(muxus_core::protocol::ManagementClientMessage::AuthResponse(
                AuthPromptResponse {
                    answers: vec!["secret".into()],
                    remember_password: None,
                    skipped: None,
                },
            ))
            .unwrap(),
        )
        .await;
    assert!(matches!(
        session.next().await,
        ManagementServerMessage::Ready { .. }
    ));
    let ManagementStreamMessage::Result { id, .. } = session.stream().await else {
        panic!("the early request did not run");
    };
    assert_eq!(id, "early");
}

/// A gNMI target behind TLS with a self-signed certificate for `localhost`.
async fn tls_target() -> u16 {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certified.cert.der().clone()], key)
    .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(socket).await else {
                    return;
                };
                common::serve(tls, |mut call| async move {
                    if call.path != "/gnmi.gNMI/Capabilities" {
                        call.reply(&[], code::UNIMPLEMENTED, None);
                        return;
                    }
                    call.ended().await;
                    let response = gnmi::CapabilityResponse {
                        g_nmi_version: "0.10.0".into(),
                        ..Default::default()
                    };
                    call.reply(&[&response.encode_to_vec()], 0, None);
                })
                .await;
            });
        }
    });
    port
}

#[tokio::test]
async fn shows_an_untrusted_certificate_once_and_pins_it() {
    let port = tls_target().await;
    let env = Arc::new(TestEnv::default());
    let session = Session::start(env.clone());
    session
        .send(json!({"op": "connect", "profile": {"kind": "gnmi", "host": "localhost", "port": port}}))
        .await;
    let ManagementServerMessage::Certificate(challenge) = session.next().await else {
        panic!("no certificate challenge");
    };
    assert_eq!(challenge.port, port);
    session
        .send(json!({"op": "certificate-response", "accept": true}))
        .await;
    let ManagementServerMessage::AuthPrompt(prompt) = session.next().await else {
        panic!("no login prompt");
    };
    assert_eq!(prompt.prompts.len(), 2);
    assert_eq!(
        prompt.skip_label.as_deref(),
        Some("Connect without a login")
    );
    session
        .send(json!({"op": "auth-response", "answers": [], "skipped": true}))
        .await;
    let ManagementServerMessage::Ready { info, .. } = session.next().await else {
        panic!("not ready");
    };
    let ManagementSessionInfo::Gnmi(info) = info else {
        panic!("not a gNMI session");
    };
    assert_eq!(info.tls.pinned, Some(true));
    assert_eq!(info.tls.verified, Some(false));
    assert_eq!(
        info.tls.fingerprint.as_deref(),
        Some(challenge.fingerprint.as_str())
    );
    assert_eq!(info.address, format!("localhost:{port}"));
    assert_eq!(
        env.trusted.lock().unwrap()[0].fingerprint,
        challenge.fingerprint
    );

    session
        .send(json!({"op": "tls-probe", "id": "probe"}))
        .await;
    let ManagementStreamMessage::Result { result, .. } = session.stream().await else {
        panic!("no probe result");
    };
    let ManagementResult::TlsProbe { certificate } = result else {
        panic!("not a probe result");
    };
    assert_eq!(certificate.fingerprint, challenge.fingerprint);
    assert_eq!(
        certificate.verification_error.as_deref(),
        Some("DEPTH_ZERO_SELF_SIGNED_CERT")
    );

    // The next connection to the same device does not ask again.
    let session = Session::start(env);
    session
        .send(json!({"op": "connect", "profile": {"kind": "gnmi", "host": "localhost", "port": port}}))
        .await;
    assert!(matches!(
        session.next().await,
        ManagementServerMessage::AuthPrompt(_)
    ));
}

/// A gNMI target without logins that stores uploads and serves them back.
async fn file_target() -> u16 {
    use muxus_mgmt::services::json::{decode_json, encode_json};
    let stored: Arc<Mutex<Vec<u8>>> = Arc::default();
    common::listen(move |mut call| {
        let stored = stored.clone();
        async move {
            let message = |name: &str| muxus_mgmt::proto::pool().get_message_by_name(name).unwrap();
            match call.path.as_str() {
                "/gnmi.gNMI/Capabilities" => {
                    call.ended().await;
                    let response = gnmi::CapabilityResponse {
                        g_nmi_version: "0.10.0".into(),
                        ..Default::default()
                    };
                    call.reply(&[&response.encode_to_vec()], 0, None);
                }
                "/gnoi.file.File/Put" => {
                    let mut content = Vec::new();
                    while let Some(request) = call.next_request().await {
                        let request = decode_json(&message("gnoi.file.PutRequest"), &request).unwrap();
                        if let Some(contents) = request.get("contents").and_then(|value| value.as_str()) {
                            use base64::Engine as _;
                            content.extend(base64::engine::general_purpose::STANDARD.decode(contents).unwrap());
                        }
                    }
                    *stored.lock().unwrap() = content;
                    let reply = encode_json(&message("gnoi.file.PutResponse"), &json!({})).unwrap();
                    call.reply(&[&reply], 0, None);
                }
                "/gnoi.file.File/Get" => {
                    call.ended().await;
                    let content = stored.lock().unwrap().clone();
                    use base64::Engine as _;
                    let reply = encode_json(
                        &message("gnoi.file.GetResponse"),
                        &json!({"contents": base64::engine::general_purpose::STANDARD.encode(&content)}),
                    )
                    .unwrap();
                    call.reply(&[&reply], 0, None);
                }
                _ => call.reply(&[], code::UNIMPLEMENTED, None),
            }
        }
    })
    .await
}

#[tokio::test]
async fn uploads_and_downloads_files_through_the_ui_connection() {
    use muxus_mgmt::workbench::connection::{ConnectionEvent, ManagementConnection};
    let port = file_target().await;
    let (input, output) = ManagementSession::start(Arc::new(TestEnv::default()));
    let profile: ManagementProfile = serde_json::from_value(json!({
        "kind": "gnmi", "host": "127.0.0.1", "port": port, "tls": "plaintext", "username": "admin"
    }))
    .unwrap();
    let connection = ManagementConnection::new(input, profile);
    let (events_tx, events) = async_channel::unbounded();
    tokio::spawn(connection.clone().dispatch(output, events_tx));
    loop {
        match events.recv().await.unwrap() {
            ConnectionEvent::AuthPrompt(_) => connection.send(
                serde_json::from_value(json!({"op": "auth-response", "answers": ["pw"]})).unwrap(),
            ),
            ConnectionEvent::Ready { .. } => break,
            ConnectionEvent::Closed { exit } => panic!("closed: {exit:?}"),
            _ => {}
        }
    }
    let content: Vec<u8> = (0..3_000_000u32).map(|index| (index % 251) as u8).collect();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let seen = progress.clone();
    let (upload, _cancel) =
        connection.upload_file("/tmp/big.bin", content.clone(), Some(644), move |bytes| {
            seen.lock().unwrap().push(bytes)
        });
    assert_eq!(upload.await.unwrap(), content.len() as u64);
    // One acknowledgement before the first chunk and one after each chunk but the last.
    assert_eq!(*progress.lock().unwrap(), vec![0, 1_048_576, 2_097_152]);
    let (download, _cancel) = connection.download_file("/tmp/big.bin", |_| {});
    let file = download.await.unwrap();
    assert_eq!(file.size, content.len() as u64);
    assert_eq!(file.data, content);
    assert!(!file.verified);
    connection.close();
    loop {
        if let ConnectionEvent::Closed { .. } = events.recv().await.unwrap() {
            break;
        }
    }
}
