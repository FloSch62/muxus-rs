//! A fake gNMI device for the workbench example and its reference captures:
//! an SR Linux–flavoured data tree served over plain-text HTTP/2 with gRPC
//! framing. Capabilities, Get (with the depth extension and `[name=*]`
//! wildcards), Set and Subscribe (stream with samples, once, poll) work;
//! everything else answers UNIMPLEMENTED, so the workbench treats every
//! gNOI/gNSI service as offered. Every value is invented.

#![allow(dead_code, deprecated)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use h2::RecvStream;
use h2::server::SendResponse;
use http::{HeaderMap, HeaderValue, Response};
use muxus_mgmt::proto::gnmi;
use prost::Message;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// The device's data, shared by every call.
#[derive(Clone)]
pub struct Device {
    data: Arc<Mutex<Value>>,
}

const STATE_ONLY: &[&str] = &[
    "oper-state",
    "statistics",
    "current-datetime",
    "last-booted",
    "hw-mac-address",
    "serial-number",
    "uptime",
    "session-state",
    "received-messages",
];

fn models() -> Vec<gnmi::ModelData> {
    [
        ("srl_nokia-acl", "2026-03-31"),
        ("srl_nokia-bgp", "2026-03-31"),
        ("srl_nokia-interfaces", "2026-03-31"),
        ("srl_nokia-network-instance", "2026-03-31"),
        ("srl_nokia-platform", "2026-03-31"),
        ("srl_nokia-system", "2026-03-31"),
        ("srl_nokia-system-info", "2026-03-31"),
        ("srl_nokia-gnmi-server", "2026-03-31"),
        ("srl_nokia-ntp", "2026-03-31"),
        ("srl_nokia-lldp", "2026-03-31"),
        ("srl_nokia-if-ip", "2026-03-31"),
        ("srl_nokia-routing-policy", "2026-03-31"),
        ("srl_nokia-snmp", "2026-03-31"),
        ("srl_nokia-tls", "2026-03-31"),
        ("ietf-yang-library", "2019-01-04"),
    ]
    .into_iter()
    .map(|(name, version)| gnmi::ModelData {
        name: name.into(),
        organization: if name.starts_with("ietf") {
            "IETF NETCONF (Network Configuration) Working Group".into()
        } else {
            "Nokia".into()
        },
        version: version.into(),
    })
    .collect()
}

fn interface(name: &str, description: &str, up: bool, index: u64) -> Value {
    json!({
        "name": name,
        "description": description,
        "admin-state": if up || name == "mgmt0" { "enable" } else { "disable" },
        "mtu": if name == "mgmt0" { 1514 } else { 9232 },
        "oper-state": if up { "up" } else { "down" },
        "statistics": {
            "in-octets": (812_345_678 * index).to_string(),
            "out-octets": (723_456_789 * index).to_string(),
            "in-packets": (1_234_567 * index).to_string(),
            "out-packets": (1_134_567 * index).to_string(),
            "in-error-packets": "0",
            "out-error-packets": "0",
            "in-discarded-packets": (index * 3).to_string(),
        },
        "subinterface": [{
            "index": 0,
            "admin-state": "enable",
            "oper-state": if up { "up" } else { "down" },
            "ipv4": { "address": [{ "ip-prefix": format!("10.0.{index}.1/31") }] },
        }],
    })
}

