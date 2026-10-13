//! Checks against a real device; ignored unless asked for:
//!
//! ```text
//! MUXUS_MGMT_LIVE_HOST=172.20.20.2 MUXUS_MGMT_LIVE_PASSWORD='…' \
//!   cargo test -p muxus-mgmt --test live_device -- --ignored --nocapture
//! ```
//!
//! `MUXUS_MGMT_LIVE_PORT` (57400) and `MUXUS_MGMT_LIVE_USER` (admin) are
//! optional. The device must serve gNMI over TLS.

use std::sync::Arc;
use std::time::Duration;

use muxus_core::management::{
    GnmiDataType, GnmiEncoding, GnmiSubscriptionListMode, GnmiSubscriptionMode, ManagementProtocol,
    ManagementSessionInfo,
};
use muxus_core::protocol::{GnmiSubscription, ManagementProfile};
use muxus_mgmt::dial::connect_tcp;
use muxus_mgmt::gnmi::{GnmiClient, GnmiGetRequest, GnmiSubscribeRequest, SubscriptionEvent};
use muxus_mgmt::services::json::decode_json;
use muxus_mgmt::services::method_def;
use muxus_mgmt::services::transfer::list_services;
use muxus_mgmt::session::{ManagementEnv, ManagementSession};
use muxus_mgmt::tls::{TlsFiles, upgrade_gnmi_tls};
use muxus_mgmt::workbench::connection::{ConnectionEvent, ManagementConnection};
use muxus_mgmt::workbench::controller::WorkbenchController;
use muxus_mgmt::workbench::data_tree::node_label;
use muxus_mgmt::workbench::store::WorkbenchStore;
use serde_json::json;

struct Target {
    host: String,
    port: u16,
    user: String,
    password: String,
}

fn target() -> Target {
    Target {
        host: std::env::var("MUXUS_MGMT_LIVE_HOST").expect("MUXUS_MGMT_LIVE_HOST"),
        port: std::env::var("MUXUS_MGMT_LIVE_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(57400),
        user: std::env::var("MUXUS_MGMT_LIVE_USER").unwrap_or_else(|_| "admin".into()),
        password: std::env::var("MUXUS_MGMT_LIVE_PASSWORD").expect("MUXUS_MGMT_LIVE_PASSWORD"),
    }
}

fn profile(target: &Target) -> ManagementProfile {
    serde_json::from_value(json!({
        "kind": "gnmi", "host": target.host, "port": target.port,
        "username": target.user, "tls": "skip-verify"
    }))
    .unwrap()
}

#[tokio::test]
#[ignore = "needs a device"]
async fn talks_gnmi_and_gnoi_to_a_real_device() {
    let target = target();
    let ManagementProfile::Gnmi(gnmi_profile) = profile(&target) else {
        unreachable!()
    };
    let socket = connect_tcp(&target.host, target.port).await.unwrap();
    let tls = upgrade_gnmi_tls(socket, &gnmi_profile, &TlsFiles::default())
        .await
        .unwrap();
    let certificate = tls.certificate.clone().unwrap();
    println!(
        "TLS {:?}: {} (issuer {}), verification: {:?}",
        tls.protocol, certificate.subject, certificate.issuer, certificate.verification_error
    );
    let client = GnmiClient::connect(
        tls.stream,
        &format!("{}:{}", target.host, target.port),
        true,
    )
    .await
    .unwrap();
    client.set_credentials(Some(&target.user), Some(&target.password));
    let capabilities = client.capabilities(None).await.unwrap();
    println!(
        "gNMI {} with {} models, encodings {:?}",
        capabilities.version,
        capabilities.models.len(),
        capabilities.encodings
    );
    println!("services: {:?}", list_services(&client).await);

    let name = client
        .get(
            &GnmiGetRequest {
                paths: vec!["/system/name/host-name".into()],
                data_type: GnmiDataType::Config,
                ..Default::default()
            },
            GnmiEncoding::JsonIetf,
            None,
        )
        .await
        .unwrap();
    println!("host name: {:?}", name.notifications[0].updates);

    // Depth 2 at the top: SR Linux shows only leaves at the last level.
    let shallow = client
        .get(
            &GnmiGetRequest {
                paths: vec!["/".into()],
                depth: Some(2),
                max_bytes: Some(24 * 1024 * 1024),
                ..Default::default()
            },
            GnmiEncoding::JsonIetf,
            None,
        )
        .await
        .unwrap();
    println!("depth 2 at /: {} bytes", shallow.bytes);

    let mut sample = client
        .subscribe(
            &GnmiSubscribeRequest {
                prefix: None,
                mode: GnmiSubscriptionListMode::Stream,
                updates_only: false,
                subscriptions: vec![GnmiSubscription {
                    path: "/interface[name=mgmt0]/statistics/in-octets".into(),
                    mode: GnmiSubscriptionMode::Sample,
                    sample_interval_ms: Some(1000),
                    suppress_redundant: None,
                    heartbeat_interval_ms: None,
                }],
            },
            GnmiEncoding::JsonIetf,
        )
        .unwrap();
    let control = sample.control();
    let mut updates = 0;
    let started = std::time::Instant::now();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(10), sample.next())
        .await
        .unwrap()
    {
        match event {
            SubscriptionEvent::Notification(notification, bytes) => {
                updates += 1;
                println!(
                    "sample after {:?}: {} = {} ({bytes} bytes)",
                    started.elapsed(),
                    notification.updates[0].path,
                    notification.updates[0].value.value
                );
                if updates == 3 {
                    control.cancel();
                }
            }
            SubscriptionEvent::Sync => println!("sync after {:?}", started.elapsed()),
            SubscriptionEvent::End(error) => {
                println!("ended: {error:?}");
                break;
            }
        }
    }
    assert!(updates >= 3);

    let time = method_def("/gnoi.system.System/Time").unwrap();
    match client
        .unary_call(time.path, bytes::Bytes::new(), None)
        .await
    {
        Ok((reply, _)) => println!("gNOI time: {:?}", decode_json(&time.response, &reply)),
        Err(err) => println!("gNOI time: {} {}", err.code_name(), err.message),
    }
    client.close();
}

