//! What a host runs, read with its own tools (`server/src/environments/discovery.ts`).

use std::sync::LazyLock;

use muxus_core::environments::{
    ComposeAction, ContainerAction, ContainerInfo, ContainerRuntime, ContainerRuntimeEnvironment,
    ContainerlabEnvironment, ContainerlabTopologyResponse, EnvironmentAction,
    EnvironmentActionResponse, EnvironmentProblem, EnvironmentProvider, HostEnvironment,
    KubernetesEnvironment, KubernetesPod, LibvirtAction, LibvirtDomain, LibvirtEnvironment,
    LibvirtUri, ProxmoxAction, ProxmoxEnvironment, ProxmoxGuestType, VncEndpoint,
    is_container_name,
};
use regex::Regex;

use super::parse::{
    DOCKER_PS_FORMAT, INSPECT_NETWORKS_FORMAT, containerlab_labs, parse_cli_version,
    parse_docker_ps, parse_inspect_addresses, parse_kube_namespaces, parse_kube_pods,
    parse_kubectl_version, parse_lines, parse_lxc_addresses, parse_podman_ps, parse_pve_resources,
    parse_pve_version, parse_qemu_agent_addresses, parse_virsh_addresses, parse_virsh_display,
    parse_virsh_list,
};
use super::probe::{Probe, ProbeResult, ProbeResults, ProbeRunner, probe_failure, probe_output};
use crate::error::ApiError;

/// How many lab containers one inspect call asks about.
const MAX_INSPECTED: usize = 256;

/// Everything a host runs that Muxus can enter, and the tools that would not answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiscoveryResult {
    pub environments: Vec<HostEnvironment>,
    pub problems: Vec<EnvironmentProblem>,
}

fn discovery_probes() -> Vec<Probe> {
    vec![
        Probe::new("docker-cli", &["docker", "--version"]),
        Probe::new(
            "docker-ps",
            &[
                "docker",
                "ps",
                "--all",
                "--no-trunc",
                "--format",
                DOCKER_PS_FORMAT,
            ],
        ),
        Probe::new("podman-cli", &["podman", "--version"]),
        Probe::new("podman-ps", &["podman", "ps", "--all", "--format", "json"]),
        Probe::new(
            "kubectl-version",
            &["kubectl", "version", "--client", "-o", "json"],
        ),
        Probe::new(
            "kubectl-contexts",
            &["kubectl", "config", "get-contexts", "-o", "name"],
        ),
        Probe::new("kubectl-current", &["kubectl", "config", "current-context"]),
        Probe::new("pve-version", &["pveversion"]),
        Probe::new(
            "pve-resources",
            &[
                "pvesh",
                "get",
                "/cluster/resources",
                "--type",
                "vm",
                "--output-format",
                "json",
            ],
        ),
        Probe::new("virsh-version", &["virsh", "--version"]),
        Probe::new(
            "virsh-system",
            &["virsh", "-c", "qemu:///system", "list", "--all"],
        ),
        Probe::new(
            "virsh-session",
            &["virsh", "-c", "qemu:///session", "list", "--all"],
        ),
    ]
}

/// podman-docker installs a `docker` that is podman underneath; its containers
/// are podman's, listed once under Podman.
fn docker_is_podman(cli: Option<&ProbeResult>) -> bool {
    match cli {
        Some(ProbeResult::Ran { stdout, stderr, .. }) => format!("{stdout}\n{stderr}")
            .to_lowercase()
            .contains("podman"),
        _ => false,
    }
}

/// Problems worth a line in the panel: the tool exists but would not answer.
fn problem(
    provider: EnvironmentProvider,
    result: Option<&ProbeResult>,
) -> Option<EnvironmentProblem> {
    probe_failure(result).map(|message| EnvironmentProblem { provider, message })
}

async fn run(run: &ProbeRunner, probes: Vec<Probe>) -> Result<ProbeResults, String> {
    run(probes).await
}