fn document() -> Value {
    json!({
        "srl_nokia-acl:acl": {
            "acl-filter": [{
                "name": "cpm",
                "type": "ipv4",
                "description": "Protect the CPM",
                "entry": [
                    { "sequence-id": 10, "description": "Accept SSH", "action": { "accept": {} } },
                    { "sequence-id": 20, "description": "Accept gNMI", "action": { "accept": {} } },
                ],
            }],
        },
        "srl_nokia-interfaces:interface": [
            interface("ethernet-1/1", "to spine-01", true, 1),
            interface("ethernet-1/2", "to spine-02", true, 2),
            interface("ethernet-1/3", "to server-01", false, 3),
            interface("mgmt0", "management", true, 4),
        ],
        "srl_nokia-network-instance:network-instance": [
            {
                "name": "default",
                "type": "srl_nokia-network-instance:default",
                "admin-state": "enable",
                "oper-state": "up",
                "interface": [{ "name": "ethernet-1/1.0" }, { "name": "ethernet-1/2.0" }],
                "protocols": {
                    "srl_nokia-bgp:bgp": {
                        "autonomous-system": 65001,
                        "router-id": "10.255.0.1",
                        "neighbor": [
                            { "peer-address": "10.0.1.0", "peer-group": "spines", "session-state": "established" },
                            { "peer-address": "10.0.2.0", "peer-group": "spines", "session-state": "established" },
                        ],
                    },
                },
            },
            {
                "name": "mgmt",
                "type": "srl_nokia-network-instance:ip-vrf",
                "admin-state": "enable",
                "oper-state": "up",
                "interface": [{ "name": "mgmt0.0" }],
            },
        ],
        "srl_nokia-platform:platform": {
            "chassis": {
                "type": "7220 IXR-D2L",
                "serial-number": "SIM-0001",
                "hw-mac-address": "1A:2B:00:FF:00:00",
                "last-booted": "2026-10-12T08:01:12.400Z",
            },
        },
        "srl_nokia-system:system": {
            "information": {
                "version": "v26.3.1-71-g8d8e4a6c2a",
                "description": "SRLinux-v26.3.1 7220 IXR-D2L",
                "current-datetime": "2026-10-12T09:41:07.231Z",
                "last-booted": "2026-10-12T08:01:12.400Z",
            },
            "name": { "host-name": "leaf1", "domain-name": "lab.internal" },
            "srl_nokia-ntp:ntp": {
                "admin-state": "enable",
                "network-instance": "mgmt",
                "server": [{ "address": "10.0.0.123" }],
            },
            "srl_nokia-gnmi-server:gnmi-server": {
                "admin-state": "enable",
                "timeout": 7200,
                "rate-limit": 60,
            },
        },
    })
}

impl Default for Device {
    fn default() -> Self {
        Self::new()
    }
}

