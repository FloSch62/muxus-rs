//! Parsers for what docker, podman, kubectl, pvesh and virsh print
//! (`server/src/environments/parse.ts`). Each takes the tool's raw stdout and
//! returns only well-formed entries: a host is free to print anything, and a
//! line that does not parse is skipped, not fatal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use muxus_core::collate::locale_compare;
use muxus_core::environments::{
    ContainerInfo, ContainerPort, ContainerRuntime, ContainerlabLab, ContainerlabLabels,
    ContainerlabNode, KubernetesOwner, KubernetesPod, LibvirtDomain, ProxmoxGuest,
    ProxmoxGuestType, VncEndpoint,
};
use muxus_core::js::js_trim;
use regex::Regex;
use serde_json::{Map, Value};

fn record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// A non-blank string, trimmed.
fn text(value: Option<&Value>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A finite number.
fn count(value: Option<&Value>) -> Option<f64> {
    value?.as_f64().filter(|number| number.is_finite())
}

fn json_lines(output: &str) -> Vec<Value> {
    output
        .split('\n')
        .map(js_trim)
        .filter(|line| line.starts_with('{'))
        // A warning or a truncated line is skipped.
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The JSON value that starts at the first `[` or `{`.
fn json_value(output: &str) -> Option<Value> {
    let start = output.find(['[', '{'])?;
    serde_json::from_str(&output[start..]).ok()
}

/// The `docker ps --format` template whose lines [`parse_docker_ps`] reads.
pub const DOCKER_PS_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"name":{{json .Names}},"image":{{json .Image}},"state":{{json .State}},"#,
    r#""status":{{json .Status}},"ports":{{json .Ports}},"#,
    r#""project":{{json (.Label "com.docker.compose.project")}},"#,
    r#""clab":{{json (.Label "containerlab")}},"clabNode":{{json (.Label "clab-node-name")}},"#,
    r#""clabKind":{{json (.Label "clab-node-kind")}},"clabTopo":{{json (.Label "clab-topo-file")}},"#,
    r#""clabOwner":{{json (.Label "clab-owner")}},"clabInternal":{{json (.Label "clab-internal-node")}}}"#,
);

static DOCKER_PORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(.*):(\d+)(?:-(\d+))?->)?(\d+)(?:-(\d+))?/(\w+)$").expect("port pattern")
});

/// Docker's port column: "0.0.0.0:8080->80/tcp, :::8080->80/tcp, 443/tcp".
/// The IPv4 and IPv6 bindings of one port become one entry.
pub fn parse_docker_ports(column: &str) -> Vec<ContainerPort> {
    let mut ports = Vec::new();
    let mut seen = HashSet::new();
    for raw in column.split(',') {
        let entry = js_trim(raw);
        if entry.is_empty() {
            continue;
        }
        let Some(found) = DOCKER_PORT.captures(entry) else {
            continue;
        };
        let number = |index: usize| {
            found
                .get(index)
                .and_then(|m| m.as_str().parse::<u32>().ok())
        };
        let Some(first) = number(4) else {
            continue;
        };
        let last = number(5).unwrap_or(first);
        let protocol = found.get(6).map_or("", |m| m.as_str()).to_string();
        let host_start = number(2);
        let host_end = number(3);
        // A published range lists each port; keep a sane bound on what it expands to.
        let span = last.saturating_sub(first).min(63);
        for offset in 0..=span {
            let Ok(container_port) = u16::try_from(first + offset) else {
                continue;
            };
            let mut port = ContainerPort {
                host_ip: None,
                host_port: None,
                container_port,
                protocol: protocol.clone(),
            };
            if let Some(start) = host_start {
                let host = start + if host_end.is_some() { offset } else { 0 };
                port.host_port = u16::try_from(host).ok();
                let raw_ip = found.get(1).map_or("", |m| m.as_str());
                let ip = raw_ip.strip_prefix('[').unwrap_or(raw_ip);
                let ip = ip.strip_suffix(']').unwrap_or(ip);
                port.host_ip = Some(if ip.is_empty() { "0.0.0.0" } else { ip }.to_string());
            }
            let key = format!(
                "{}:{}/{}",
                port.host_port.map(|p| p.to_string()).unwrap_or_default(),
                port.container_port,
                port.protocol
            );
            if seen.insert(key) {
                ports.push(port);
            }
        }
    }
    ports
}

