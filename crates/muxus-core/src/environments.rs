//! Host environments (port of `shared/src/environments.ts`): the containers,
//! clusters, labs and virtual machines a host runs, found and entered through
//! the host's own command-line tools (docker, podman, kubectl, pvesh, virsh).
//! Nothing is installed on the host.
//!
//! A session into one of them is an ordinary local or SSH session whose
//! profile carries an [`EnvironmentTarget`]. The backend, never the UI, turns
//! the target into a command, from the fields validated here.
//!
//! The name patterns are the TypeScript regular expressions, written out as
//! checks: lengths count UTF-16 code units, as JavaScript does.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::js;
use crate::validate::{self, Validate, ValidationError, join};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerRuntime {
    Docker,
    Podman,
}

pub const CONTAINER_RUNTIMES: [ContainerRuntime; 2] =
    [ContainerRuntime::Docker, ContainerRuntime::Podman];

impl ContainerRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerRuntime::Docker => "docker",
            ContainerRuntime::Podman => "podman",
        }
    }
}

/// Which shell a container session starts; `auto` prefers bash, then ash, then sh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerShell {
    #[default]
    Auto,
    Bash,
    Sh,
}

pub const CONTAINER_SHELLS: [ContainerShell; 3] = [
    ContainerShell::Auto,
    ContainerShell::Bash,
    ContainerShell::Sh,
];

/// The libvirt connections Muxus looks at: the system daemon and the user's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LibvirtUri {
    #[serde(rename = "qemu:///system")]
    System,
    #[serde(rename = "qemu:///session")]
    Session,
}

pub const LIBVIRT_URIS: [LibvirtUri; 2] = [LibvirtUri::System, LibvirtUri::Session];

impl LibvirtUri {
    pub fn as_str(self) -> &'static str {
        match self {
            LibvirtUri::System => "qemu:///system",
            LibvirtUri::Session => "qemu:///session",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        LIBVIRT_URIS.into_iter().find(|uri| uri.as_str() == value)
    }
}

/// `CONTAINER_NAME_PATTERN` as zod prints it.
pub const CONTAINER_NAME_PATTERN: &str = "/^[A-Za-z0-9][A-Za-z0-9_.-]{0,254}$/";
/// `KUBE_NAME_PATTERN` as zod prints it.
pub const KUBE_NAME_PATTERN: &str = "/^(?=.{1,253}$)[a-z0-9]([-a-z0-9.]*[a-z0-9])?$/";
/// `KUBE_CONTAINER_PATTERN` as zod prints it.
pub const KUBE_CONTAINER_PATTERN: &str = "/^(?=.{1,63}$)[a-z0-9]([-a-z0-9]*[a-z0-9])?$/";
/// `KUBE_CONTEXT_PATTERN` as zod prints it.
pub const KUBE_CONTEXT_PATTERN: &str = "/^(?!-)[^\\u0000-\\u001f\\u007f]{1,253}$/";
/// `LIBVIRT_DOMAIN_PATTERN` as zod prints it.
pub const LIBVIRT_DOMAIN_PATTERN: &str = "/^(?![-\\s])[^\\u0000-\\u001f\\u007f]{1,200}$/";

/// Proxmox guest IDs.
pub const PROXMOX_MIN_VMID: u32 = 100;
pub const PROXMOX_MAX_VMID: u32 = 999_999_999;

/// Docker and Podman container names and IDs: `/^[A-Za-z0-9][A-Za-z0-9_.-]{0,254}$/`.
pub fn is_container_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 255
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `^[a-z0-9]([-a-z0-9<extra>]*[a-z0-9])?$` with a length limit.
fn is_dns_name(value: &str, max: usize, allow_dot: bool) -> bool {
    let bytes = value.as_bytes();
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= max
        && edge(bytes[0])
        && edge(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|&b| edge(b) || b == b'-' || (allow_dot && b == b'.'))
}

/// Kubernetes object names (DNS-1123 subdomain): pods and namespaces.
pub fn is_kube_name(value: &str) -> bool {
    is_dns_name(value, 253, true)
}

/// Kubernetes container names (DNS-1123 label).
pub fn is_kube_container(value: &str) -> bool {
    is_dns_name(value, 63, false)
}

