//! The workbench controller on a NETCONF session: history, locks, the
//! candidate check and the explorer (the NETCONF half of
//! `client/src/management/controller.ts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::management::{ManagementProtocol, ManagementResult, ManagementSessionInfo};
use muxus_core::protocol::ManagementProfile;
use muxus_mgmt::workbench::connection::{
    LiveSubscription, PendingRequest, RequestMessage, RequestOutcome, SubscribeMessage,
    WorkbenchTransport,
};
use muxus_mgmt::workbench::controller::WorkbenchController;
use muxus_mgmt::workbench::data_tree::{DataNodeKind, node_label};
use muxus_mgmt::workbench::requests::{
    Datastore, ManagementDraft, NetconfDraft, NetconfOperation, default_netconf_draft,
};
use muxus_mgmt::workbench::store::{RunStatus, WorkbenchStore};
use serde_json::json;

/// A NETCONF device whose candidate differs from running.
#[derive(Default)]
struct Device {
    rpcs: Mutex<Vec<String>>,
}

const RUNNING: &str = "<interface xmlns=\"urn:if\"><name>e1</name><mtu>1500</mtu></interface><interface xmlns=\"urn:if\"><name>e2</name><mtu>1500</mtu></interface>";
const CANDIDATE: &str = "<interface xmlns=\"urn:if\"><name>e1</name><mtu>9000</mtu></interface>";

impl WorkbenchTransport for Device {
    fn is_closed(&self) -> bool {
        false
    }

    fn request(&self, message: RequestMessage) -> PendingRequest {
        let RequestMessage::NetconfRpc { xml, .. } = message else {
            panic!("only RPCs here");
        };
        self.rpcs.lock().unwrap().push(xml.clone());
        let body = if xml.contains("<get-config><source><candidate/>") {
            format!("<data>{CANDIDATE}</data>")
        } else if xml.contains("<get-config>") {
            format!("<data>{RUNNING}</data>")
        } else {
            "<ok/>".to_owned()
        };
        let reply = format!(
            "<rpc-reply message-id=\"1\" xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\">{body}</rpc-reply>"
        );
        PendingRequest {
            id: "r".into(),
            outcome: Box::pin(async move {
                Ok(RequestOutcome {
                    result: ManagementResult::NetconfRpc {
                        xml: reply,
                        message_id: "1".into(),
                    },
                    duration_ms: 1,
                    bytes: 10,
                })
            }),
            cancel: Arc::new(|| {}),
        }
    }

    fn subscribe(&self, _message: SubscribeMessage) -> LiveSubscription {
        panic!("NETCONF has no subscriptions");
    }
}

fn controller(device: Arc<Device>) -> Arc<WorkbenchController<Device>> {
    let profile: ManagementProfile =
        serde_json::from_value(json!({"kind": "netconf", "host": "r1"})).unwrap();
    let info: ManagementSessionInfo = serde_json::from_value(json!({
        "protocol": "netconf", "address": "admin@r1:830", "connectedAt": "", "sessionId": "4",
        "base": "1.1", "capabilities": ["urn:ietf:params:netconf:capability:candidate:1.0"]
    }))
    .unwrap();
    WorkbenchController::new(
        WorkbenchStore::new(ManagementProtocol::Netconf),
        Some(device),
        profile,
        Some(info),
        tokio::runtime::Handle::current(),
    )
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

#[tokio::test]
async fn records_runs_tracks_locks_and_checks_the_candidate() {
    let device = Arc::new(Device::default());
    let controller = controller(device.clone());
    let lock = NetconfDraft {
        operation: NetconfOperation::Lock,
        target: Datastore::Candidate,
        ..default_netconf_draft()
    };
    let record = controller
        .run(Some(ManagementDraft::Netconf(lock)), false)
        .unwrap();
    assert_eq!(record.label, "lock");
    assert_eq!(record.summary, "candidate");
    settle().await;
    controller.store.read(|state| {
        assert_eq!(state.locks, vec!["candidate"]);
        assert_eq!(state.run(&record.id).unwrap().status, RunStatus::Ok);
        assert_eq!(state.selected_run_id.as_deref(), Some(record.id.as_str()));
    });

    let edit = NetconfDraft {
        operation: NetconfOperation::EditConfig,
        target: Datastore::Candidate,
        config: "<interface xmlns=\"urn:if\"><name>e1</name><mtu>9000</mtu></interface>".into(),
        ..default_netconf_draft()
    };
    controller.run(Some(ManagementDraft::Netconf(edit)), false);
    settle().await;
    controller.store.read(|state| {
        let candidate = state.candidate.clone().unwrap();
        assert!(candidate.dirty);
        assert!(!candidate.checking);
    });

    let commit = NetconfDraft {
        operation: NetconfOperation::Commit,
        confirmed: true,
        confirm_timeout: 60.0,
        ..default_netconf_draft()
    };
    controller.run(Some(ManagementDraft::Netconf(commit)), false);
    settle().await;
    assert!(
        controller
            .store
            .read(|state| state.confirm_deadline.is_some())
    );
    let rpcs = device.rpcs.lock().unwrap().clone();
    assert!(rpcs.iter().any(|rpc| {
        rpc.contains("<commit><confirmed/><confirm-timeout>60</confirm-timeout></commit>")
    }));
}

#[tokio::test]
async fn loads_the_whole_configuration_into_the_explorer() {
    let device = Arc::new(Device::default());
    let controller = controller(device.clone());
    controller.load_node("").await;
    controller.store.read(|state| {
        let root = &state.explorer.root;
        assert!(root.loaded);
        let list = &root.children[0];
        assert_eq!(list.kind, DataNodeKind::List);
        assert_eq!(
            list.children.iter().map(node_label).collect::<Vec<_>>(),
            vec!["e1", "e2"]
        );
        // With configuration loaded whole, every node is already complete.
        assert!(list.children.iter().all(|entry| entry.loaded));
        assert_eq!(list.namespace.as_deref(), Some("urn:if"));
    });
    assert!(
        device.rpcs.lock().unwrap()[0]
            .contains("<get-config><source><running/></source></get-config>")
    );
    // A detached controller keeps its history but runs nothing.
    controller.detach();
    assert!(
        controller
            .run(
                Some(ManagementDraft::Netconf(default_netconf_draft())),
                false
            )
            .is_none()
    );
}