/// Everything a host runs that Muxus can enter: one batch, plus one for lab addresses.
pub async fn discover_environments(runner: &ProbeRunner) -> Result<DiscoveryResult, String> {
    let results = run(runner, discovery_probes()).await?;
    let mut environments = Vec::new();
    let mut problems = Vec::new();

    let mut runtimes: Vec<(ContainerRuntime, Vec<ContainerInfo>)> = Vec::new();
    if !docker_is_podman(results.get("docker-cli")) {
        match probe_output(results.get("docker-ps")) {
            Some(ps) => {
                let containers = parse_docker_ps(ps);
                runtimes.push((ContainerRuntime::Docker, containers.clone()));
                environments.push(HostEnvironment::Containers(ContainerRuntimeEnvironment {
                    runtime: ContainerRuntime::Docker,
                    version: parse_cli_version(
                        probe_output(results.get("docker-cli")).unwrap_or(""),
                    ),
                    containers,
                }));
            }
            None => problems.extend(problem(
                EnvironmentProvider::Docker,
                results.get("docker-ps"),
            )),
        }
    }
    match probe_output(results.get("podman-ps")) {
        Some(ps) => {
            let containers = parse_podman_ps(ps);
            runtimes.push((ContainerRuntime::Podman, containers.clone()));
            environments.push(HostEnvironment::Containers(ContainerRuntimeEnvironment {
                runtime: ContainerRuntime::Podman,
                version: parse_cli_version(probe_output(results.get("podman-cli")).unwrap_or("")),
                containers,
            }));
        }
        None => problems.extend(problem(
            EnvironmentProvider::Podman,
            results.get("podman-ps"),
        )),
    }

    // Containerlab labels its nodes; their management addresses need one inspect.
    let lab_runtimes: Vec<&(ContainerRuntime, Vec<ContainerInfo>)> = runtimes
        .iter()
        .filter(|(_, containers)| containers.iter().any(|c| c.containerlab.is_some()))
        .collect();
    if !lab_runtimes.is_empty() {
        let probes = lab_runtimes
            .iter()
            .map(|(runtime, containers)| {
                let mut argv: Vec<String> = vec![
                    runtime.as_str().into(),
                    "inspect".into(),
                    "--format".into(),
                    INSPECT_NETWORKS_FORMAT.into(),
                ];
                argv.extend(
                    containers
                        .iter()
                        .filter(|c| c.containerlab.is_some())
                        .take(MAX_INSPECTED)
                        .map(|c| c.name.clone()),
                );
                Probe {
                    id: format!("{}-inspect", runtime.as_str()),
                    argv,
                }
            })
            .collect();
        let inspected = run(runner, probes).await?;
        let mut labs = Vec::new();
        for (runtime, containers) in lab_runtimes {
            // inspect fails as a whole when one container vanished meanwhile;
            // whatever it printed for the rest still counts.
            let output = match inspected.get(&format!("{}-inspect", runtime.as_str())) {
                Some(ProbeResult::Ran { stdout, .. }) => stdout.as_str(),
                _ => "",
            };
            labs.extend(containerlab_labs(
                *runtime,
                containers,
                &parse_inspect_addresses(output),
            ));
        }
        if !labs.is_empty() {
            environments.push(HostEnvironment::Containerlab(ContainerlabEnvironment {
                labs,
            }));
        }
    }

    if let Some(contexts) = probe_output(results.get("kubectl-contexts")) {
        let names = parse_lines(contexts);
        if !names.is_empty() {
            let current = parse_lines(probe_output(results.get("kubectl-current")).unwrap_or(""))
                .into_iter()
                .next()
                .filter(|current| names.contains(current));
            environments.push(HostEnvironment::Kubernetes(KubernetesEnvironment {
                version: parse_kubectl_version(
                    probe_output(results.get("kubectl-version")).unwrap_or(""),
                ),
                contexts: names,
                current_context: current,
            }));
        }
    }

    match probe_output(results.get("pve-resources")) {
        Some(resources) => environments.push(HostEnvironment::Proxmox(ProxmoxEnvironment {
            version: parse_pve_version(probe_output(results.get("pve-version")).unwrap_or("")),
            guests: parse_pve_resources(resources),
        })),
        None => problems.extend(problem(
            EnvironmentProvider::Proxmox,
            results.get("pve-resources"),
        )),
    }

    environments.extend(libvirt_environments(&results, &mut problems));
    Ok(DiscoveryResult {
        environments,
        problems,
    })
}

static REFUSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)denied|authenticat|polkit|authoriz").expect("refused"));

