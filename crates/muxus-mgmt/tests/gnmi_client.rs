//! The gNMI client against an in-process tonic gNMI server (port of
//! `tests/unit/server/gnmi-client.test.ts`).

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use muxus_core::management::{
    GnmiDataType, GnmiEncoding, GnmiModel, GnmiSetEncoding, GnmiSetOp, GnmiSetResult,
    GnmiSubscriptionListMode, GnmiSubscriptionMode, GnmiValue, GnmiValueType,
};
use muxus_core::protocol::{GnmiCommit, GnmiSetValue, GnmiSubscription};
use muxus_mgmt::gnmi::{
    GnmiClient, GnmiGetRequest, GnmiSetRequest, GnmiSubscribeRequest, SubscriptionEvent,
};
use muxus_mgmt::{Error, error::code};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Response, Status, Streaming};

#[allow(clippy::all, clippy::pedantic, dead_code, deprecated)]
mod server_proto {
    pub mod gnmi {
        include!(concat!(env!("OUT_DIR"), "/test/gnmi.rs"));
    }
    pub mod gnmi_ext {
        include!(concat!(env!("OUT_DIR"), "/test/gnmi_ext.rs"));
    }
}

use server_proto::gnmi;
use server_proto::gnmi::g_nmi_server::{GNmi, GNmiServer};
use server_proto::gnmi_ext;

fn path(elems: &[(&str, &[(&str, &str)])]) -> gnmi::Path {
    gnmi::Path {
        elem: elems
            .iter()
            .map(|(name, keys)| gnmi::PathElem {
                name: (*name).into(),
                key: keys
                    .iter()
                    .map(|(key, value)| gnmi::path_elem::KeyEntry {
                        key: (*key).into(),
                        value: (*value).into(),
                    })
                    .collect(),
            })
            .collect(),
        ..Default::default()
    }
}

fn typed(value: gnmi::typed_value::Value) -> Option<gnmi::TypedValue> {
    Some(gnmi::TypedValue { value: Some(value) })
}

#[derive(Default)]
struct Seen {
    metadata: Vec<(String, String)>,
    gets: Vec<gnmi::GetRequest>,
    sets: Vec<gnmi::SetRequest>,
    subscribes: Vec<gnmi::SubscribeRequest>,
}

/// A gNMI target that answers like the TypeScript test's fake.
#[derive(Clone, Default)]
struct Target {
    seen: Arc<Mutex<Seen>>,
}

fn metadata_of<T>(request: &Request<T>) -> Vec<(String, String)> {
    request
        .metadata()
        .iter()
        .filter_map(|entry| match entry {
            tonic::metadata::KeyAndValueRef::Ascii(key, value) => Some((
                key.to_string(),
                value.to_str().unwrap_or_default().to_owned(),
            )),
            tonic::metadata::KeyAndValueRef::Binary(..) => None,
        })
        .collect()
}

type SubscribeStream =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<gnmi::SubscribeResponse, Status>> + Send>>;

#[tonic::async_trait]
impl GNmi for Target {
    async fn capabilities(
        &self,
        request: Request<gnmi::CapabilityRequest>,
    ) -> Result<Response<gnmi::CapabilityResponse>, Status> {
        self.seen.lock().unwrap().metadata = metadata_of(&request);
        Ok(Response::new(gnmi::CapabilityResponse {
            supported_models: vec![gnmi::ModelData {
                name: "openconfig-interfaces".into(),
                organization: "OpenConfig".into(),
                version: "3.0.0".into(),
            }],
            supported_encodings: vec![4, 0],
            g_nmi_version: "0.10.0".into(),
            ..Default::default()
        }))
    }