impl Device {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(document())),
        }
    }

    /// Serve on `127.0.0.1:port` (0 picks one); returns the port.
    pub async fn listen(self, port: u16) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
        let port = listener.local_addr().expect("local address").port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let device = self.clone();
                tokio::spawn(async move { device.serve(socket).await });
            }
        });
        port
    }

    async fn serve(self, socket: tokio::net::TcpStream) {
        let Ok(mut connection) = h2::server::Builder::new()
            .initial_window_size(16 * 1024 * 1024)
            .handshake::<_, Bytes>(socket)
            .await
        else {
            return;
        };
        while let Some(Ok((request, respond))) = connection.accept().await {
            let (parts, body) = request.into_parts();
            let path = parts.uri.path().to_owned();
            let device = self.clone();
            tokio::spawn(async move { device.call(path, body, respond).await });
        }
    }

    async fn call(self, path: String, body: RecvStream, mut respond: SendResponse<Bytes>) {
        let (requests_tx, mut requests) = mpsc::unbounded_channel::<Bytes>();
        tokio::spawn(read_messages(body, requests_tx));
        let response = Response::builder()
            .status(200)
            .header("content-type", "application/grpc")
            .body(())
            .expect("response");
        let Ok(mut send) = respond.send_response(response, false) else {
            return;
        };
        let mut reply = |message: &[u8]| {
            let mut out = BytesMut::with_capacity(message.len() + 5);
            out.put_u8(0);
            out.put_u32(message.len() as u32);
            out.put_slice(message);
            send.reserve_capacity(out.len());
            send.send_data(out.freeze(), false).is_ok()
        };
        let status = match path.as_str() {
            "/gnmi.gNMI/Capabilities" => {
                let _ = requests.recv().await;
                let response = gnmi::CapabilityResponse {
                    supported_models: models(),
                    supported_encodings: vec![
                        gnmi::Encoding::JsonIetf as i32,
                        gnmi::Encoding::Ascii as i32,
                        gnmi::Encoding::Proto as i32,
                    ],
                    g_nmi_version: "0.10.0".into(),
                    extension: Vec::new(),
                };
                reply(&response.encode_to_vec());
                (0, None)
            }
            "/gnmi.gNMI/Get" => match requests.recv().await {
                Some(bytes) => match gnmi::GetRequest::decode(bytes) {
                    Ok(request) => match self.get(&request) {
                        Ok(response) => {
                            reply(&response.encode_to_vec());
                            (0, None)
                        }
                        Err((code, message)) => (code, Some(message)),
                    },
                    Err(err) => (3, Some(err.to_string())),
                },
                None => (3, Some("no request".into())),
            },
            "/gnmi.gNMI/Set" => match requests.recv().await {
                Some(bytes) => match gnmi::SetRequest::decode(bytes) {
                    Ok(request) => match self.set(&request) {
                        Ok(response) => {
                            reply(&response.encode_to_vec());
                            (0, None)
                        }
                        Err((code, message)) => (code, Some(message)),
                    },
                    Err(err) => (3, Some(err.to_string())),
                },
                None => (3, Some("no request".into())),
            },
            "/gnmi.gNMI/Subscribe" => {
                let first = match requests.recv().await {
                    Some(bytes) => gnmi::SubscribeRequest::decode(bytes).ok(),
                    None => None,
                };
                match first.and_then(|request| match request.request {
                    Some(gnmi::subscribe_request::Request::Subscribe(list)) => Some(list),
                    _ => None,
                }) {
                    Some(list) => self.subscribe(list, &mut requests, &mut reply).await,
                    None => (
                        3,
                        Some("the first message must be a subscription list".into()),
                    ),
                }
            }
            _ => (12, Some(format!("unknown service {path}"))),
        };
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", HeaderValue::from(status.0));
        if let Some(message) = status.1
            && let Ok(value) = HeaderValue::from_str(&percent(&message))
        {
            trailers.insert("grpc-message", value);
        }
        let _ = send.send_trailers(trailers);
    }

    fn get(&self, request: &gnmi::GetRequest) -> Result<gnmi::GetResponse, (i32, String)> {
        let depth = request
            .extension
            .iter()
            .find_map(|extension| match &extension.ext {
                Some(muxus_mgmt::proto::gnmi_ext::extension::Ext::Depth(depth)) => {
                    Some(depth.level)
                }
                _ => None,
            })
            .unwrap_or(0);
        let config = request.r#type == gnmi::get_request::DataType::Config as i32;
        let data = self.data.lock().expect("data");
        let mut updates = Vec::new();
        let prefix = request.prefix.clone().unwrap_or_default();
        for path in &request.path {
            let elems: Vec<gnmi::PathElem> =
                prefix.elem.iter().chain(&path.elem).cloned().collect();
            for (concrete, value) in resolve(&data, &elems)? {
                let mut value = value.clone();
                if config {
                    strip_state(&mut value);
                }
                if depth > 0 {
                    value = truncate(&value, depth);
                }
                updates.push(update(concrete, &value));
            }
        }
        Ok(gnmi::GetResponse {
            notification: vec![gnmi::Notification {
                timestamp: now_ns(),
                prefix: None,
                update: updates,
                delete: Vec::new(),
                atomic: false,
            }],
            error: None,
            extension: Vec::new(),
        })
    }

    fn set(&self, request: &gnmi::SetRequest) -> Result<gnmi::SetResponse, (i32, String)> {
        let mut data = self.data.lock().expect("data");
        let prefix = request.prefix.clone().unwrap_or_default();
        let mut results = Vec::new();
        let full = |path: &Option<gnmi::Path>| -> Vec<gnmi::PathElem> {
            prefix
                .elem
                .iter()
                .chain(
                    path.as_ref()
                        .map(|path| path.elem.as_slice())
                        .unwrap_or_default(),
                )
                .cloned()
                .collect()
        };
        for path in &request.delete {
            let elems = full(&Some(path.clone()));
            remove(&mut data, &elems);
            results.push(result(path.clone(), 1));
        }
        for (op, list) in [(2, &request.replace), (3, &request.update)] {
            for item in list.iter() {
                let elems = full(&item.path);
                let value = match item.val.as_ref().and_then(|value| value.value.as_ref()) {
                    Some(gnmi::typed_value::Value::JsonIetfVal(bytes))
                    | Some(gnmi::typed_value::Value::JsonVal(bytes)) => {
                        serde_json::from_slice(bytes).map_err(|err| (3, err.to_string()))?
                    }
                    Some(gnmi::typed_value::Value::AsciiVal(text)) => Value::String(text.clone()),
                    Some(gnmi::typed_value::Value::StringVal(text)) => Value::String(text.clone()),
                    _ => return Err((3, "unsupported value encoding".into())),
                };
                write(&mut data, &elems, value, op == 3);
                results.push(result(item.path.clone().unwrap_or_default(), op));
            }
        }
        Ok(gnmi::SetResponse {
            prefix: request.prefix.clone(),
            response: results,
            message: None,
            timestamp: now_ns(),
            extension: Vec::new(),
        })
    }

    async fn subscribe(
        &self,
        list: gnmi::SubscriptionList,
        requests: &mut mpsc::UnboundedReceiver<Bytes>,
        reply: &mut impl FnMut(&[u8]) -> bool,
    ) -> (i32, Option<String>) {
        let prefix = list.prefix.clone().unwrap_or_default();
        let paths: Vec<Vec<gnmi::PathElem>> = list
            .subscription
            .iter()
            .map(|subscription| {
                prefix
                    .elem
                    .iter()
                    .chain(
                        subscription
                            .path
                            .as_ref()
                            .map(|path| path.elem.as_slice())
                            .unwrap_or_default(),
                    )
                    .cloned()
                    .collect()
            })
            .collect();
        let interval = list
            .subscription
            .iter()
            .map(|subscription| subscription.sample_interval)
            .find(|interval| *interval > 0)
            .unwrap_or(10_000_000_000);
        let sample = list
            .subscription
            .iter()
            .any(|subscription| subscription.mode != gnmi::SubscriptionMode::OnChange as i32);
        let send_all = |reply: &mut dyn FnMut(&[u8]) -> bool| -> Result<bool, (i32, String)> {
            let data = self.data.lock().expect("data").clone();
            let mut updates = Vec::new();
            for elems in &paths {
                for (concrete, value) in resolve(&data, elems)? {
                    leaves(&concrete, value, &mut updates);
                }
            }
            let notification = gnmi::SubscribeResponse {
                response: Some(gnmi::subscribe_response::Response::Update(
                    gnmi::Notification {
                        timestamp: now_ns(),
                        prefix: None,
                        update: updates,
                        delete: Vec::new(),
                        atomic: false,
                    },
                )),
                extension: Vec::new(),
            };
            Ok(reply(&notification.encode_to_vec()))
        };
        let sync = gnmi::SubscribeResponse {
            response: Some(gnmi::subscribe_response::Response::SyncResponse(true)),
            extension: Vec::new(),
        }
        .encode_to_vec();
        if let Err((code, message)) = send_all(reply) {
            return (code, Some(message));
        }
        if !reply(&sync) {
            return (1, None);
        }
        let mode = list.mode;
        if mode == gnmi::subscription_list::Mode::Once as i32 {
            return (0, None);
        }
        loop {
            if mode == gnmi::subscription_list::Mode::Poll as i32 {
                match requests.recv().await {
                    Some(_) => {
                        if send_all(reply).is_err() || !reply(&sync) {
                            return (1, None);
                        }
                    }
                    None => return (0, None),
                }
                continue;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_nanos(interval)) => {
                    self.tick(interval);
                    if sample && !matches!(send_all(reply), Ok(true)) {
                        return (1, None);
                    }
                }
                message = requests.recv() => {
                    if message.is_none() {
                        return (1, Some("the client went away".into()));
                    }
                }
            }
        }
    }

    /// Counters grow with time.
    fn tick(&self, interval_ns: u64) {
        let seconds = interval_ns as f64 / 1e9;
        let mut data = self.data.lock().expect("data");
        if let Some(Value::Array(interfaces)) = data.get_mut("srl_nokia-interfaces:interface") {
            for (index, interface) in interfaces.iter_mut().enumerate() {
                let up = interface.get("oper-state") == Some(&json!("up"));
                if let Some(Value::Object(statistics)) = interface.get_mut("statistics") {
                    for (name, rate) in [
                        ("in-octets", 125_000.),
                        ("out-octets", 98_000.),
                        ("in-packets", 410.),
                        ("out-packets", 380.),
                    ] {
                        if !up {
                            continue;
                        }
                        let current: f64 = statistics
                            .get(name)
                            .and_then(Value::as_str)
                            .and_then(|text| text.parse().ok())
                            .unwrap_or(0.);
                        let next = current + rate * seconds * (index as f64 + 1.);
                        statistics.insert(name.into(), json!((next as u64).to_string()));
                    }
                }
            }
        }
    }
}