/// `[^\u0000-\u001f\u007f]`: one UTF-16 code unit that is not a C0 control or DEL.
fn printable_unit(c: char) -> bool {
    !matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}')
}

/// kubeconfig context names are free-form ("arn:aws:eks:…/prod", "admin@kind").
/// They travel quoted as `--context=<name>`; only control characters and a
/// leading dash are refused.
pub fn is_kube_context(value: &str) -> bool {
    let length = js::utf16_len(value);
    (1..=253).contains(&length) && !value.starts_with('-') && value.chars().all(printable_unit)
}

/// libvirt domain names: anything printable that cannot read as an option.
pub fn is_libvirt_domain(value: &str) -> bool {
    let length = js::utf16_len(value);
    (1..=200).contains(&length)
        && !value
            .chars()
            .next()
            .is_some_and(|first| first == '-' || js::is_js_whitespace(first))
        && value.chars().all(printable_unit)
}

fn is_vmid(vmid: u32) -> bool {
    (PROXMOX_MIN_VMID..=PROXMOX_MAX_VMID).contains(&vmid)
}

/// Something inside a host that a terminal session can enter or watch.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EnvironmentTarget {
    Container {
        runtime: ContainerRuntime,
        container: String,
        shell: Option<ContainerShell>,
    },
    ContainerLogs {
        runtime: ContainerRuntime,
        container: String,
    },
    Pod {
        context: Option<String>,
        namespace: String,
        pod: String,
        container: Option<String>,
    },
    PodLogs {
        context: Option<String>,
        namespace: String,
        pod: String,
        container: Option<String>,
        /// The log of the container's previous run, after a crash.
        previous: Option<bool>,
    },
    ProxmoxLxc {
        vmid: u32,
    },
    ProxmoxSerial {
        vmid: u32,
    },
    LibvirtConsole {
        domain: String,
        uri: Option<LibvirtUri>,
    },
}

/// How many log lines a log session starts with before following.
pub const LOG_TAIL_LINES: u32 = 1000;

/// Prefer bash, then ash (Alpine, BusyBox), then plain sh — whatever the image has.
const AUTO_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; elif command -v ash >/dev/null 2>&1; then exec ash; else exec sh; fi";

fn shell_argv(shell: Option<ContainerShell>) -> Vec<String> {
    match shell {
        Some(ContainerShell::Bash) => vec!["bash".into()],
        Some(ContainerShell::Sh) => vec!["sh".into()],
        _ => vec!["sh".into(), "-c".into(), AUTO_SHELL.into()],
    }
}

fn kube_context_args(context: Option<&str>) -> Vec<String> {
    match context {
        Some(context) if !context.is_empty() => vec![format!("--context={context}")],
        _ => Vec::new(),
    }
}

/// A target whose names or IDs a command would not accept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid environment target")]
pub struct InvalidEnvironmentTarget;

/// Whether a target's names and IDs are all ones the commands below accept.
pub fn is_valid_environment_target(target: &EnvironmentTarget) -> bool {
    match target {
        EnvironmentTarget::Container { container, .. }
        | EnvironmentTarget::ContainerLogs { container, .. } => is_container_name(container),
        EnvironmentTarget::Pod {
            context,
            namespace,
            pod,
            container,
        }
        | EnvironmentTarget::PodLogs {
            context,
            namespace,
            pod,
            container,
            ..
        } => {
            is_kube_name(namespace)
                && is_kube_name(pod)
                && container.as_deref().is_none_or(is_kube_container)
                && context.as_deref().is_none_or(is_kube_context)
        }
        EnvironmentTarget::ProxmoxLxc { vmid } | EnvironmentTarget::ProxmoxSerial { vmid } => {
            is_vmid(*vmid)
        }
        // The URI is one of the two known ones by type.
        EnvironmentTarget::LibvirtConsole { domain, .. } => is_libvirt_domain(domain),
    }
}