struct ClabFields {
    lab: Option<String>,
    node: Option<String>,
    kind: Option<String>,
    topo: Option<String>,
    owner: Option<String>,
    internal: Option<String>,
}

fn clab_labels(fields: ClabFields) -> Option<ContainerlabLabels> {
    // Containerlab's own helpers (sshx, gotty …) are not nodes of the topology.
    if fields.internal.as_deref() == Some("true") {
        return None;
    }
    Some(ContainerlabLabels {
        lab: fields.lab?,
        node: fields.node?,
        kind: fields.kind.unwrap_or_default(),
        topology_file: fields.topo,
        owner: fields.owner,
    })
}

fn by_name(a: &ContainerInfo, b: &ContainerInfo) -> std::cmp::Ordering {
    locale_compare(&a.name, &b.name)
}

/// The first 12 UTF-16 units of an ID (`id.slice(0, 12)`).
fn short_id(id: &str) -> String {
    muxus_core::js::utf16_prefix(id, 12).to_string()
}

/// `docker ps -a --format` lines in the [`DOCKER_PS_FORMAT`] shape.
pub fn parse_docker_ps(output: &str) -> Vec<ContainerInfo> {
    let mut containers = Vec::new();
    for value in json_lines(output) {
        let Some(row) = record(&value) else {
            continue;
        };
        let name = text(row.get("name")).and_then(|names| {
            let first = names.split(',').next().unwrap_or("").to_string();
            (!first.is_empty()).then_some(first)
        });
        let (Some(name), Some(id)) = (name, text(row.get("id"))) else {
            continue;
        };
        let containerlab = clab_labels(ClabFields {
            lab: text(row.get("clab")),
            node: text(row.get("clabNode")),
            kind: text(row.get("clabKind")),
            topo: text(row.get("clabTopo")),
            owner: text(row.get("clabOwner")),
            internal: text(row.get("clabInternal")),
        });
        containers.push(ContainerInfo {
            id: short_id(&id),
            name,
            image: text(row.get("image")).unwrap_or_default(),
            state: text(row.get("state"))
                .map(|state| state.to_lowercase())
                .unwrap_or_else(|| "unknown".into()),
            status: text(row.get("status")).unwrap_or_default(),
            ports: parse_docker_ports(&text(row.get("ports")).unwrap_or_default()),
            compose_project: text(row.get("project")),
            containerlab,
        });
    }
    containers.sort_by(by_name);
    containers
}