async fn read_messages(mut body: RecvStream, sender: mpsc::UnboundedSender<Bytes>) {
    let mut buffer = BytesMut::new();
    while let Some(Ok(chunk)) = body.data().await {
        let _ = body.flow_control().release_capacity(chunk.len());
        buffer.extend_from_slice(&chunk);
        while buffer.len() >= 5 {
            let length = u32::from_be_bytes(buffer[1..5].try_into().expect("4 bytes")) as usize;
            if buffer.len() < 5 + length {
                break;
            }
            buffer.advance(5);
            let _ = sender.send(buffer.split_to(length).freeze());
        }
    }
}

fn percent(message: &str) -> String {
    message
        .bytes()
        .map(|byte| {
            if (0x20..0x7f).contains(&byte) && byte != b'%' {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i64)
        .unwrap_or(0)
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// The child of an object named `name`, with or without its module prefix.
fn child_key(object: &Map<String, Value>, name: &str) -> Option<String> {
    object.keys().find(|key| local(key) == local(name)).cloned()
}

fn keys_of(elem: &gnmi::PathElem) -> Vec<(String, String)> {
    elem.key
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn entry_matches(entry: &Value, keys: &[(String, String)]) -> bool {
    keys.iter().all(|(key, value)| {
        value == "*"
            || entry.get(key).is_some_and(|candidate| match candidate {
                Value::String(text) => text == value,
                other => serde_json::from_str::<Value>(value).is_ok_and(|parsed| &parsed == other),
            })
    })
}

fn path_of(elems: &[gnmi::PathElem]) -> gnmi::Path {
    gnmi::Path {
        element: Vec::new(),
        origin: String::new(),
        elem: elems.to_vec(),
        target: String::new(),
    }
}

/// A selected path and its value.
type Selected<'a> = (Vec<gnmi::PathElem>, &'a Value);

/// Every concrete path and value `elems` selects (wildcard keys expand).
fn resolve<'a>(
    data: &'a Value,
    elems: &[gnmi::PathElem],
) -> Result<Vec<Selected<'a>>, (i32, String)> {
    let mut found: Vec<(Vec<gnmi::PathElem>, &Value)> = vec![(Vec::new(), data)];
    for elem in elems {
        let mut next = Vec::new();
        for (path, value) in found {
            let Value::Object(object) = value else {
                continue;
            };
            let Some(key) = child_key(object, &elem.name) else {
                let mut options: Vec<&str> = object.keys().map(|key| local(key)).collect();
                options.sort_unstable();
                return Err((
                    3,
                    format!(
                        "Path not valid - unknown element '{}'. Options are [{}]",
                        local(&elem.name),
                        options.join(", ")
                    ),
                ));
            };
            let child = &object[&key];
            let keys = keys_of(elem);
            let mut named = elem.clone();
            named.name = if path.is_empty() {
                key.clone()
            } else {
                local(&key).to_owned()
            };
            match child {
                Value::Array(entries) if !keys.is_empty() => {
                    for entry in entries.iter().filter(|entry| entry_matches(entry, &keys)) {
                        let mut concrete = named.clone();
                        concrete.key = keys
                            .iter()
                            .map(|(key, _)| gnmi::path_elem::KeyEntry {
                                key: key.clone(),
                                value: match &entry[key] {
                                    Value::String(text) => text.clone(),
                                    other => other.to_string(),
                                },
                            })
                            .collect();
                        let mut path = path.clone();
                        path.push(concrete);
                        next.push((path, entry));
                    }
                }
                _ => {
                    let mut path = path.clone();
                    path.push(named);
                    next.push((path, child));
                }
            }
        }
        found = next;
    }
    Ok(found)
}

fn strip_state(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| !STATE_ONLY.contains(&local(key)));
            for child in object.values_mut() {
                strip_state(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_state),
        _ => {}
    }
}