/// The system daemon is where most hypervisor hosts keep their VMs; a user's
/// own session daemon adds its VMs to the same list. A virsh that reaches
/// neither is a client without a daemon, not a problem worth reporting,
/// unless it was refused.
fn libvirt_environments(
    results: &ProbeResults,
    problems: &mut Vec<EnvironmentProblem>,
) -> Option<HostEnvironment> {
    let version = parse_cli_version(&format!(
        "version {}",
        probe_output(results.get("virsh-version")).unwrap_or("")
    ));
    let system = probe_output(results.get("virsh-system"));
    if system.is_none()
        && let Some(failure) = probe_failure(results.get("virsh-system"))
        && REFUSED.is_match(&failure)
    {
        problems.push(EnvironmentProblem {
            provider: EnvironmentProvider::Libvirt,
            message: failure,
        });
    }
    let system_domains = system.map(parse_virsh_list).unwrap_or_default();
    let session_domains = probe_output(results.get("virsh-session"))
        .map(parse_virsh_list)
        .unwrap_or_default();
    // Some setups answer the session URI with the system daemon: the same VMs twice.
    let duplicate = session_domains.len() == system_domains.len()
        && session_domains.iter().all(|domain| {
            system_domains
                .iter()
                .any(|system| system.name == domain.name)
        });
    let mut domains: Vec<LibvirtDomain> = system_domains
        .into_iter()
        .map(|domain| LibvirtDomain {
            uri: Some(LibvirtUri::System),
            ..domain
        })
        .collect();
    if !duplicate {
        domains.extend(session_domains.into_iter().map(|domain| LibvirtDomain {
            uri: Some(LibvirtUri::Session),
            ..domain
        }));
    }
    if system.is_none() && domains.is_empty() {
        return None;
    }
    Some(HostEnvironment::Libvirt(LibvirtEnvironment {
        version,
        domains,
    }))
}

fn context_args(context: Option<&str>) -> Vec<String> {
    match context {
        Some(context) if !context.is_empty() => vec![format!("--context={context}")],
        _ => Vec::new(),
    }
}

async fn run_one(runner: &ProbeRunner, probe: Probe) -> Result<Option<ProbeResult>, ApiError> {
    let id = probe.id.clone();
    let mut results = runner(vec![probe]).await.map_err(ApiError::internal)?;
    Ok(results.remove(&id))
}

/// stdout of a probe that must succeed; a tool that fails is the host's answer, not a Muxus fault.
fn required(result: Option<&ProbeResult>, tool: &str) -> Result<String, ApiError> {
    if matches!(result, None | Some(ProbeResult::Missing)) {
        return Err(ApiError::new(
            502,
            format!("{tool} is not installed on this host"),
        ));
    }
    match probe_output(result) {
        Some(output) => Ok(output.to_string()),
        None => Err(ApiError::new(
            502,
            probe_failure(result).unwrap_or_else(|| format!("{tool} failed")),
        )),
    }
}

fn owned(argv: Vec<&str>) -> Vec<String> {
    argv.into_iter().map(str::to_string).collect()
}

pub async fn kubernetes_namespaces(
    runner: &ProbeRunner,
    context: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    let mut argv = vec!["kubectl".to_string()];
    argv.extend(context_args(context));
    argv.extend(owned(vec![
        "get",
        "namespaces",
        "-o",
        "name",
        "--request-timeout=10s",
    ]));
    let result = run_one(
        runner,
        Probe {
            id: "namespaces".into(),
            argv,
        },
    )
    .await?;
    Ok(parse_kube_namespaces(&required(
        result.as_ref(),
        "kubectl",
    )?))
}

/// Pods of one namespace, or of all of them when `namespace` is `None`.
pub async fn kubernetes_pods(
    runner: &ProbeRunner,
    context: Option<&str>,
    namespace: Option<&str>,
) -> Result<Vec<KubernetesPod>, ApiError> {
    let mut argv = vec!["kubectl".to_string()];
    argv.extend(context_args(context));
    argv.extend(["get".to_string(), "pods".to_string()]);
    match namespace {
        Some(namespace) => argv.push(format!("--namespace={namespace}")),
        None => argv.push("--all-namespaces".into()),
    }
    argv.extend(owned(vec!["-o", "json", "--request-timeout=10s"]));
    let result = run_one(
        runner,
        Probe {
            id: "pods".into(),
            argv,
        },
    )
    .await?;
    Ok(parse_kube_pods(&required(result.as_ref(), "kubectl")?))
}

