//! The gNMI explorer against fake devices that answer like SR OS and SR
//! Linux (port of `tests/unit/client/management-explorer.test.ts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::gnmi_path::{format_gnmi_path, parse_gnmi_path, strip_module_prefix};
use muxus_core::management::{
    GnmiEncoding, GnmiNotification, GnmiSessionInfo, GnmiTlsMode, GnmiUpdate, GnmiValue,
    GnmiValueType, ManagementError, ManagementProtocol, ManagementResult, ManagementSessionInfo,
    ManagementTlsInfo,
};
use muxus_core::protocol::ManagementProfile;
use muxus_mgmt::workbench::connection::{
    LiveSubscription, ManagementRequestError, PendingRequest, RequestMessage, RequestOutcome,
    SubscribeMessage, SubscriptionUpdate, WorkbenchTransport,
};
use muxus_mgmt::workbench::controller::WorkbenchController;
use muxus_mgmt::workbench::data_tree::{DataNode, node_label};
use muxus_mgmt::workbench::store::WorkbenchStore;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

fn is_leaf(value: &Value) -> bool {
    match value {
        Value::Object(_) => false,
        Value::Array(items) => items
            .iter()
            .all(|item| !item.is_object() && !item.is_array()),
        _ => true,
    }
}

/// The gNMI depth extension as specified: `depth` levels below the path, the
/// last one carrying leaves and leaf-lists only. A container with nothing left
/// in reach is left out. A list and its entries are one level.
fn prune(value: &Map<String, Value>, depth: u32) -> Map<String, Value> {
    if depth == 0 {
        return value.clone();
    }
    let mut out = Map::new();
    for (name, child) in value {
        if is_leaf(child) {
            out.insert(name.clone(), child.clone());
        } else if depth > 1 && child.is_array() {
            let entries: Vec<Value> = child
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| prune(entry.as_object().unwrap(), depth - 1))
                .filter(|entry| !entry.is_empty())
                .map(Value::Object)
                .collect();
            if !entries.is_empty() {
                out.insert(name.clone(), Value::Array(entries));
            }
        } else if depth > 1 {
            let pruned = prune(child.as_object().unwrap(), depth - 1);
            if !pruned.is_empty() {
                out.insert(name.clone(), Value::Object(pruned));
            }
        }
    }
    out
}

fn failure(message: &str, code: Option<&str>) -> ManagementRequestError {
    ManagementRequestError {
        error: ManagementError {
            message: message.into(),
            code: code.map(str::to_owned),
            cancelled: None,
        },
        duration_ms: 0,
    }
}

/// A device that answers Gets from `data`, rejecting module names in paths as
/// SR OS does. With `split_root`, the top level comes as one update per branch,
/// also like SR OS.
struct FakeDevice {
    data: Value,
    split_root: bool,
    requests: Mutex<Vec<(String, Option<u32>)>>,
}

impl FakeDevice {
    fn value_at(&self, path: &str) -> Result<Value, ManagementRequestError> {
        let mut value = self.data.clone();
        for elem in parse_gnmi_path(path).unwrap().elems {
            if elem.name.contains(':') {
                return Err(failure(&format!("{} - Unknown element", elem.name), None));
            }
            let object = value.as_object().cloned().unwrap_or_default();
            let member = object
                .keys()
                .find(|name| strip_module_prefix(name) == elem.name)
                .cloned();
            let mut next = member.and_then(|member| object.get(&member).cloned());
            if let (Some(keys), Some(Value::Array(entries))) = (&elem.keys, &next) {
                next = entries
                    .iter()
                    .find(|entry| {
                        keys.iter().all(|(key, wanted)| {
                            entry.get(key).map(|value| match value {
                                Value::String(text) => text.clone(),
                                other => other.to_string(),
                            }) == Some(wanted.to_owned())
                        })
                    })
                    .cloned();
            }
            value = next.ok_or_else(|| failure(&format!("{path} - Unknown element"), None))?;
        }
        Ok(value)
    }

    fn get(&self, path: &str, depth: u32) -> Result<Vec<GnmiNotification>, ManagementRequestError> {
        let value = self.value_at(path)?;
        let object = value.as_object().cloned().unwrap_or_default();
        let updates: Vec<(String, Map<String, Value>)> = if path == "/" && self.split_root {
            object
                .iter()
                .map(|(name, top)| {
                    (
                        format!("/{name}"),
                        prune(top.as_object().unwrap(), depth.saturating_sub(1)),
                    )
                })
                .collect()
        } else {
            vec![(path.to_owned(), prune(&object, depth))]
        };
        let shown: Vec<GnmiUpdate> = updates
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(path, value)| GnmiUpdate {
                path,
                value: GnmiValue {
                    kind: GnmiValueType::JsonIetf,
                    value: Value::Object(value),
                },
                duplicates: None,
            })
            .collect();
        Ok(vec![GnmiNotification {
            timestamp: "1".into(),
            prefix: None,
            atomic: None,
            updates: if shown.is_empty() {
                vec![GnmiUpdate {
                    path: path.to_owned(),
                    value: GnmiValue {
                        kind: GnmiValueType::None,
                        value: Value::Null,
                    },
                    duplicates: None,
                }]
            } else {
                shown
            },
            deletes: vec![],
        }])
    }

    fn requests(&self) -> Vec<(String, Option<u32>)> {
        self.requests.lock().unwrap().clone()
    }
}