    async fn get(
        &self,
        request: Request<gnmi::GetRequest>,
    ) -> Result<Response<gnmi::GetResponse>, Status> {
        let request = request.into_inner();
        self.seen.lock().unwrap().gets.push(request.clone());
        if request
            .path
            .iter()
            .any(|path| path.elem.iter().any(|elem| elem.name == "nope"))
        {
            return Err(Status::invalid_argument(
                "Path not valid - unknown element 'nope'",
            ));
        }
        Ok(Response::new(gnmi::GetResponse {
            notification: vec![gnmi::Notification {
                timestamp: 5,
                update: vec![gnmi::Update {
                    path: Some(path(&[("system", &[]), ("name", &[])])),
                    val: typed(gnmi::typed_value::Value::JsonIetfVal(b"\"leaf1\"".to_vec())),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }))
    }

    async fn set(
        &self,
        request: Request<gnmi::SetRequest>,
    ) -> Result<Response<gnmi::SetResponse>, Status> {
        self.seen.lock().unwrap().sets.push(request.into_inner());
        Ok(Response::new(gnmi::SetResponse {
            timestamp: 7,
            response: vec![gnmi::UpdateResult {
                path: Some(path(&[
                    ("interface", &[("name", "e1")]),
                    ("description", &[]),
                ])),
                op: 3,
                ..Default::default()
            }],
            ..Default::default()
        }))
    }

    type SubscribeStream = SubscribeStream;

    async fn subscribe(
        &self,
        request: Request<Streaming<gnmi::SubscribeRequest>>,
    ) -> Result<Response<SubscribeStream>, Status> {
        let mut requests = request.into_inner();
        let first = requests
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty"))?;
        self.seen.lock().unwrap().subscribes.push(first);
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            for value in [1u64, 2] {
                let _ = tx
                    .send(Ok(gnmi::SubscribeResponse {
                        response: Some(gnmi::subscribe_response::Response::Update(
                            gnmi::Notification {
                                timestamp: value as i64,
                                update: vec![gnmi::Update {
                                    path: Some(path(&[("c", &[])])),
                                    val: typed(gnmi::typed_value::Value::UintVal(value)),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    }))
                    .await;
            }
            let _ = tx
                .send(Ok(gnmi::SubscribeResponse {
                    response: Some(gnmi::subscribe_response::Response::SyncResponse(true)),
                    ..Default::default()
                }))
                .await;
            // Stay open until the client goes away.
            tx.closed().await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn connect(target: Target) -> GnmiClient {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(GNmiServer::new(target))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap()
}

#[tokio::test]
async fn sends_credentials_as_metadata_and_reads_capabilities() {
    let target = Target::default();
    let client = connect(target.clone()).await;
    client.set_credentials(Some("admin"), Some("secret"));
    let capabilities = client.capabilities(None).await.unwrap();
    assert_eq!(capabilities.version, "0.10.0");
    assert_eq!(
        capabilities.encodings,
        vec![GnmiEncoding::JsonIetf, GnmiEncoding::Json]
    );
    assert_eq!(
        capabilities.models,
        vec![GnmiModel {
            name: "openconfig-interfaces".into(),
            organization: "OpenConfig".into(),
            version: "3.0.0".into(),
        }]
    );
    assert!(capabilities.bytes > 0);
    let metadata = target.seen.lock().unwrap().metadata.clone();
    let header = |name: &str| {
        metadata
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(header("username").as_deref(), Some("admin"));
    assert_eq!(header("password").as_deref(), Some("secret"));
    assert_eq!(header("user-agent").as_deref(), Some("muxus-gnmi"));
    assert!(header("grpc-timeout").is_some());
    client.close();
}

#[tokio::test]
async fn encodes_get_with_its_options_and_turns_grpc_errors_into_readable_failures() {
    let target = Target::default();
    let client = connect(target.clone()).await;
    let result = client
        .get(
            &GnmiGetRequest {
                paths: vec!["/system/name".into()],
                data_type: GnmiDataType::State,
                depth: Some(2),
                ..Default::default()
            },
            GnmiEncoding::JsonIetf,
            None,
        )
        .await
        .unwrap();
    let update = &result.notifications[0].updates[0];
    assert_eq!(update.path, "/system/name");
    assert_eq!(
        update.value,
        GnmiValue {
            kind: GnmiValueType::JsonIetf,
            value: "leaf1".into()
        }
    );
    let request = target.seen.lock().unwrap().gets[0].clone();
    assert_eq!(request.r#type, 2);
    assert_eq!(request.encoding, 4);
    assert_eq!(
        request.extension,
        vec![gnmi_ext::Extension {
            ext: Some(gnmi_ext::extension::Ext::Depth(gnmi_ext::Depth {
                level: 2
            }))
        }]
    );
    let failure = client
        .get(
            &GnmiGetRequest {
                paths: vec!["/nope".into()],
                ..Default::default()
            },
            GnmiEncoding::JsonIetf,
            None,
        )
        .await
        .unwrap_err();
    let Error::Grpc(failure) = failure else {
        panic!("not a gRPC failure: {failure:?}");
    };
    assert_eq!(failure.code_name(), "INVALID_ARGUMENT");
    assert_eq!(failure.message, "Path not valid - unknown element 'nope'");
    // A malformed path never reaches the device.
    let invalid = client
        .get(
            &GnmiGetRequest {
                paths: vec!["/a[b".into()],
                ..Default::default()
            },
            GnmiEncoding::JsonIetf,
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(invalid, Error::Path(_)));
    assert_eq!(target.seen.lock().unwrap().gets.len(), 2);
}

#[tokio::test]
async fn sends_set_with_the_commit_confirmed_extension() {
    let target = Target::default();
    let client = connect(target.clone()).await;
    let result = client
        .set(
            &GnmiSetRequest {
                updates: vec![GnmiSetValue {
                    path: "/interface[name=e1]/description".into(),
                    value: "\"uplink\"".into(),
                    encoding: GnmiSetEncoding::JsonIetf,
                }],
                deletes: vec!["/interface[name=e2]".into()],
                commit: Some(GnmiCommit::Commit {
                    id: "c1".into(),
                    rollback_seconds: Some(60),
                }),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.timestamp, "7");
    assert_eq!(
        result.results,
        vec![GnmiSetResult {
            path: "/interface[name=e1]/description".into(),
            op: GnmiSetOp::Update
        }]
    );
    let request = target.seen.lock().unwrap().sets[0].clone();
    assert_eq!(
        request.delete,
        vec![path(&[("interface", &[("name", "e2")])])]
    );
    assert_eq!(
        request.update[0].val,
        typed(gnmi::typed_value::Value::JsonIetfVal(
            b"\"uplink\"".to_vec()
        ))
    );
    assert_eq!(
        request.extension,
        vec![gnmi_ext::Extension {
            ext: Some(gnmi_ext::extension::Ext::Commit(gnmi_ext::Commit {
                id: "c1".into(),
                action: Some(gnmi_ext::commit::Action::Commit(gnmi_ext::CommitRequest {
                    rollback_duration: Some(prost_types::Duration {
                        seconds: 60,
                        nanos: 0
                    }),
                })),
            }))
        }]
    );
}

#[tokio::test]
async fn streams_a_subscription_until_it_is_cancelled() {
    let target = Target::default();
    let client = connect(target.clone()).await;
    let mut subscription = client
        .subscribe(
            &GnmiSubscribeRequest {
                prefix: None,
                mode: GnmiSubscriptionListMode::Stream,
                updates_only: false,
                subscriptions: vec![GnmiSubscription {
                    path: "/c".into(),
                    mode: GnmiSubscriptionMode::Sample,
                    sample_interval_ms: Some(1000),
                    suppress_redundant: None,
                    heartbeat_interval_ms: None,
                }],
            },
            GnmiEncoding::JsonIetf,
        )
        .unwrap();
    let control = subscription.control();
    let mut values = Vec::new();
    let mut total = 0;
    let ended = loop {
        match subscription.next().await.unwrap() {
            SubscriptionEvent::Notification(notification, bytes) => {
                total += bytes;
                values.push(notification.updates[0].value.value.clone());
            }
            SubscriptionEvent::Sync => control.cancel(),
            SubscriptionEvent::End(error) => break error,
        }
    };
    assert_eq!(
        ended.map(|error| error.code_name()).as_deref(),
        Some("CANCELLED")
    );
    assert_eq!(values, vec![serde_json::json!(1), serde_json::json!(2)]);
    // Messages that share a DATA frame count against the first of them.
    assert!(total > 0);
    assert!(subscription.next().await.is_none());
    let request = target.seen.lock().unwrap().subscribes[0].clone();
    let Some(gnmi::subscribe_request::Request::Subscribe(list)) = request.request else {
        panic!("not a subscribe request");
    };
    assert_eq!(list.encoding, 4);
    // STREAM is mode 0, proto3's default, so it is not on the wire at all.
    assert_eq!(list.mode, 0);
    assert_eq!(list.subscription[0].mode, 2);
    assert_eq!(list.subscription[0].sample_interval, 1_000_000_000);
}

#[tokio::test]
async fn reports_a_lost_connection_to_everything_still_running() {
    // A device that accepts the connection but never answers.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(socket).await.unwrap();
        // Keep every stream open and unanswered.
        let mut held = Vec::new();
        while let Some(Ok(stream)) = connection.accept().await {
            held.push(stream);
        }
    });
    let socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let client = GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap();
    let closed = client.closed();
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.capabilities(None).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    client.close();
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.code, code::UNAVAILABLE, "{error:?}");
    assert!(closed.await.contains("closed"));
    // Later calls fail right away.
    let later = client.capabilities(None).await.unwrap_err();
    assert_eq!(later.message, "The session is closed.");
}