/// The program and arguments that enter a target, run on the host that owns
/// it. Local sessions spawn this argv directly; SSH sessions send it as the
/// channel's command, each argument quoted by [`environment_command_line`].
pub fn environment_argv(
    target: &EnvironmentTarget,
) -> Result<Vec<String>, InvalidEnvironmentTarget> {
    if !is_valid_environment_target(target) {
        return Err(InvalidEnvironmentTarget);
    }
    let argv = match target {
        EnvironmentTarget::Container {
            runtime,
            container,
            shell,
        } => {
            let mut argv: Vec<String> = vec![
                runtime.as_str().into(),
                "exec".into(),
                "-it".into(),
                "-e".into(),
                "TERM=xterm-256color".into(),
                container.clone(),
            ];
            argv.extend(shell_argv(*shell));
            argv
        }
        EnvironmentTarget::ContainerLogs { runtime, container } => vec![
            runtime.as_str().into(),
            "logs".into(),
            "--follow".into(),
            "--tail".into(),
            LOG_TAIL_LINES.to_string(),
            container.clone(),
        ],
        EnvironmentTarget::Pod {
            context,
            namespace,
            pod,
            container,
        } => {
            let mut argv: Vec<String> = vec!["kubectl".into()];
            argv.extend(kube_context_args(context.as_deref()));
            argv.push(format!("--namespace={namespace}"));
            argv.extend(["exec".into(), "-it".into(), pod.clone()]);
            if let Some(container) = container.as_deref().filter(|c| !c.is_empty()) {
                argv.push(format!("--container={container}"));
            }
            argv.push("--".into());
            argv.extend(shell_argv(Some(ContainerShell::Auto)));
            argv
        }
        EnvironmentTarget::PodLogs {
            context,
            namespace,
            pod,
            container,
            previous,
        } => {
            let mut argv: Vec<String> = vec!["kubectl".into()];
            argv.extend(kube_context_args(context.as_deref()));
            argv.push(format!("--namespace={namespace}"));
            argv.extend(["logs".into(), pod.clone()]);
            if let Some(container) = container.as_deref().filter(|c| !c.is_empty()) {
                argv.push(format!("--container={container}"));
            }
            argv.push(format!("--tail={LOG_TAIL_LINES}"));
            argv.push(if *previous == Some(true) {
                "--previous".into()
            } else {
                "--follow".into()
            });
            argv
        }
        EnvironmentTarget::ProxmoxLxc { vmid } => {
            vec!["pct".into(), "enter".into(), vmid.to_string()]
        }
        EnvironmentTarget::ProxmoxSerial { vmid } => {
            vec!["qm".into(), "terminal".into(), vmid.to_string()]
        }
        EnvironmentTarget::LibvirtConsole { domain, uri } => {
            // --force takes over a console another client still holds (an earlier tab,
            // virt-manager); without it virsh refuses and the tab closes at once.
            let mut argv: Vec<String> = vec!["virsh".into()];
            if let Some(uri) = uri {
                argv.extend(["-c".into(), uri.as_str().into()]);
            }
            argv.extend(["console".into(), "--force".into(), domain.clone()]);
            argv
        }
    };
    Ok(argv)
}

/// Quote one argument for a POSIX shell. Plain words stay readable.
pub fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        });
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// [`environment_argv`] as one POSIX command line, for SSH and for copying.
pub fn environment_command_line(
    target: &EnvironmentTarget,
) -> Result<String, InvalidEnvironmentTarget> {
    Ok(environment_argv(target)?
        .iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" "))
}

/// Read-only targets: what they show is output, not a shell to type into.
pub fn is_log_environment(target: &EnvironmentTarget) -> bool {
    matches!(
        target,
        EnvironmentTarget::ContainerLogs { .. } | EnvironmentTarget::PodLogs { .. }
    )
}

/// Tab title for a session into a target.
pub fn environment_title(target: &EnvironmentTarget) -> String {
    match target {
        EnvironmentTarget::Container { container, .. } => container.clone(),
        EnvironmentTarget::ContainerLogs { container, .. } => format!("logs · {container}"),
        EnvironmentTarget::Pod { pod, container, .. } => match container.as_deref() {
            Some(container) if !container.is_empty() => format!("{pod}/{container}"),
            _ => pod.clone(),
        },
        EnvironmentTarget::PodLogs { pod, .. } => format!("logs · {pod}"),
        EnvironmentTarget::ProxmoxLxc { vmid } => format!("CT {vmid}"),
        EnvironmentTarget::ProxmoxSerial { vmid } => format!("VM {vmid} · serial"),
        EnvironmentTarget::LibvirtConsole { domain, .. } => format!("{domain} · console"),
    }
}