impl WorkbenchTransport for FakeDevice {
    fn is_closed(&self) -> bool {
        false
    }

    fn request(&self, message: RequestMessage) -> PendingRequest {
        let RequestMessage::GnmiGet {
            paths,
            depth,
            max_bytes,
            ..
        } = message
        else {
            panic!("the explorer only sends Gets");
        };
        let path = paths[0].clone();
        self.requests.lock().unwrap().push((path.clone(), depth));
        let result = self
            .get(&path, depth.unwrap_or(0))
            .and_then(|notifications| {
                let bytes = serde_json::to_string(&notifications).unwrap().len() as u64;
                if max_bytes.is_some_and(|max| bytes > max) {
                    return Err(failure("too large", Some("RESOURCE_EXHAUSTED")));
                }
                Ok(RequestOutcome {
                    result: ManagementResult::GnmiGet { notifications },
                    duration_ms: 1,
                    bytes,
                })
            });
        PendingRequest {
            id: "r".into(),
            outcome: Box::pin(async move {
                tokio::task::yield_now().await;
                result
            }),
            cancel: Arc::new(|| {}),
        }
    }

    fn subscribe(&self, _message: SubscribeMessage) -> LiveSubscription {
        let (sender, updates) = mpsc::unbounded_channel();
        let _ = sender.send(SubscriptionUpdate::Sync);
        LiveSubscription {
            id: "s".into(),
            updates,
            poll: Arc::new(|| {}),
            cancel: Arc::new(|| {}),
        }
    }
}

struct Explorer {
    controller: Arc<WorkbenchController<FakeDevice>>,
    device: Arc<FakeDevice>,
}

impl Explorer {
    fn new(data: Value, split_root: bool) -> Self {
        let device = Arc::new(FakeDevice {
            data,
            split_root,
            requests: Mutex::default(),
        });
        let info = ManagementSessionInfo::Gnmi(GnmiSessionInfo {
            address: "admin@sr1:57400".into(),
            via: None,
            connected_at: String::new(),
            version: "0.8.0".into(),
            encodings: vec![GnmiEncoding::JsonIetf],
            models: vec![],
            tls: ManagementTlsInfo {
                mode: GnmiTlsMode::Plaintext,
                verified: None,
                pinned: None,
                fingerprint: None,
                subject: None,
                issuer: None,
                valid_to: None,
                protocol: None,
            },
            services: None,
        });
        let profile: ManagementProfile =
            serde_json::from_value(json!({"kind": "gnmi", "host": "sr1", "port": 57400})).unwrap();
        let controller = WorkbenchController::new(
            WorkbenchStore::new(ManagementProtocol::Gnmi),
            Some(device.clone()),
            profile,
            Some(info),
            tokio::runtime::Handle::current(),
        );
        Self { controller, device }
    }

    fn root(&self) -> DataNode {
        self.controller
            .store
            .read(|state| state.explorer.root.clone())
    }
}

fn child(node: &DataNode, label: &str) -> DataNode {
    node.children
        .iter()
        .find(|candidate| node_label(candidate) == label)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "{} has no {label}: {}",
                node_label(node),
                labels(node).join(", ")
            )
        })
}

fn labels(node: &DataNode) -> Vec<String> {
    node.children.iter().map(node_label).collect()
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(condition(), "condition never held");
}

#[tokio::test]
async fn looks_deeper_when_the_depth_extension_hides_the_top_level_as_on_sr_os() {
    let explorer = Explorer::new(
        json!({
            "configure": {
                "nokia-conf:system": {"name": "sr1"},
                "nokia-conf:log": {"filter": [{"filter-name": "1001", "named-entry": [{"entry-name": "10"}]}]}
            },
            "state": {
                "nokia-state:system": {"oper-name": "sr1"},
                "nokia-state:service": {"vprn": [{"service-name": "blue", "oper-state": "up"}]}
            }
        }),
        true,
    );
    explorer.controller.load_node("").await;
    // Depth 2 shows nothing at all; the explorer does not stop there.
    assert_eq!(
        explorer.device.requests()[..2],
        [("/".to_owned(), Some(2)), ("/".to_owned(), Some(3))]
    );
    assert_eq!(labels(&explorer.root()), vec!["configure", "state"]);
    // The branches without values near the top arrive once the whole tree is in.
    wait_for(|| labels(&child(&explorer.root(), "state")) == vec!["system", "service"]).await;
    assert_eq!(
        labels(&child(&explorer.root(), "configure")),
        vec!["system", "log"]
    );
    assert_eq!(
        explorer.device.requests().last(),
        Some(&("/".to_owned(), None))
    );
    assert!(child(&child(&explorer.root(), "state"), "service").loaded);
    // Paths built from the tree name no module, which SR OS would reject.
    assert_eq!(
        format_gnmi_path(&child(&child(&explorer.root(), "configure"), "system").path),
        "/configure/system"
    );
}

