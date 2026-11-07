//! Containers, clusters, labs and VMs (`server/src/routes/environments.ts`): on the computer
//! running Muxus ([`EnvironmentSource::Local`]) or on the far end of a live SSH connection
//! ([`EnvironmentSource::Ssh`]). Each request runs the host's own tools over one exec channel of
//! that connection; nothing is installed.
//!
//! Automatic discovery (`auto`) skips a connection whose server never ran a command, the way
//! the shell probe tells: some network devices drop the whole connection when a second channel
//! asks to exec.

use std::sync::Arc;

use muxus_core::api::{ForwardRequest, ForwardStatus, ForwardType};
use muxus_core::environments::{
    ContainerRuntime, ContainerlabTopologyResponse, EnvironmentAction, EnvironmentActionResponse,
    EnvironmentAddressesResponse, EnvironmentPortRequest, EnvironmentPortResponse,
    EnvironmentsResponse, KUBE_CONTEXT_PATTERN, KUBE_NAME_PATTERN, KubernetesNamespacesResponse,
    KubernetesPodsResponse, LIBVIRT_DOMAIN_PATTERN, LibvirtDisplayResponse, LibvirtUri,
    PROXMOX_MAX_VMID, PROXMOX_MIN_VMID, ProxmoxGuestType, is_kube_context, is_kube_name,
    is_libvirt_domain,
};
use muxus_core::validate::Validate;
use muxus_ssh::connection::ConnectionLeaseOwner;
use muxus_ssh::connection::manager::ConnectionLease;

use crate::adapters::ConnectionCommand;
use crate::backend::{Backend, Shared};
use crate::environments::discovery::{
    containerlab_topology, discover_environments, kubernetes_namespaces, kubernetes_pods,
    libvirt_addresses, libvirt_display, proxmox_addresses, run_environment_action,
};
use crate::environments::probe::{ProbeRunner, local_probe_runner, remote_probe_runner};
use crate::environments::{EnvironmentSource, kubus_available};
use crate::error::{ApiError, ApiResult};

/// A runner for `source`, with the lease that keeps its connection while it runs.
struct Runner {
    run: ProbeRunner,
    lease: Option<ConnectionLease>,
}

impl Runner {
    fn release(self) {
        if let Some(lease) = self.lease {
            lease.release();
        }
    }
}

fn runner(shared: &Shared, source: &EnvironmentSource) -> ApiResult<Runner> {
    match source {
        EnvironmentSource::Local => Ok(Runner {
            run: local_probe_runner(Some(shared.local_environment().vars)),
            lease: None,
        }),
        EnvironmentSource::Ssh { conn_id } => {
            let lease = shared
                .connections
                .acquire(conn_id, ConnectionLeaseOwner::Environments)
                .ok_or_else(|| ApiError::not_found("connection not found"))?;
            let command = ConnectionCommand(lease.connection().clone());
            Ok(Runner {
                run: remote_probe_runner(Arc::new(command)),
                lease: Some(lease),
            })
        }
    }
}

fn pattern_error(field: &str, pattern: &str) -> ApiError {
    ApiError::bad_request(format!(
        "Invalid string: must match pattern {pattern} at {field}"
    ))
}

fn check_context(context: Option<&str>) -> ApiResult<()> {
    match context {
        Some(context) if !is_kube_context(context) => {
            Err(pattern_error("context", KUBE_CONTEXT_PATTERN))
        }
        _ => Ok(()),
    }
}

fn check_domain(domain: &str) -> ApiResult<()> {
    if is_libvirt_domain(domain) {
        Ok(())
    } else {
        Err(pattern_error("domain", LIBVIRT_DOMAIN_PATTERN))
    }
}

/// A free loopback port for a new listener.
async fn free_local_port() -> ApiResult<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

impl Backend {
    /// Run `work` with a runner for `source` on the backend runtime.
    async fn with_environment_runner<T, F, Fut>(
        &self,
        source: EnvironmentSource,
        work: F,
    ) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(ProbeRunner) -> Fut + Send + 'static,
        Fut: Future<Output = ApiResult<T>> + Send + 'static,
    {
        self.run(move |shared| async move {
            let runner = runner(&shared, &source)?;
            let result = work(runner.run.clone()).await;
            runner.release();
            result
        })
        .await
    }

    /// `GET /api/environments/local` and `/api/environments/ssh/:connId`. Requests for the
    /// same host and mode share one reading.
    pub async fn environments(
        &self,
        source: EnvironmentSource,
        auto: bool,
    ) -> ApiResult<EnvironmentsResponse> {
        self.run(move |shared| async move {
            let reading = shared.environments.read(&source, auto, || {
                let shared = shared.clone();
                let source = source.clone();
                async move {
                    let runner = runner(&shared, &source)?;
                    if auto
                        && let Some(lease) = &runner.lease
                        && !lease.connection().ran_command()
                    {
                        runner.release();
                        return Ok(EnvironmentsResponse::Unsupported {
                            message: "The host does not run commands.".into(),
                        });
                    }
                    let result = discover_environments(&runner.run).await;
                    runner.release();
                    let found = result.map_err(ApiError::internal)?;
                    Ok(EnvironmentsResponse::Ok {
                        sampled_at: chrono::Utc::now().timestamp_millis().max(0) as u64,
                        environments: found.environments,
                        problems: found.problems,
                    })
                }
            });
            reading.await
        })
        .await
    }

