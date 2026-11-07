//! Kubeconfig contexts on a host, and for an opened one the pods of a
//! namespace grouped by workload (`components/environments/KubernetesSection.tsx`).
//! Pods are read when a context opens, not with the rest of the host: a
//! cluster can be slow or far away.

use std::sync::LazyLock;

use muxus_core::environments::{
    EnvironmentTarget, KubernetesEnvironment, KubernetesPod, environment_command_line,
};
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{Status, Theme};
use muxus_ui::widgets::progress::CircularProgress;
use regex::Regex;

use super::model::{EnvironmentHost, EnvironmentKind, HostEnvironmentsEntry, pod_workload};
use super::panel::{EnvironmentsPanel, MenuEntry, PanelAction, PodsState};
use super::sessions::kubus_pod_route;
use super::tree::{Health, LEVEL, LineSpec, note_line, status_dot, tree_line};

const ALL_NAMESPACES: &str = "";

static TROUBLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)Error|BackOff|Crash|Evicted|OOM").expect("pod trouble"));

fn pod_health(pod: &KubernetesPod) -> Health {
    if pod.phase == "Succeeded" {
        return Health::Off;
    }
    if pod.phase == "Failed" || TROUBLE.is_match(pod.reason.as_deref().unwrap_or("")) {
        return Health::Down;
    }
    let mut parts = pod.ready.split('/');
    let ready = parts.next();
    let total = parts.next();
    if pod.phase == "Running" && ready == total {
        return Health::Up;
    }
    Health::Warn
}