/// The value down to `depth` levels; the last level keeps only leaves (as SR Linux does).
fn truncate(value: &Value, depth: u32) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, child)| match child {
                    Value::Object(_) | Value::Array(_) if depth <= 1 => {
                        // Leaf-lists of scalars are leaves too.
                        match child {
                            Value::Array(items) if items.iter().all(|item| !item.is_object()) => {
                                Some((key.clone(), child.clone()))
                            }
                            _ => None,
                        }
                    }
                    Value::Object(_) => Some((key.clone(), truncate(child, depth - 1))),
                    Value::Array(items) => Some((
                        key.clone(),
                        Value::Array(items.iter().map(|item| truncate(item, depth - 1)).collect()),
                    )),
                    _ => Some((key.clone(), child.clone())),
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| truncate(item, depth)).collect())
        }
        other => other.clone(),
    }
}

fn update(path: Vec<gnmi::PathElem>, value: &Value) -> gnmi::Update {
    gnmi::Update {
        path: Some(path_of(&path)),
        value: None,
        val: Some(gnmi::TypedValue {
            value: Some(gnmi::typed_value::Value::JsonIetfVal(
                serde_json::to_vec(value).unwrap_or_default(),
            )),
        }),
        duplicates: 0,
    }
}

/// One update per leaf, as devices stream them.
fn leaves(path: &[gnmi::PathElem], value: &Value, out: &mut Vec<gnmi::Update>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let mut child_path = path.to_vec();
                child_path.push(gnmi::PathElem {
                    name: local(key).to_owned(),
                    key: Vec::new(),
                });
                leaves(&child_path, child, out);
            }
        }
        Value::Array(items) if items.iter().any(Value::is_object) => {
            for item in items {
                let mut child_path = path.to_vec();
                if let (Some(last), Some(name)) = (
                    child_path.last_mut(),
                    item.get("name")
                        .or(item.get("index"))
                        .or(item.get("peer-address")),
                ) {
                    let key = if item.get("name").is_some() {
                        "name"
                    } else if item.get("index").is_some() {
                        "index"
                    } else {
                        "peer-address"
                    };
                    last.key = vec![gnmi::path_elem::KeyEntry {
                        key: key.into(),
                        value: match name {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        },
                    }];
                }
                leaves(&child_path, item, out);
            }
        }
        _ => out.push(update(path.to_vec(), value)),
    }
}