#[tokio::test]
async fn adds_the_branches_a_depth_limited_look_left_out_below_a_node() {
    let explorer = Explorer::new(
        json!({
            "network-instance": [{
                "name": "mgmt",
                "type": "ip-vrf",
                "protocols": {"bgp": {"admin-state": "enable"}},
                "route-table": {"ipv4": {"route": [{"prefix": "0.0.0.0/0"}]}}
            }]
        }),
        false,
    );
    explorer.controller.load_node("").await;
    let entry = child(&child(&explorer.root(), "network-instance"), "mgmt");
    explorer.controller.load_node(&entry.id).await;
    let entry_now = || child(&child(&explorer.root(), "network-instance"), "mgmt");
    assert_eq!(labels(&entry_now()), vec!["name", "type"]);
    wait_for(|| labels(&entry_now()) == vec!["name", "type", "protocols", "route-table"]).await;
    assert_eq!(
        format_gnmi_path(&child(&entry_now(), "protocols").path),
        "/network-instance[name=mgmt]/protocols"
    );
}

#[tokio::test]
async fn keeps_what_it_shows_when_a_deeper_look_grows_too_large() {
    let route: Vec<Value> = (0..4000)
        .map(|index| {
            json!({"prefix": format!("10.{}.{}.0/24", index >> 8, index & 255), "path": "x".repeat(1200)})
        })
        .collect();
    let explorer = Explorer::new(
        json!({"system": {"name": "r1", "rib": {"route-count": 4000, "route": route}}}),
        false,
    );
    explorer.controller.load_node("").await;
    let system = child(&explorer.root(), "system");
    explorer.controller.load_node(&system.id).await;
    let system_now = || child(&explorer.root(), "system");
    assert_eq!(labels(&system_now()), vec!["name", "rib"]);
    // The routes are past the limit: that look gives up, and the first one stays.
    wait_for(|| {
        explorer
            .device
            .requests()
            .contains(&("/system".to_owned(), Some(3)))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(labels(&system_now()), vec!["name", "rib"]);
    assert!(!child(&system_now(), "rib").loaded);
    assert!(
        !explorer
            .device
            .requests()
            .contains(&("/system".to_owned(), None))
    );
}

#[tokio::test]
async fn browses_whole_subtrees_when_the_device_rejects_the_depth_extension() {
    struct NoDepth(FakeDevice);
    impl WorkbenchTransport for NoDepth {
        fn is_closed(&self) -> bool {
            false
        }
        fn request(&self, message: RequestMessage) -> PendingRequest {
            if let RequestMessage::GnmiGet { depth: Some(_), .. } = &message {
                return PendingRequest {
                    id: "r".into(),
                    outcome: Box::pin(async {
                        Err(failure(
                            "depth extension not supported",
                            Some("UNIMPLEMENTED"),
                        ))
                    }),
                    cancel: Arc::new(|| {}),
                };
            }
            self.0.request(message)
        }
        fn subscribe(&self, message: SubscribeMessage) -> LiveSubscription {
            self.0.subscribe(message)
        }
    }
    let device = Arc::new(NoDepth(FakeDevice {
        data: json!({"system": {"name": "r1", "clock": {"timezone": "UTC"}}}),
        split_root: false,
        requests: Mutex::default(),
    }));
    let profile: ManagementProfile =
        serde_json::from_value(json!({"kind": "gnmi", "host": "r1"})).unwrap();
    let info: ManagementSessionInfo = serde_json::from_value(json!({
        "protocol": "gnmi", "address": "r1:57400", "connectedAt": "", "version": "0.8.0",
        "encodings": ["json_ietf"], "models": [], "tls": {"mode": "plaintext"}
    }))
    .unwrap();
    let controller = WorkbenchController::new(
        WorkbenchStore::new(ManagementProtocol::Gnmi),
        Some(device.clone()),
        profile,
        Some(info),
        tokio::runtime::Handle::current(),
    );
    controller.load_node("").await;
    controller.store.read(|state| {
        assert!(state.explorer.no_depth);
        let system = child(&state.explorer.root, "system");
        assert!(system.loaded);
        assert_eq!(labels(&system), vec!["name", "clock"]);
    });
}