/// `^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$`.
fn is_lab_name(lab: &str) -> bool {
    let bytes = lab.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// An absolute topology file path, as containerlab records it (`^/[^\u0000-\u001f\u007f]*\.ya?ml$`).
fn is_topology_path(path: &str) -> bool {
    path.starts_with('/')
        && (path.ends_with(".yml") || path.ends_with(".yaml"))
        && !path
            .chars()
            .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'))
}

const MAX_TOPOLOGY_BYTES: usize = 4 * 1024 * 1024;

/// A running lab's topology file and saved annotations. The path is the one
/// the lab's own containers carry, never one the caller names.
pub async fn containerlab_topology(
    runner: &ProbeRunner,
    runtime: ContainerRuntime,
    lab: &str,
) -> Result<ContainerlabTopologyResponse, ApiError> {
    if !is_lab_name(lab) {
        return Err(ApiError::bad_request("invalid lab name"));
    }
    let located = run_one(
        runner,
        Probe {
            id: "locate".into(),
            argv: vec![
                runtime.as_str().into(),
                "ps".into(),
                "--all".into(),
                "--filter".into(),
                format!("label=containerlab={lab}"),
                "--format".into(),
                r#"{{.Label "clab-topo-file"}}"#.into(),
            ],
        },
    )
    .await?;
    let topology_file = parse_lines(&required(located.as_ref(), runtime.as_str())?)
        .into_iter()
        .find(|line| is_topology_path(line))
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "lab \"{lab}\" does not say where its topology file is"
            ))
        })?;
    let limit = MAX_TOPOLOGY_BYTES.to_string();
    let files = runner(vec![
        Probe {
            id: "yaml".into(),
            argv: vec![
                "head".into(),
                "-c".into(),
                limit.clone(),
                topology_file.clone(),
            ],
        },
        Probe {
            id: "annotations".into(),
            argv: vec![
                "head".into(),
                "-c".into(),
                limit,
                format!("{topology_file}.annotations.json"),
            ],
        },
    ])
    .await
    .map_err(ApiError::internal)?;
    let Some(yaml) = probe_output(files.get("yaml")) else {
        return Err(ApiError::new(
            502,
            format!(
                "could not read {topology_file}: {}",
                probe_failure(files.get("yaml")).unwrap_or_else(|| "unknown error".into())
            ),
        ));
    };
    let annotations = probe_output(files.get("annotations"))
        .map(|text| muxus_core::js::js_trim(text).to_string())
        .filter(|text| !text.is_empty());
    Ok(ContainerlabTopologyResponse {
        path: topology_file,
        yaml: yaml.to_string(),
        annotations,
    })
}

fn virsh_args(uri: Option<LibvirtUri>) -> Vec<String> {
    let mut argv = vec!["virsh".to_string()];
    if let Some(uri) = uri {
        argv.extend(["-c".into(), uri.as_str().into()]);
    }
    argv
}

/// A domain's addresses: from the guest's DHCP lease, the guest agent, else the ARP table.
pub async fn libvirt_addresses(
    runner: &ProbeRunner,
    uri: Option<LibvirtUri>,
    domain: &str,
) -> Result<Vec<String>, ApiError> {
    let probe = |id: &str, extra: &[&str]| {
        let mut argv = virsh_args(uri);
        argv.extend(["domifaddr".into(), domain.to_string()]);
        argv.extend(extra.iter().map(|arg| arg.to_string()));
        Probe {
            id: id.into(),
            argv,
        }
    };
    let results = runner(vec![
        probe("lease", &[]),
        probe("agent", &["--source", "agent"]),
        probe("arp", &["--source", "arp"]),
    ])
    .await
    .map_err(ApiError::internal)?;
    for id in ["lease", "agent", "arp"] {
        let addresses = parse_virsh_addresses(probe_output(results.get(id)).unwrap_or(""));
        if !addresses.is_empty() {
            return Ok(addresses);
        }
    }
    if matches!(results.get("lease"), None | Some(ProbeResult::Missing)) {
        return Err(ApiError::new(502, "virsh is not installed on this host"));
    }
    Ok(Vec::new())
}

pub async fn libvirt_display(
    runner: &ProbeRunner,
    uri: Option<LibvirtUri>,
    domain: &str,
) -> Result<Option<VncEndpoint>, ApiError> {
    let mut argv = virsh_args(uri);
    argv.extend(["domdisplay".into(), domain.to_string()]);
    let result = run_one(
        runner,
        Probe {
            id: "display".into(),
            argv,
        },
    )
    .await?;
    Ok(parse_virsh_display(&required(result.as_ref(), "virsh")?))
}