fn locate<'a>(data: &'a mut Value, elems: &[gnmi::PathElem]) -> Option<&'a mut Value> {
    let mut current = data;
    for elem in elems {
        let Value::Object(object) = current else {
            return None;
        };
        let key = child_key(object, &elem.name).unwrap_or_else(|| elem.name.clone());
        let keys = keys_of(elem);
        let child = object.entry(key).or_insert_with(|| {
            if keys.is_empty() {
                Value::Object(Map::new())
            } else {
                Value::Array(Vec::new())
            }
        });
        current = if !keys.is_empty() && child.is_array() {
            let entries = child.as_array_mut().expect("an array");
            let index = match entries.iter().position(|entry| entry_matches(entry, &keys)) {
                Some(index) => index,
                None => {
                    let mut entry = Map::new();
                    for (key, value) in &keys {
                        entry.insert(key.clone(), Value::String(value.clone()));
                    }
                    entries.push(Value::Object(entry));
                    entries.len() - 1
                }
            };
            &mut entries[index]
        } else {
            child
        };
    }
    Some(current)
}

fn merge(target: &mut Value, value: Value) {
    match (target, value) {
        (Value::Object(target), Value::Object(value)) => {
            for (key, child) in value {
                match target.get_mut(&key) {
                    Some(existing) => merge(existing, child),
                    None => {
                        target.insert(key, child);
                    }
                }
            }
        }
        (target, value) => *target = value,
    }
}

fn write(data: &mut Value, elems: &[gnmi::PathElem], value: Value, merge_into: bool) {
    if let Some(target) = locate(data, elems) {
        if merge_into {
            merge(target, value);
        } else {
            *target = value;
        }
    }
}

fn remove(data: &mut Value, elems: &[gnmi::PathElem]) {
    let Some((last, parent)) = elems.split_last() else {
        return;
    };
    let Some(Value::Object(object)) = locate(data, parent) else {
        return;
    };
    let Some(key) = child_key(object, &last.name) else {
        return;
    };
    let keys = keys_of(last);
    if keys.is_empty() {
        object.remove(&key);
    } else if let Some(Value::Array(entries)) = object.get_mut(&key) {
        entries.retain(|entry| !entry_matches(entry, &keys));
    }
}

fn result(path: gnmi::Path, op: i32) -> gnmi::UpdateResult {
    gnmi::UpdateResult {
        timestamp: 0,
        path: Some(path),
        message: None,
        op,
    }
}