struct NoStore;
impl ManagementEnv for NoStore {}

#[tokio::test]
#[ignore = "needs a device"]
async fn browses_a_real_device_through_a_session() {
    let target = target();
    let (input, output) = ManagementSession::start(Arc::new(NoStore));
    let connection = ManagementConnection::new(input, profile(&target));
    let (events_tx, events) = async_channel::unbounded();
    tokio::spawn(connection.clone().dispatch(output, events_tx));
    let info = loop {
        match events.recv().await.unwrap() {
            ConnectionEvent::Status { message, .. } => println!("status: {message}"),
            ConnectionEvent::AuthPrompt(_) => connection.send(
                serde_json::from_value(
                    json!({"op": "auth-response", "answers": [target.password]}),
                )
                .unwrap(),
            ),
            ConnectionEvent::Ready { info, .. } => break info,
            other => panic!("unexpected {other:?}"),
        }
    };
    let ManagementSessionInfo::Gnmi(gnmi) = &info else {
        panic!("not gNMI");
    };
    println!("ready: {} via TLS {:?}", gnmi.address, gnmi.tls);
    let controller = WorkbenchController::new(
        WorkbenchStore::new(ManagementProtocol::Gnmi),
        Some(Arc::new(connection.clone())),
        profile(&target),
        Some(info),
        tokio::runtime::Handle::current(),
    );
    controller.load_node("").await;
    let top = controller.store.read(|state| {
        assert!(
            state.explorer.errors.is_empty(),
            "{:?}",
            state.explorer.errors
        );
        state
            .explorer
            .root
            .children
            .iter()
            .map(node_label)
            .collect::<Vec<_>>()
    });
    println!("top level: {top:?}");
    assert!(!top.is_empty());
    let interface = controller.store.read(|state| {
        state
            .explorer
            .root
            .children
            .iter()
            .find(|child| node_label(child) == "interface")
            .map(|child| child.id.clone())
    });
    if let Some(interface) = interface {
        let entry = controller.store.read(|state| {
            muxus_mgmt::workbench::data_tree::find_node(&state.explorer.root, &interface)
                .and_then(|list| list.children.first())
                .map(|entry| (entry.id.clone(), node_label(entry)))
        });
        if let Some((entry, label)) = entry {
            controller.load_node(&entry).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            let children = controller.store.read(|state| {
                muxus_mgmt::workbench::data_tree::find_node(&state.explorer.root, &entry)
                    .map(|node| node.children.iter().map(node_label).collect::<Vec<_>>())
                    .unwrap_or_default()
            });
            println!("interface {label}: {children:?}");
        }
    }
    connection.close();
}