    /// `GET …/kubernetes/namespaces`.
    pub async fn environment_namespaces(
        &self,
        source: EnvironmentSource,
        context: Option<String>,
    ) -> ApiResult<KubernetesNamespacesResponse> {
        check_context(context.as_deref())?;
        self.with_environment_runner(source, move |run| async move {
            Ok(KubernetesNamespacesResponse {
                namespaces: kubernetes_namespaces(&run, context.as_deref()).await?,
            })
        })
        .await
    }

    /// `GET …/kubernetes/pods`: `namespace` `None` lists every namespace.
    pub async fn environment_pods(
        &self,
        source: EnvironmentSource,
        context: Option<String>,
        namespace: Option<String>,
    ) -> ApiResult<KubernetesPodsResponse> {
        check_context(context.as_deref())?;
        if let Some(namespace) = &namespace
            && !is_kube_name(namespace)
        {
            return Err(pattern_error("namespace", KUBE_NAME_PATTERN));
        }
        self.with_environment_runner(source, move |run| async move {
            Ok(KubernetesPodsResponse {
                pods: kubernetes_pods(&run, context.as_deref(), namespace.as_deref()).await?,
            })
        })
        .await
    }

    /// `GET …/containerlab/topology`.
    pub async fn environment_topology(
        &self,
        source: EnvironmentSource,
        runtime: ContainerRuntime,
        lab: String,
    ) -> ApiResult<ContainerlabTopologyResponse> {
        if lab.is_empty() || lab.len() > 128 {
            return Err(ApiError::bad_request("invalid lab name"));
        }
        self.with_environment_runner(source, move |run| async move {
            containerlab_topology(&run, runtime, &lab).await
        })
        .await
    }

    /// `GET …/libvirt/addresses`.
    pub async fn environment_libvirt_addresses(
        &self,
        source: EnvironmentSource,
        uri: Option<LibvirtUri>,
        domain: String,
    ) -> ApiResult<EnvironmentAddressesResponse> {
        check_domain(&domain)?;
        self.with_environment_runner(source, move |run| async move {
            Ok(EnvironmentAddressesResponse {
                addresses: libvirt_addresses(&run, uri, &domain).await?,
            })
        })
        .await
    }

    /// `GET …/libvirt/display`.
    pub async fn environment_libvirt_display(
        &self,
        source: EnvironmentSource,
        uri: Option<LibvirtUri>,
        domain: String,
    ) -> ApiResult<LibvirtDisplayResponse> {
        check_domain(&domain)?;
        self.with_environment_runner(source, move |run| async move {
            Ok(LibvirtDisplayResponse {
                vnc: libvirt_display(&run, uri, &domain).await?,
            })
        })
        .await
    }

    /// `GET …/proxmox/addresses`.
    pub async fn environment_proxmox_addresses(
        &self,
        source: EnvironmentSource,
        guest_type: ProxmoxGuestType,
        vmid: u32,
    ) -> ApiResult<EnvironmentAddressesResponse> {
        if !(PROXMOX_MIN_VMID..=PROXMOX_MAX_VMID).contains(&vmid) {
            return Err(ApiError::bad_request("invalid vmid"));
        }
        self.with_environment_runner(source, move |run| async move {
            Ok(EnvironmentAddressesResponse {
                addresses: proxmox_addresses(&run, guest_type, vmid).await?,
            })
        })
        .await
    }

    /// `POST …/actions`: a power or lifecycle action.
    pub async fn environment_action(
        &self,
        source: EnvironmentSource,
        action: EnvironmentAction,
    ) -> ApiResult<EnvironmentActionResponse> {
        let mut action = action;
        action.validate().map_err(ApiError::invalid)?;
        self.with_environment_runner(source, move |run| async move {
            run_environment_action(&run, &action).await
        })
        .await
    }

    /// `POST /api/environments/ssh/:connId/ports`: open a port on the far side of an SSH host
    /// on this computer (a published container port, a hypervisor's web interface, a guest's
    /// service). An existing forward to the same place is reused.
    pub async fn environment_port(
        &self,
        conn_id: &str,
        request: EnvironmentPortRequest,
    ) -> ApiResult<EnvironmentPortResponse> {
        let mut request = request;
        request.validate().map_err(ApiError::invalid)?;
        let conn_id = conn_id.to_string();
        self.run(move |shared| async move {
            let existing = shared
                .forwards
                .list(Some(&conn_id))
                .into_iter()
                .find(|forward| {
                    forward.kind == ForwardType::Local
                        && forward.status == ForwardStatus::Active
                        && forward.target_host.as_deref() == Some(request.host.as_str())
                        && forward.target_port == Some(request.port)
                });
            if let Some(existing) = existing {
                return Ok(EnvironmentPortResponse {
                    forward_id: existing.id,
                    local_port: existing.bind_port,
                });
            }
            let bind_port = free_local_port().await?;
            let info = shared
                .forwards
                .start(ForwardRequest {
                    conn_id,
                    kind: ForwardType::Local,
                    bind_port,
                    target_host: Some(request.host),
                    target_port: Some(request.port),
                    tunnel_id: None,
                    independent: None,
                })
                .await?;
            Ok(EnvironmentPortResponse {
                forward_id: info.id,
                local_port: info.bind_port,
            })
        })
        .await
    }

    /// Whether the Kubus desktop app is installed (`muxus:kubus:available`).
    pub async fn kubus_available(&self) -> bool {
        self.run(|_| async { Ok(kubus_available().await) })
            .await
            .unwrap_or(false)
    }
}