impl Validate for EnvironmentTarget {
    /// `environmentTargetSchema` in ws-protocol.ts.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        let check = |key: &str, value: &str, ok: fn(&str) -> bool, source: &str| {
            validate::pattern(&join(path, key), value, ok(value), source)
        };
        let vmid = |vmid: u32| {
            validate::range(
                &join(path, "vmid"),
                vmid,
                PROXMOX_MIN_VMID,
                PROXMOX_MAX_VMID,
            )
        };
        match self {
            EnvironmentTarget::Container { container, .. }
            | EnvironmentTarget::ContainerLogs { container, .. } => check(
                "container",
                container,
                is_container_name,
                CONTAINER_NAME_PATTERN,
            ),
            EnvironmentTarget::Pod {
                context,
                namespace,
                pod,
                container,
            }
            | EnvironmentTarget::PodLogs {
                context,
                namespace,
                pod,
                container,
                ..
            } => {
                if let Some(context) = context {
                    check("context", context, is_kube_context, KUBE_CONTEXT_PATTERN)?;
                }
                check("namespace", namespace, is_kube_name, KUBE_NAME_PATTERN)?;
                check("pod", pod, is_kube_name, KUBE_NAME_PATTERN)?;
                if let Some(container) = container {
                    check(
                        "container",
                        container,
                        is_kube_container,
                        KUBE_CONTAINER_PATTERN,
                    )?;
                }
                Ok(())
            }
            EnvironmentTarget::ProxmoxLxc { vmid: id }
            | EnvironmentTarget::ProxmoxSerial { vmid: id } => vmid(*id),
            EnvironmentTarget::LibvirtConsole { domain, .. } => {
                check("domain", domain, is_libvirt_domain, LIBVIRT_DOMAIN_PATTERN)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery results
// ---------------------------------------------------------------------------

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerPort {
    /// Address the port is published on; absent for an unpublished container port.
    pub host_ip: Option<String>,
    pub host_port: Option<u16>,
    pub container_port: u16,
    pub protocol: String,
}

/// Containerlab's labels on one lab node's container.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerlabLabels {
    pub lab: String,
    pub node: String,
    pub kind: String,
    pub topology_file: Option<String>,
    pub owner: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running, exited, restarting, paused, created, dead … as the engine says.
    pub state: String,
    /// The engine's human status, such as "Up 3 days" or "Exited (0) 2 weeks ago".
    pub status: String,
    pub ports: Vec<ContainerPort>,
    pub compose_project: Option<String>,
    pub containerlab: Option<ContainerlabLabels>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRuntimeEnvironment {
    pub runtime: ContainerRuntime,
    pub version: Option<String>,
    pub containers: Vec<ContainerInfo>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerlabNode {
    /// Node name in the topology.
    pub name: String,
    /// Container name, `clab-<lab>-<node>` unless the topology changes the prefix.
    pub container: String,
    pub kind: String,
    pub image: String,
    pub state: String,
    /// Management addresses without prefix length.
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerlabLab {
    pub name: String,
    pub runtime: ContainerRuntime,
    pub topology_file: Option<String>,
    pub owner: Option<String>,
    pub nodes: Vec<ContainerlabNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerlabEnvironment {
    pub labs: Vec<ContainerlabLab>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubernetesEnvironment {
    pub version: Option<String>,
    pub contexts: Vec<String>,
    pub current_context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxmoxGuestType {
    Qemu,
    Lxc,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxmoxGuest {
    pub vmid: u32,
    pub name: String,
    #[serde(rename = "type")]
    pub guest_type: ProxmoxGuestType,
    /// running, stopped, paused …
    pub status: String,
    pub node: Option<String>,
    /// CPU use as a fraction of `maxcpu` cores.
    pub cpu: Option<f64>,
    pub maxcpu: Option<f64>,
    pub mem: Option<f64>,
    pub maxmem: Option<f64>,
    pub uptime: Option<f64>,
    pub template: Option<bool>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxmoxEnvironment {
    pub version: Option<String>,
    pub guests: Vec<ProxmoxGuest>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibvirtDomain {
    pub name: String,
    /// running, shut off, paused … as virsh says.
    pub state: String,
    pub id: Option<i64>,
    /// The connection that holds it; virsh's default when absent.
    pub uri: Option<LibvirtUri>,
}

/// One hypervisor: the system daemon's domains, then the user session's.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibvirtEnvironment {
    pub version: Option<String>,
    pub domains: Vec<LibvirtDomain>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HostEnvironment {
    Containers(ContainerRuntimeEnvironment),
    Containerlab(ContainerlabEnvironment),
    Kubernetes(KubernetesEnvironment),
    Proxmox(ProxmoxEnvironment),
    Libvirt(LibvirtEnvironment),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentProvider {
    Docker,
    Podman,
    Kubernetes,
    Proxmox,
    Libvirt,
}

/// A tool that is installed but did not answer, such as docker without access to its socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProblem {
    pub provider: EnvironmentProvider,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum EnvironmentsResponse {
    #[serde(rename_all = "camelCase")]
    Ok {
        /// Milliseconds since the epoch.
        sampled_at: u64,
        environments: Vec<HostEnvironment>,
        problems: Vec<EnvironmentProblem>,
    },
    /// The host was not asked, e.g. a router CLI that cannot run commands.
    Unsupported { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubernetesOwner {
    pub kind: String,
    pub name: String,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubernetesPod {
    pub name: String,
    pub namespace: String,
    /// Pending, Running, Succeeded, Failed, Unknown.
    pub phase: String,
    /// Ready containers over all containers, such as "1/2".
    pub ready: String,
    pub restarts: u32,
    /// Why the pod is not running normally, such as CrashLoopBackOff.
    pub reason: Option<String>,
    pub owner: Option<KubernetesOwner>,
    pub containers: Vec<String>,
    pub node: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubernetesPodsResponse {
    pub pods: Vec<KubernetesPod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubernetesNamespacesResponse {
    pub namespaces: Vec<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerlabTopologyResponse {
    pub path: String,
    pub yaml: String,
    /// Saved positions and annotations beside the topology, when there are any.
    pub annotations: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentAddressesResponse {
    /// Reachable addresses from the host, IPv4 first.
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VncEndpoint {
    pub host: String,
    pub port: u16,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LibvirtDisplayResponse {
    /// VNC endpoint as seen from the hypervisor; absent when the domain has none.
    pub vnc: Option<VncEndpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    /// Deletes the container, stopping it first if it runs.
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComposeAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxmoxAction {
    Start,
    Shutdown,
    Reboot,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibvirtAction {
    Start,
    Shutdown,
    Reboot,
    Destroy,
}

/// Power and lifecycle actions offered on discovered things.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EnvironmentAction {
    Container {
        runtime: ContainerRuntime,
        container: String,
        action: ContainerAction,
    },
    Compose {
        runtime: ContainerRuntime,
        project: String,
        action: ComposeAction,
    },
    Proxmox {
        #[serde(rename = "type")]
        guest_type: ProxmoxGuestType,
        vmid: u32,
        action: ProxmoxAction,
    },
    Libvirt {
        uri: Option<LibvirtUri>,
        domain: String,
        action: LibvirtAction,
    },
}

impl Validate for EnvironmentAction {
    /// `actionSchema` of `server/src/routes/environments.ts`.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            EnvironmentAction::Container { container, .. } => validate::pattern(
                &join(path, "container"),
                container,
                is_container_name(container),
                CONTAINER_NAME_PATTERN,
            ),
            EnvironmentAction::Compose { project, .. } => {
                validate::string(&join(path, "project"), project, 1, 128)
            }
            EnvironmentAction::Proxmox { vmid, .. } => validate::range(
                &join(path, "vmid"),
                *vmid,
                PROXMOX_MIN_VMID,
                PROXMOX_MAX_VMID,
            ),
            EnvironmentAction::Libvirt { domain, .. } => validate::pattern(
                &join(path, "domain"),
                domain,
                is_libvirt_domain(domain),
                LIBVIRT_DOMAIN_PATTERN,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentActionResponse {
    pub ok: bool,
    /// What the tool printed, trimmed; shown when it failed.
    pub output: String,
}

/// Body of POST /api/environments/ssh/:connId/port: a port on the far side
/// of an SSH host to open as a local listener (`portSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentPortRequest {
    /// As seen from the SSH host: 127.0.0.1 for a published port, or a guest's address.
    pub host: String,
    pub port: u16,
}

/// `/^[A-Za-z0-9.:_-]+$/`: a name or an IPv4 or IPv6 address, nothing an
/// ssh option or a shell could read.
fn is_port_host(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'_' | b'-'))
}

impl Validate for EnvironmentPortRequest {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        let at = join(path, "host");
        validate::string(&at, &self.host, 1, 253)?;
        validate::pattern(
            &at,
            &self.host,
            is_port_host(&self.host),
            "/^[A-Za-z0-9.:_-]+$/",
        )?;
        validate::range(&join(path, "port"), self.port, 1, u16::MAX)
    }
}

/// A port on the far side of an SSH host, opened as a local listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPortResponse {
    pub forward_id: String,
    pub local_port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_checks_follow_the_typescript_patterns() {
        assert!(is_container_name("web-1"));
        assert!(is_container_name(&"a".repeat(255)));
        assert!(!is_container_name(&"a".repeat(256)));
        assert!(!is_container_name("_web"));
        assert!(is_kube_name("kube-system"));
        assert!(is_kube_name("a.b"));
        assert!(!is_kube_name("a-"));
        assert!(!is_kube_name(&"a".repeat(254)));
        assert!(is_kube_container("worker"));
        assert!(!is_kube_container("a.b"));
        assert!(!is_kube_container(&"a".repeat(64)));
        assert!(is_kube_context("admin@kind"));
        assert!(!is_kube_context("bad\u{7}"));
        assert!(!is_kube_context(""));
        assert!(is_libvirt_domain("my vm"));
        assert!(!is_libvirt_domain(" vm"));
        assert!(!is_libvirt_domain("-vm"));
        assert!(!is_libvirt_domain(&"v".repeat(201)));
    }

    #[test]
    fn opens_ports_only_on_plain_host_names_and_addresses() {
        use crate::validate::parse_value;
        let port = |host: &str, port: u32| {
            parse_value::<EnvironmentPortRequest>(serde_json::json!({ "host": host, "port": port }))
        };
        assert!(port("127.0.0.1", 8080).is_ok());
        assert!(port("fd00::1", 22).is_ok());
        assert!(port("web_1.lab", 443).is_ok());
        assert!(port("", 80).is_err());
        assert!(port("-oProxyCommand=x y", 80).is_err());
        assert!(port("host;id", 80).is_err());
        assert!(port(&"a".repeat(254), 80).is_err());
        assert!(port("host", 0).is_err());
        assert!(port("host", 65_536).is_err());
    }

    #[test]
    fn targets_keep_the_zod_key_order() {
        let json = r#"{"kind":"pod-logs","context":"kind-dev","namespace":"default","pod":"api","container":"c","previous":true}"#;
        let target: EnvironmentTarget = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&target).unwrap(), json);
        let json = r#"{"kind":"libvirt-console","domain":"vm","uri":"qemu:///session"}"#;
        let target: EnvironmentTarget = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&target).unwrap(), json);
    }

    #[test]
    fn discovery_results_read_the_backend_shape() {
        let response: EnvironmentsResponse = serde_json::from_value(serde_json::json!({
            "status": "ok",
            "sampledAt": 1,
            "environments": [
                { "kind": "containers", "runtime": "docker", "containers": [] },
                { "kind": "proxmox", "guests": [{ "vmid": 101, "name": "vm", "type": "qemu", "status": "running", "cpu": 0.25 }] }
            ],
            "problems": [{ "provider": "podman", "message": "permission denied" }]
        }))
        .unwrap();
        let EnvironmentsResponse::Ok { environments, .. } = response else {
            panic!("ok response");
        };
        assert_eq!(environments.len(), 2);
    }
}