/// `podman ps -a --format json`: one JSON array.
pub fn parse_podman_ps(output: &str) -> Vec<ContainerInfo> {
    let Some(Value::Array(items)) = json_value(output) else {
        return Vec::new();
    };
    let mut containers = Vec::new();
    for item in &items {
        let Some(row) = record(item) else {
            continue;
        };
        let name = match row.get("Names") {
            Some(Value::Array(names)) => text(names.first()),
            other => text(other),
        };
        let id = text(row.get("Id")).or_else(|| text(row.get("ID")));
        let (Some(name), Some(id)) = (name, id) else {
            continue;
        };
        let empty = Map::new();
        let labels = row.get("Labels").and_then(record).unwrap_or(&empty);
        let mut ports = Vec::new();
        if let Some(Value::Array(entries)) = row.get("Ports") {
            for entry in entries {
                let Some(port) = record(entry) else {
                    continue;
                };
                let Some(container_port) = count(port.get("container_port")) else {
                    continue;
                };
                let range = count(port.get("range")).unwrap_or(1.).clamp(1., 64.) as u32;
                let host_port = count(port.get("host_port")).filter(|port| *port != 0.);
                for offset in 0..range {
                    let Ok(container) = u16::try_from(container_port as u32 + offset) else {
                        continue;
                    };
                    ports.push(ContainerPort {
                        host_ip: host_port
                            .map(|_| text(port.get("host_ip")).unwrap_or_else(|| "0.0.0.0".into())),
                        host_port: host_port
                            .and_then(|host| u16::try_from(host as u32 + offset).ok()),
                        container_port: container,
                        protocol: text(port.get("protocol")).unwrap_or_else(|| "tcp".into()),
                    });
                }
            }
        }
        let containerlab = clab_labels(ClabFields {
            lab: text(labels.get("containerlab")),
            node: text(labels.get("clab-node-name")),
            kind: text(labels.get("clab-node-kind")),
            topo: text(labels.get("clab-topo-file")),
            owner: text(labels.get("clab-owner")),
            internal: text(labels.get("clab-internal-node")),
        });
        let project = text(labels.get("com.docker.compose.project"))
            .or_else(|| text(labels.get("io.podman.compose.project")));
        containers.push(ContainerInfo {
            id: short_id(&id),
            name,
            image: text(row.get("Image")).unwrap_or_default(),
            state: text(row.get("State"))
                .map(|state| state.to_lowercase())
                .unwrap_or_else(|| "unknown".into()),
            status: text(row.get("Status")).unwrap_or_default(),
            ports,
            compose_project: project,
            containerlab,
        });
    }
    containers.sort_by(by_name);
    containers
}

/// The `inspect --format` template whose lines [`parse_inspect_addresses`] reads.
pub const INSPECT_NETWORKS_FORMAT: &str = r#"{"name":{{json .Name}},"mode":{{json .HostConfig.NetworkMode}},"nets":{{json .NetworkSettings.Networks}}}"#;

/// Management addresses of one container.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeAddress {
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

/// Management addresses by container name, from the container's own network.
pub fn parse_inspect_addresses(output: &str) -> HashMap<String, NodeAddress> {
    let mut addresses = HashMap::new();
    for value in json_lines(output) {
        let Some(row) = record(&value) else {
            continue;
        };
        let name =
            text(row.get("name")).map(|name| name.strip_prefix('/').unwrap_or(&name).to_string());
        let networks = row.get("nets").and_then(record);
        let (Some(name), Some(networks)) = (name, networks) else {
            continue;
        };
        let mode = text(row.get("mode"));
        let network = mode
            .as_deref()
            .and_then(|mode| networks.get(mode))
            .filter(|value| !value.is_null())
            .or_else(|| networks.values().next())
            .and_then(record);
        let Some(network) = network else {
            continue;
        };
        addresses.insert(
            name,
            NodeAddress {
                ipv4: text(network.get("IPAddress")),
                ipv6: text(network.get("GlobalIPv6Address")),
            },
        );
    }
    addresses
}

/// Labs from the containerlab labels of a runtime's containers.
pub fn containerlab_labs(
    runtime: ContainerRuntime,
    containers: &[ContainerInfo],
    addresses: &HashMap<String, NodeAddress>,
) -> Vec<ContainerlabLab> {
    let mut labs: BTreeMap<String, ContainerlabLab> = BTreeMap::new();
    for container in containers {
        let Some(labels) = &container.containerlab else {
            continue;
        };
        let lab = labs
            .entry(labels.lab.clone())
            .or_insert_with(|| ContainerlabLab {
                name: labels.lab.clone(),
                runtime,
                topology_file: labels.topology_file.clone(),
                owner: labels.owner.clone(),
                nodes: Vec::new(),
            });
        let address = addresses.get(&container.name);
        lab.nodes.push(ContainerlabNode {
            name: labels.node.clone(),
            container: container.name.clone(),
            kind: labels.kind.clone(),
            image: container.image.clone(),
            state: container.state.clone(),
            ipv4: address.and_then(|a| a.ipv4.clone()),
            ipv6: address.and_then(|a| a.ipv6.clone()),
        });
    }
    let mut labs: Vec<ContainerlabLab> = labs.into_values().collect();
    for lab in &mut labs {
        lab.nodes.sort_by(|a, b| locale_compare(&a.name, &b.name));
    }
    labs.sort_by(|a, b| locale_compare(&a.name, &b.name));
    labs
}