pub async fn proxmox_addresses(
    runner: &ProbeRunner,
    guest_type: ProxmoxGuestType,
    vmid: u32,
) -> Result<Vec<String>, ApiError> {
    if guest_type == ProxmoxGuestType::Lxc {
        let result = run_one(
            runner,
            Probe {
                id: "lxc".into(),
                argv: owned(vec!["lxc-info", "-n", &vmid.to_string(), "-iH"]),
            },
        )
        .await?;
        return Ok(parse_lxc_addresses(&required(result.as_ref(), "lxc-info")?));
    }
    let result = run_one(
        runner,
        Probe {
            id: "agent".into(),
            argv: owned(vec![
                "qm",
                "guest",
                "cmd",
                &vmid.to_string(),
                "network-get-interfaces",
            ]),
        },
    )
    .await?;
    match probe_output(result.as_ref()) {
        Some(output) => Ok(parse_qemu_agent_addresses(output)),
        None => Err(ApiError::new(
            502,
            if matches!(result, Some(ProbeResult::Missing)) {
                "qm is not installed on this host".to_string()
            } else {
                format!(
                    "the QEMU guest agent did not answer ({})",
                    probe_failure(result.as_ref()).unwrap_or_else(|| "no reply".into())
                )
            },
        )),
    }
}

/// `^[a-z0-9][a-z0-9_-]{0,127}$`.
fn is_compose_project(project: &str) -> bool {
    let bytes = project.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn container_verb(action: ContainerAction) -> &'static str {
    match action {
        ContainerAction::Start => "start",
        ContainerAction::Stop => "stop",
        ContainerAction::Restart => "restart",
        ContainerAction::Pause => "pause",
        ContainerAction::Unpause => "unpause",
        ContainerAction::Remove => "rm",
    }
}

/// The command behind one power or lifecycle action.
pub fn action_argv(action: &EnvironmentAction) -> Result<Vec<String>, ApiError> {
    Ok(match action {
        EnvironmentAction::Container {
            runtime,
            container,
            action,
        } => {
            if !is_container_name(container) {
                return Err(ApiError::bad_request("invalid container name"));
            }
            // A running container is stopped first; volumes it named are kept.
            if *action == ContainerAction::Remove {
                owned(vec![runtime.as_str(), "rm", "--force", container])
            } else {
                owned(vec![runtime.as_str(), container_verb(*action), container])
            }
        }
        EnvironmentAction::Compose {
            runtime,
            project,
            action,
        } => {
            if !is_compose_project(project) {
                return Err(ApiError::bad_request("invalid compose project"));
            }
            let verb = match action {
                ComposeAction::Start => "start",
                ComposeAction::Stop => "stop",
                ComposeAction::Restart => "restart",
            };
            owned(vec![
                runtime.as_str(),
                "compose",
                "--project-name",
                project,
                verb,
            ])
        }
        EnvironmentAction::Proxmox {
            guest_type,
            vmid,
            action,
        } => {
            let tool = if *guest_type == ProxmoxGuestType::Qemu {
                "qm"
            } else {
                "pct"
            };
            let verb = match action {
                ProxmoxAction::Start => "start",
                ProxmoxAction::Shutdown => "shutdown",
                ProxmoxAction::Reboot => "reboot",
                ProxmoxAction::Stop => "stop",
            };
            owned(vec![tool, verb, &vmid.to_string()])
        }
        EnvironmentAction::Libvirt {
            uri,
            domain,
            action,
        } => {
            let verb = match action {
                LibvirtAction::Start => "start",
                LibvirtAction::Shutdown => "shutdown",
                LibvirtAction::Reboot => "reboot",
                LibvirtAction::Destroy => "destroy",
            };
            let mut argv = virsh_args(*uri);
            argv.extend([verb.to_string(), domain.clone()]);
            argv
        }
    })
}

pub async fn run_environment_action(
    runner: &ProbeRunner,
    action: &EnvironmentAction,
) -> Result<EnvironmentActionResponse, ApiError> {
    let argv = action_argv(action)?;
    let tool = argv[0].clone();
    let result = run_one(
        runner,
        Probe {
            id: "action".into(),
            argv,
        },
    )
    .await?;
    match result {
        None | Some(ProbeResult::Missing) => Ok(EnvironmentActionResponse {
            ok: false,
            output: format!("{tool} is not installed on this host"),
        }),
        Some(ProbeResult::Ran {
            exit_code,
            stdout,
            stderr,
        }) => {
            let output = format!("{stdout}\n{stderr}");
            let output = muxus_core::js::js_trim(&output);
            Ok(EnvironmentActionResponse {
                ok: exit_code == 0,
                output: muxus_core::js::utf16_prefix(output, 2000).to_string(),
            })
        }
    }
}