impl EnvironmentsPanel {
    pub(super) fn kubernetes_section(
        &mut self,
        entry: &HostEnvironmentsEntry,
        environment: &KubernetesEnvironment,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let host = &entry.host;
        // Kubus reads this computer's kubeconfig: a remote host's contexts are not there.
        let kubus = host.profile.is_none() && self.hub.read(cx).kubus();
        let key = format!("{}/kubernetes", host.key);
        let open = self.is_open(&key, true);
        let count = environment.contexts.len();
        let mut spec = LineSpec::new(key.clone(), 1, EnvironmentKind::Kubernetes.label())
            .icon(self.logo(EnvironmentKind::Kubernetes, window, cx))
            .maybe_detail(environment.version.as_ref().map(|v| format!("kubectl {v}")))
            .status_text(
                format!("{count} context{}", if count == 1 { "" } else { "s" }),
                theme,
            )
            .open(open);
        if kubus {
            spec = spec.action(self.line_action(
                format!("env-kubus-{key}"),
                "Open Kubus",
                "OpenInNewOutlined",
                PanelAction::Kubus("/".into()),
                false,
                cx,
            ));
        }
        let click = self.line_click(Some((key, true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if open {
            for context in &environment.contexts {
                let current = environment.current_context.as_deref() == Some(context.as_str());
                lines.extend(self.context_lines(entry, context, current, kubus, theme, cx));
            }
        }
        lines
    }

    /// Start reading what an opened context's list should show, unless it is already read
    /// for this list, read-again request and host reading. Reading the same list again (a
    /// refresh) keeps it on screen until the new one arrives; another namespace starts over.
    fn ensure_pods(
        &mut self,
        entry: &HostEnvironmentsEntry,
        context: &str,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{}/kubernetes/{context}", entry.host.key);
        let pods = self.pods.entry(key.clone()).or_default();
        let list = format!("{}\n{context}\n{}", entry.host.key, pods.namespace);
        let wanted = (list.clone(), pods.generation, entry.sampled_at);
        if pods.requested.as_ref() == Some(&wanted) {
            return;
        }
        let again = pods
            .requested
            .as_ref()
            .is_some_and(|(shown, _, _)| *shown == list)
            && matches!(pods.state, PodsState::Ready(_));
        if !again {
            pods.state = PodsState::Loading;
        }
        pods.requested = Some(wanted);
        let ask_namespaces = !pods.namespaces_asked;
        pods.namespaces_asked = true;
        let namespace = (!pods.namespace.is_empty()).then(|| pods.namespace.clone());
        let host = entry.host.clone();
        let source = self.hub.update(cx, |hub, cx| hub.source(&host, true, cx));
        let backend = self.hub.read(cx).backend().clone();
        let context = context.to_string();
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let source = match source.await {
                Ok(Some(source)) => source,
                Ok(None) => {
                    let message = format!("{} is not connected", host.label);
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pods) = this.pods.get_mut(&key) {
                            pods.state = PodsState::Error(message);
                        }
                        cx.notify();
                    });
                    return;
                }
                Err(message) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pods) = this.pods.get_mut(&key) {
                            pods.state = PodsState::Error(message);
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            if ask_namespaces {
                // Listing namespaces may be forbidden; a namespace can still be read.
                if let Ok(response) = backend
                    .environment_namespaces(source.clone(), Some(context.clone()))
                    .await
                {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(pods) = this.pods.get_mut(&key) {
                            pods.namespaces = response.namespaces;
                        }
                        cx.notify();
                    });
                }
            }
            let result = backend
                .environment_pods(source, Some(context), namespace)
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(pods) = this.pods.get_mut(&key) {
                    pods.state = match result {
                        Ok(response) => PodsState::Ready(response.pods),
                        Err(error) => PodsState::Error(error.message),
                    };
                }
                cx.notify();
            });
        });
        if let Some(pods) = self.pods.get_mut(&task_key) {
            pods._task = Some(task);
        }
    }

    fn context_lines(
        &mut self,
        entry: &HostEnvironmentsEntry,
        context: &str,
        current: bool,
        kubus: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let p = theme.palette;
        let host = &entry.host;
        let key = format!("{}/kubernetes/{context}", host.key);
        // Only the current context opens by itself; the others may be far-away clusters.
        let open = self.is_open(&key, current);
        if open {
            self.ensure_pods(entry, context, cx);
        }
        let (namespace, choices) = {
            let pods = self.pods.entry(key.clone()).or_default();
            let mut choices: Vec<String> = vec![ALL_NAMESPACES.into(), "default".into()];
            for name in pods
                .namespaces
                .iter()
                .chain(std::iter::once(&pods.namespace))
            {
                if !choices.contains(name) {
                    choices.push(name.clone());
                }
            }
            (pods.namespace.clone(), choices)
        };
        let namespace_label = |value: &str| {
            if value == ALL_NAMESPACES {
                "All namespaces".to_string()
            } else {
                value.to_string()
            }
        };
        let mut spec = LineSpec::new(key.clone(), 2, context.to_string())
            .icon(status_dot(
                if current { Health::Up } else { Health::Off },
                theme,
            ))
            .maybe_detail(current.then_some("current"));
        spec.emphasis = current;
        spec = spec.open(open);
        if open {
            let entries: Vec<MenuEntry> = choices
                .iter()
                .map(|choice| {
                    MenuEntry::new(
                        namespace_label(choice),
                        PanelAction::Namespace(key.clone(), choice.clone()),
                    )
                })
                .collect();
            let entity = cx.entity().downgrade();
            let picker = div()
                .id(ElementId::Name(format!("env-namespace-{key}").into()))
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    label(namespace_label(&namespace))
                        .size(11.)
                        .color(p.text.secondary),
                )
                .child(icon("ArrowDropDown", 16., p.text.secondary))
                .on_click(move |event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    let position = event
                        .mouse_position()
                        .unwrap_or_else(|| window.mouse_position());
                    let entries = entries.clone();
                    let _ = entity.update(cx, |this, cx| this.open_menu(position, entries, cx));
                });
            spec = spec.status(picker).action(self.line_action(
                format!("env-pods-reload-{key}"),
                "Read the pods again",
                "RefreshOutlined",
                PanelAction::ReloadPods(key.clone()),
                false,
                cx,
            ));
        }
        if kubus {
            spec = spec.action(self.line_action(
                format!("env-kubus-{key}"),
                "Open in Kubus",
                "OpenInNewOutlined",
                PanelAction::Kubus("/".into()),
                false,
                cx,
            ));
        }
        let click = self.line_click(Some((key.clone(), current)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if !open {
            return lines;
        }
        let filter = self.filter.clone();
        let show_stopped = self.show_stopped;
        let pods = self.pods.get(&key);
        match pods.map(|pods| &pods.state) {
            Some(PodsState::Loading) | None => {
                lines.push(
                    div()
                        .pl(px(8. + 3. * LEVEL + 22.))
                        .py(px(6.))
                        .child(CircularProgress::new(
                            SharedString::from(format!("env-pods-loading-{key}")),
                            14.,
                            p.primary.main,
                        ))
                        .into_any_element(),
                );
            }
            Some(PodsState::Error(message)) => {
                lines.push(note_line(
                    3,
                    message.clone(),
                    theme.status_text_color(Status::Error),
                ));
            }
            Some(PodsState::Ready(list)) => {
                let visible: Vec<KubernetesPod> = list
                    .iter()
                    .filter(|pod| {
                        (filter.is_empty()
                            || pod.name.contains(&filter)
                            || pod.namespace.contains(&filter))
                            && (show_stopped || pod.phase != "Succeeded")
                    })
                    .cloned()
                    .collect();
                if visible.is_empty() {
                    lines.push(note_line(
                        3,
                        format!(
                            "No pods{}.",
                            if namespace.is_empty() {
                                String::new()
                            } else {
                                format!(" in {namespace}")
                            }
                        ),
                        p.text.secondary,
                    ));
                }
                let mut groups: Vec<(String, String, Vec<KubernetesPod>)> = Vec::new();
                for pod in visible {
                    let workload = pod_workload(&pod);
                    let id = match &workload {
                        Some((kind, name)) => format!("{}/{kind}/{name}", pod.namespace),
                        None => format!("{}//", pod.namespace),
                    };
                    let group_label = workload
                        .map(|(_, name)| name)
                        .unwrap_or_else(|| "Standalone pods".into());
                    match groups.iter_mut().find(|(known, _, _)| *known == id) {
                        Some((_, _, pods)) => pods.push(pod),
                        None => groups.push((id, group_label, vec![pod])),
                    }
                }
                for (id, group_label, pods) in groups {
                    lines.extend(self.workload_lines(
                        host,
                        &format!("{key}/{id}"),
                        &group_label,
                        &pods,
                        context,
                        kubus,
                        namespace.is_empty(),
                        theme,
                        cx,
                    ));
                }
            }
        }
        lines
    }

    #[allow(clippy::too_many_arguments)]
    fn workload_lines(
        &mut self,
        host: &EnvironmentHost,
        id: &str,
        group_label: &str,
        pods: &[KubernetesPod],
        context: &str,
        kubus: bool,
        show_namespace: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let open = self.is_open(id, true);
        let ready = pods
            .iter()
            .filter(|pod| pod_health(pod) == Health::Up)
            .count();
        let workload = pods.first().and_then(pod_workload);
        let detail = format!(
            "{}{}",
            workload.map(|(kind, _)| kind).unwrap_or_default(),
            match (show_namespace, pods.first()) {
                (true, Some(pod)) => format!(" · {}", pod.namespace),
                _ => String::new(),
            }
        );
        let spec = LineSpec::new(id.to_string(), 3, group_label.to_string())
            .maybe_detail((!detail.is_empty()).then_some(detail))
            .status_text(format!("{ready} / {} ready", pods.len()), theme)
            .open(open);
        let click = self.line_click(Some((id.to_string(), true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if open {
            for pod in pods {
                lines.extend(self.pod_line(host, pod, context, kubus, theme, cx));
            }
        }
        lines
    }

    fn pod_line(
        &mut self,
        host: &EnvironmentHost,
        pod: &KubernetesPod,
        context: &str,
        kubus: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let health = pod_health(pod);
        let running = pod.phase == "Running";
        let shell = |container: Option<&String>| {
            PanelAction::Target(
                host.clone(),
                EnvironmentTarget::Pod {
                    context: Some(context.to_string()),
                    namespace: pod.namespace.clone(),
                    pod: pod.name.clone(),
                    container: container.cloned(),
                },
            )
        };
        let logs = |previous: bool, container: Option<&String>| {
            PanelAction::Target(
                host.clone(),
                EnvironmentTarget::PodLogs {
                    context: Some(context.to_string()),
                    namespace: pod.namespace.clone(),
                    pod: pod.name.clone(),
                    container: container.cloned(),
                    previous: previous.then_some(true),
                },
            )
        };
        let several = pod.containers.len() > 1;
        let mut menu = Vec::new();
        if several && running {
            for container in &pod.containers {
                menu.push(MenuEntry::new(
                    format!("Shell in {container}"),
                    shell(Some(container)),
                ));
            }
        }
        if several {
            for container in &pod.containers {
                menu.push(MenuEntry::new(
                    format!("Logs of {container}"),
                    logs(false, Some(container)),
                ));
            }
        }
        let previous = MenuEntry::new("Logs of the previous run", logs(true, None));
        menu.push(if kubus { previous } else { previous.divider() });
        if kubus {
            menu.push(
                MenuEntry::new(
                    "Open in Kubus",
                    PanelAction::Kubus(kubus_pod_route(context, &pod.namespace, &pod.name)),
                )
                .divider(),
            );
        }
        menu.push(MenuEntry::new(
            "Copy exec command",
            PanelAction::Copy(
                environment_command_line(&EnvironmentTarget::Pod {
                    context: Some(context.to_string()),
                    namespace: pod.namespace.clone(),
                    pod: pod.name.clone(),
                    container: None,
                })
                .unwrap_or_default(),
            ),
        ));
        let id = format!(
            "{}/kubernetes/{context}/{}/{}",
            host.key, pod.namespace, pod.name
        );
        let mut spec = LineSpec::new(id.clone(), 4, pod.name.clone())
            .icon(status_dot(health, theme))
            .maybe_detail((pod.restarts > 0).then(|| {
                format!(
                    "{} restart{}",
                    pod.restarts,
                    if pod.restarts == 1 { "" } else { "s" }
                )
            }))
            .status_text(pod.ready.clone(), theme)
            .hint("Namespace", pod.namespace.clone())
            .hint(
                "Phase",
                match &pod.reason {
                    Some(reason) => format!("{} ({reason})", pod.phase),
                    None => pod.phase.clone(),
                },
            )
            .hint("Ready", pod.ready.clone())
            .hint("Restarts", pod.restarts.to_string())
            .hint("Node", pod.node.clone())
            .hint(
                "Owner",
                pod.owner
                    .as_ref()
                    .map(|owner| format!("{}/{}", owner.kind, owner.name)),
            )
            .hint("Containers", pod.containers.join(", "))
            .hint("Created", pod.created_at.clone());
        spec.title_mono = true;
        spec.dim = health == Health::Off;
        if running {
            spec = spec.action(self.line_action(
                format!("env-pod-shell-{id}"),
                "Open shell",
                "TerminalOutlined",
                shell(None),
                false,
                cx,
            ));
        }
        spec = spec.action(self.line_action(
            format!("env-pod-logs-{id}"),
            "Follow logs",
            "SubjectOutlined",
            logs(false, None),
            false,
            cx,
        ));
        if let Some(menu) = self.line_menu(
            format!("env-pod-menu-{id}"),
            format!("More actions for {}", pod.name),
            menu,
            cx,
        ) {
            spec = spec.action(menu);
        }
        let open = if running {
            shell(None)
        } else {
            logs(false, None)
        };
        let click = self.line_click(None, Some(open), cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if let Some(reason) = &pod.reason {
            let color = theme.status_text_color(if health == Health::Down {
                Status::Error
            } else {
                Status::Warning
            });
            lines.push(note_line(4, reason.clone(), color));
        }
        lines
    }
}