static CLI_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bversion\s+v?([0-9][\w.+-]*)").expect("version pattern"));

/// "Docker version 28.0.1, build …" / "podman version 5.2.0" → the version.
pub fn parse_cli_version(output: &str) -> Option<String> {
    let version = CLI_VERSION.captures(output)?.get(1)?.as_str();
    Some(version.strip_suffix(',').unwrap_or(version).to_string())
}

/// Non-empty lines, such as `kubectl config get-contexts -o name`.
pub fn parse_lines(output: &str) -> Vec<String> {
    output
        .split('\n')
        .map(js_trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// `kubectl version --client -o json`.
pub fn parse_kubectl_version(output: &str) -> Option<String> {
    let value = json_value(output)?;
    text(value.get("clientVersion")?.get("gitVersion"))
}

/// `^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`.
fn is_namespace(name: &str) -> bool {
    let bytes = name.as_bytes();
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    !bytes.is_empty()
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1])
        && bytes.iter().all(|&b| edge(b) || b == b'-')
}

/// `kubectl get namespaces -o name`: "namespace/default" per line.
pub fn parse_kube_namespaces(output: &str) -> Vec<String> {
    let mut names: Vec<String> = parse_lines(output)
        .into_iter()
        .map(|line| line.strip_prefix("namespace/").unwrap_or(&line).to_string())
        .filter(|name| is_namespace(name))
        .collect();
    names.sort_by(|a, b| locale_compare(a, b));
    names
}

/// `kubectl get pods -o json`, reduced to what a pod row shows.
pub fn parse_kube_pods(output: &str) -> Vec<KubernetesPod> {
    let Some(value) = json_value(output) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = value.get("items") else {
        return Vec::new();
    };
    let mut pods = Vec::new();
    for item in items {
        let metadata = item.get("metadata").and_then(record);
        let spec = item.get("spec").and_then(record);
        let status = item.get("status").and_then(record);
        let name = metadata.and_then(|m| text(m.get("name")));
        let namespace = metadata.and_then(|m| text(m.get("namespace")));
        let (Some(name), Some(namespace)) = (name, namespace) else {
            continue;
        };
        let statuses: Vec<&Map<String, Value>> =
            match status.and_then(|s| s.get("containerStatuses")) {
                Some(Value::Array(entries)) => entries.iter().filter_map(record).collect(),
                _ => Vec::new(),
            };
        let containers: Vec<String> = match spec.and_then(|s| s.get("containers")) {
            Some(Value::Array(entries)) => entries
                .iter()
                .filter_map(|entry| text(record(entry)?.get("name")))
                .collect(),
            _ => Vec::new(),
        };
        let ready = statuses
            .iter()
            .filter(|entry| entry.get("ready") == Some(&Value::Bool(true)))
            .count();
        let restarts: f64 = statuses
            .iter()
            .map(|entry| count(entry.get("restartCount")).unwrap_or(0.))
            .sum();
        let phase = status.and_then(|s| text(s.get("phase")));
        let mut reason: Option<String> = None;
        if metadata.is_some_and(|m| m.get("deletionTimestamp").is_some_and(truthy)) {
            reason = Some("Terminating".into());
        }
        for entry in &statuses {
            if reason.is_some() {
                break;
            }
            let state = entry.get("state").and_then(record);
            reason = state
                .and_then(|s| s.get("waiting"))
                .and_then(|w| text(w.get("reason")))
                .or_else(|| {
                    state
                        .and_then(|s| s.get("terminated"))
                        .and_then(|t| text(t.get("reason")))
                });
            if reason.as_deref() == Some("Completed") && phase.as_deref() == Some("Succeeded") {
                reason = None;
            }
        }
        if reason.is_none() {
            reason = status.and_then(|s| text(s.get("reason")));
        }
        let owner = metadata
            .and_then(|m| m.get("ownerReferences"))
            .and_then(Value::as_array)
            .and_then(|refs| refs.first())
            .and_then(record);
        let owner = owner.and_then(|owner| {
            Some(KubernetesOwner {
                kind: text(owner.get("kind"))?,
                name: text(owner.get("name"))?,
            })
        });
        let total = if containers.is_empty() {
            statuses.len()
        } else {
            containers.len()
        };
        pods.push(KubernetesPod {
            name,
            namespace,
            phase: phase.unwrap_or_else(|| "Unknown".into()),
            ready: format!("{ready}/{total}"),
            restarts: restarts.max(0.) as u32,
            reason,
            owner,
            containers,
            node: spec.and_then(|s| text(s.get("nodeName"))),
            created_at: metadata.and_then(|m| text(m.get("creationTimestamp"))),
        });
    }
    pods.sort_by(|a, b| {
        locale_compare(&a.namespace, &b.namespace).then_with(|| locale_compare(&a.name, &b.name))
    });
    pods
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

static PVE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"pve-manager/([\w.-]+)").expect("pve pattern"));

/// `pveversion`: "pve-manager/8.3.0/c1689ccb1065a83b (running kernel: …)".
pub fn parse_pve_version(output: &str) -> Option<String> {
    Some(PVE_VERSION.captures(output)?.get(1)?.as_str().to_string())
}

/// `pvesh get /cluster/resources --type vm --output-format json`.
pub fn parse_pve_resources(output: &str) -> Vec<ProxmoxGuest> {
    let Some(Value::Array(items)) = json_value(output) else {
        return Vec::new();
    };
    let mut guests = Vec::new();
    for item in &items {
        let Some(row) = record(item) else {
            continue;
        };
        let Some(vmid) = count(row.get("vmid")) else {
            continue;
        };
        let guest_type = match row.get("type").and_then(Value::as_str) {
            Some("qemu") => ProxmoxGuestType::Qemu,
            Some("lxc") => ProxmoxGuestType::Lxc,
            _ => continue,
        };
        let vmid = vmid as u32;
        let label = if guest_type == ProxmoxGuestType::Qemu {
            "VM"
        } else {
            "CT"
        };
        guests.push(ProxmoxGuest {
            vmid,
            name: text(row.get("name")).unwrap_or_else(|| format!("{label} {vmid}")),
            guest_type,
            status: text(row.get("status")).unwrap_or_else(|| "unknown".into()),
            node: text(row.get("node")),
            cpu: count(row.get("cpu")),
            maxcpu: count(row.get("maxcpu")),
            mem: count(row.get("mem")),
            maxmem: count(row.get("maxmem")),
            uptime: count(row.get("uptime")),
            template: matches!(row.get("template"), Some(Value::Bool(true)))
                .then_some(true)
                .or_else(|| (count(row.get("template")) == Some(1.)).then_some(true)),
        });
    }
    guests.sort_by_key(|guest| guest.vmid);
    guests
}

static VIRSH_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\d+|-)\s+(\S(?:.*?\S)?)\s{2,}(\S.*?)\s*$").expect("virsh pattern")
});

/// `virsh list --all`:
///
/// ```text
///  Id   Name        State
/// ----------------------------
///  1    runner      running
///  -    win11       shut off
/// ```
pub fn parse_virsh_list(output: &str) -> Vec<LibvirtDomain> {
    let mut domains = Vec::new();
    for line in output.split('\n') {
        let Some(found) = VIRSH_ROW.captures(line) else {
            continue;
        };
        let id = found.get(1).map_or("", |m| m.as_str());
        let name = found.get(2).map_or("", |m| m.as_str());
        if name == "Name" {
            continue;
        }
        domains.push(LibvirtDomain {
            name: name.to_string(),
            state: found.get(3).map_or("", |m| m.as_str()).to_string(),
            id: id.parse().ok(),
            uri: None,
        });
    }
    domains.sort_by(|a, b| locale_compare(&a.name, &b.name));
    domains
}

fn usable_address(address: &str) -> bool {
    !address.is_empty()
        && !address.starts_with("127.")
        && address != "::1"
        && !address.to_lowercase().starts_with("fe80:")
        && !address.starts_with("169.254.")
}

/// Unique usable addresses, IPv4 first.
fn ipv4_first(addresses: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let unique: Vec<String> = addresses
        .into_iter()
        .filter(|address| seen.insert(address.clone()))
        .filter(|address| usable_address(address))
        .collect();
    let (v6, v4): (Vec<String>, Vec<String>) = unique
        .into_iter()
        .partition(|address| address.contains(':'));
    v4.into_iter().chain(v6).collect()
}

static VIRSH_ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s(ipv4|ipv6)\s+([0-9a-fA-F:.]+)(?:/\d+)?\s*$").expect("address pattern")
});

/// `virsh domifaddr`: " vnet0  52:54:00:…  ipv4  192.168.122.10/24".
pub fn parse_virsh_addresses(output: &str) -> Vec<String> {
    ipv4_first(
        output
            .split('\n')
            .filter_map(|line| Some(VIRSH_ADDRESS.captures(line)?.get(2)?.as_str().to_string())),
    )
}

static VNC_DISPLAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^vnc://(\[[^\]]*\]|[^:/\s]*)(?::(\d+))?").expect("display pattern")
});

/// `virsh domdisplay`: "vnc://127.0.0.1:0" names display 0, port 5900.
pub fn parse_virsh_display(output: &str) -> Option<VncEndpoint> {
    let found = VNC_DISPLAY.captures(js_trim(output))?;
    let display: u32 = found.get(2).map_or(Some(0), |m| m.as_str().parse().ok())?;
    let raw = found.get(1).map_or("", |m| m.as_str());
    let raw = raw.strip_prefix('[').unwrap_or(raw);
    let raw = raw.strip_suffix(']').unwrap_or(raw);
    let host = if raw.is_empty() || raw == "0.0.0.0" || raw == "::" {
        "127.0.0.1"
    } else {
        raw
    };
    let port = if display >= 5900 {
        display
    } else {
        5900 + display
    };
    Some(VncEndpoint {
        host: host.to_string(),
        port: u16::try_from(port).ok()?,
    })
}

/// `qm guest cmd <vmid> network-get-interfaces`: the guest agent's view.
pub fn parse_qemu_agent_addresses(output: &str) -> Vec<String> {
    let Some(Value::Array(items)) = json_value(output) else {
        return Vec::new();
    };
    let mut addresses = Vec::new();
    for item in &items {
        let Some(iface) = record(item) else {
            continue;
        };
        if text(iface.get("name")).as_deref() == Some("lo") {
            continue;
        }
        if let Some(Value::Array(entries)) = iface.get("ip-addresses") {
            addresses.extend(
                entries
                    .iter()
                    .filter_map(|entry| text(record(entry)?.get("ip-address"))),
            );
        }
    }
    ipv4_first(addresses)
}

/// `lxc-info -n <vmid> -iH`: one address per line.
pub fn parse_lxc_addresses(output: &str) -> Vec<String> {
    ipv4_first(parse_lines(output).into_iter().filter(|line| {
        line.chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    }))
}
