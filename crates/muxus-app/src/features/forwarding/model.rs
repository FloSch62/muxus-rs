//! What the forwarding panel shows and sends (`components/ForwardingPanel.tsx`,
//! `api/tunnels.ts`): how forwards group under tunnels and connections, the
//! words of each row, and the requests behind each button.

use std::collections::HashMap;

use muxus_core::api::{
    ConfigForward, ConnectionInfo, ForwardInfo, ForwardOrigin, ForwardRequest, ForwardStatus,
    ForwardType, HostUpsertRequest, SshHostEntry, TunnelInput, TunnelRecord,
};

pub use crate::features::host_editor::logic::forwards::{describe_forward, forward_flag};

/// A per-tunnel switch, offered in the row menu as well as the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelSwitch {
    AutoStart,
    AutoReconnect,
}

impl TunnelSwitch {
    /// `TUNNEL_SWITCHES`, in menu order.
    pub const ALL: [TunnelSwitch; 2] = [TunnelSwitch::AutoStart, TunnelSwitch::AutoReconnect];

    pub fn label(self) -> &'static str {
        match self {
            TunnelSwitch::AutoStart => "Start when Muxus starts",
            TunnelSwitch::AutoReconnect => "Reconnect automatically",
        }
    }

    pub fn is_on(self, tunnel: &TunnelRecord) -> bool {
        match self {
            TunnelSwitch::AutoStart => tunnel.auto_start,
            TunnelSwitch::AutoReconnect => tunnel.auto_reconnect,
        }
    }
}

/// The rule of a forward or tunnel as `describeForward` reads it.
pub fn rule_of(
    kind: ForwardType,
    bind_port: u16,
    target_host: Option<&str>,
    target_port: Option<u16>,
) -> ConfigForward {
    ConfigForward {
        kind,
        bind_port,
        target_host: target_host.map(str::to_owned),
        target_port,
    }
}

pub fn describe_tunnel(tunnel: &TunnelRecord) -> String {
    describe_forward(&rule_of(
        tunnel.kind,
        tunnel.bind_port,
        tunnel.target_host.as_deref(),
        tunnel.target_port,
    ))
}

pub fn describe_forward_info(forward: &ForwardInfo) -> String {
    describe_forward(&rule_of(
        forward.kind,
        forward.bind_port,
        forward.target_host.as_deref(),
        forward.target_port,
    ))
}

/// A tunnel's name, else its target (`tunnel.name || tunnel.target`).
pub fn tunnel_title(tunnel: &TunnelRecord) -> String {
    match tunnel.name.as_deref() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => tunnel.target.clone(),
    }
}

/// The name in a row control's accessible label (`tunnel.name ?? tunnel.id`).
pub fn tunnel_label(tunnel: &TunnelRecord) -> String {
    tunnel.name.clone().unwrap_or_else(|| tunnel.id.clone())
}

/// `describeTunnelConnection`: where a tunnel connects and how it signs in.
pub fn describe_tunnel_connection(tunnel: &TunnelRecord) -> String {
    let Some(options) = &tunnel.ssh_options else {
        return format!("SSH config · {}", tunnel.target);
    };
    let endpoint = format!(
        "{}{}{}",
        options
            .user
            .as_deref()
            .map(|user| format!("{user}@"))
            .unwrap_or_default(),
        tunnel.target,
        options
            .port
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    let jumps = options.proxy_jump.as_ref().map_or(0, Vec::len);
    let route = if jumps > 0 {
        format!("{jumps} jump{}", if jumps == 1 { "" } else { "s" })
    } else {
        "direct".to_owned()
    };
    let keys = options.identity_files.as_ref().map_or(0, Vec::len);
    let auth = if options.password_only == Some(true) {
        "password".to_owned()
    } else if keys > 0 {
        format!("{keys} key{}", if keys == 1 { "" } else { "s" })
    } else {
        "agent / defaults".to_owned()
    };
    format!("{endpoint} · {route} · {auth}")
}

/// The running forward of each saved tunnel. Like `new Map(entries)`, a later
/// forward for the same tunnel wins.
pub fn running_by_tunnel(forwards: &[ForwardInfo]) -> HashMap<String, ForwardInfo> {
    let mut map = HashMap::new();
    for forward in forwards {
        if let Some(tunnel_id) = &forward.tunnel_id {
            map.insert(tunnel_id.clone(), forward.clone());
        }
    }
    map
}

/// Forwards not realizing a saved tunnel (config, ad-hoc or orphaned), by
/// connection, in list order.
pub fn session_forwards_by_conn(
    forwards: &[ForwardInfo],
    tunnels: &[TunnelRecord],
) -> HashMap<String, Vec<ForwardInfo>> {
    let mut groups: HashMap<String, Vec<ForwardInfo>> = HashMap::new();
    for forward in forwards {
        if forward
            .tunnel_id
            .as_ref()
            .is_some_and(|id| tunnels.iter().any(|tunnel| &tunnel.id == id))
        {
            continue;
        }
        groups
            .entry(forward.conn_id.clone())
            .or_default()
            .push(forward.clone());
    }
    groups
}

/// The status dot of a tunnel row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelDot {
    /// Not running: `text.disabled`.
    Idle,
    Active,
    /// Starting or reconnecting.
    Pending,
    Error,
}

pub fn tunnel_dot(running: Option<&ForwardInfo>) -> TunnelDot {
    match running.map(|forward| forward.status) {
        None => TunnelDot::Idle,
        Some(ForwardStatus::Error) => TunnelDot::Error,
        Some(ForwardStatus::Active) => TunnelDot::Active,
        Some(ForwardStatus::Starting | ForwardStatus::Reconnecting) => TunnelDot::Pending,
    }
}

/// A status line under a tunnel's rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelStatusLine {
    /// "Starting…" in secondary ink.
    Starting,
    /// "Reconnecting — <error>" in the warning text color, the error as tooltip.
    Reconnecting { text: String, tooltip: String },
    /// The error in `error.main`, also as tooltip.
    Failed(String),
}

pub fn tunnel_status_line(running: Option<&ForwardInfo>) -> Option<TunnelStatusLine> {
    let running = running?;
    match running.status {
        ForwardStatus::Starting => Some(TunnelStatusLine::Starting),
        ForwardStatus::Reconnecting => Some(TunnelStatusLine::Reconnecting {
            text: match &running.error {
                Some(error) => format!("Reconnecting — {error}"),
                None => "Reconnecting…".to_owned(),
            },
            tooltip: running.error.clone().unwrap_or_default(),
        }),
        ForwardStatus::Error => running.error.clone().map(TunnelStatusLine::Failed),
        ForwardStatus::Active => None,
    }
}

/// The button at the end of a tunnel row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TunnelAction {
    /// A start is in flight: a spinner.
    Busy,
    /// Stop the running forward.
    Stop { tooltip: &'static str },
    /// Start (or, after the backend gave up, start and sign in).
    Start { tooltip: String, disabled: bool },
}

/// The server stopped trying: start it again, with prompts, or dismiss it.
pub fn needs_start(running: Option<&ForwardInfo>) -> bool {
    running.is_some_and(|forward| forward.status == ForwardStatus::Error)
}

pub fn tunnel_action(
    tunnel: &TunnelRecord,
    running: Option<&ForwardInfo>,
    busy: bool,
    any_starting: bool,
) -> TunnelAction {
    if busy {
        return TunnelAction::Busy;
    }
    match running {
        Some(forward) if !needs_start(running) => TunnelAction::Stop {
            tooltip: match forward.status {
                ForwardStatus::Starting => "Cancel start",
                ForwardStatus::Reconnecting => "Stop reconnecting",
                _ => "Stop tunnel",
            },
        },
        _ => TunnelAction::Start {
            tooltip: if needs_start(running) {
                "Start tunnel and sign in".to_owned()
            } else {
                format!("Start tunnel (via {})", tunnel.target)
            },
            disabled: any_starting,
        },
    }
}

/// The second line of a tunnel row: the progress of a start, else the rule.
pub fn tunnel_rule_line(tunnel: &TunnelRecord, progress: Option<&str>) -> String {
    match progress {
        Some(progress) => progress.to_owned(),
        None => format!("{} {}", forward_flag(tunnel.kind), describe_tunnel(tunnel)),
    }
}

/// The confirmation before a tunnel is deleted: title and description.
pub fn delete_confirmation(tunnel: &TunnelRecord, running: bool) -> (String, String) {
    (
        format!("Delete tunnel “{}”?", tunnel_title(tunnel)),
        if running {
            format!(
                "{} is running — deleting the tunnel stops it and drops anything using the forwarded port.",
                describe_tunnel(tunnel)
            )
        } else {
            "The saved forwarding rule is removed. This cannot be undone.".to_owned()
        },
    )
}

/// The ssh_config block behind a connection, when it came from one.
pub fn host_entry_for<'a>(
    hosts: &'a [SshHostEntry],
    conn: &ConnectionInfo,
) -> Option<&'a SshHostEntry> {
    let alias = conn.metadata_alias.as_deref()?;
    hosts
        .iter()
        .find(|host| host.aliases.iter().any(|candidate| candidate == alias))
}

/// `user@host` plus `:port` when it is not 22.
pub fn connection_address(conn: &ConnectionInfo) -> String {
    let port = if conn.port == 22 {
        String::new()
    } else {
        format!(":{}", conn.port)
    };
    format!("{}@{}{port}", conn.user, conn.host)
}

/// The rule a running forward would be as a config line.
pub fn config_rule(forward: &ForwardInfo) -> ConfigForward {
    if forward.kind == ForwardType::Dynamic {
        rule_of(forward.kind, forward.bind_port, None, None)
    } else {
        rule_of(
            forward.kind,
            forward.bind_port,
            forward.target_host.as_deref(),
            forward.target_port,
        )
    }
}

/// "Save to <alias>'s ssh config": the block written again with the forward
/// appended to its rules.
pub fn bookmark_request(entry: &SshHostEntry, forward: &ForwardInfo) -> HostUpsertRequest {
    let mut options = entry.options.clone();
    let mut forwards = options.forwards.take().unwrap_or_default();
    forwards.push(config_rule(forward));
    options.forwards = Some(forwards);
    HostUpsertRequest {
        aliases: entry.aliases.clone(),
        description: entry.description.clone(),
        file: Some(entry.file.clone()),
        options,
        previous_alias: Some(entry.alias.clone()),
    }
}

/// `saveTunnelOptions`: the tunnel's definition with one switch flipped.
pub fn toggled_input(tunnel: &TunnelRecord, switch: TunnelSwitch) -> TunnelInput {
    let mut input = tunnel_input(tunnel);
    match switch {
        TunnelSwitch::AutoStart => input.auto_start = Some(!tunnel.auto_start),
        TunnelSwitch::AutoReconnect => input.auto_reconnect = Some(!tunnel.auto_reconnect),
    }
    input
}

/// A saved tunnel as the input that saves it unchanged.
pub fn tunnel_input(tunnel: &TunnelRecord) -> TunnelInput {
    TunnelInput {
        id: Some(tunnel.id.clone()),
        name: tunnel.name.clone(),
        target: tunnel.target.clone(),
        ssh_options: tunnel.ssh_options.clone(),
        kind: tunnel.kind,
        bind_port: tunnel.bind_port,
        target_host: tunnel.target_host.clone(),
        target_port: tunnel.target_port,
        auto_start: Some(tunnel.auto_start),
        auto_reconnect: Some(tunnel.auto_reconnect),
    }
}

/// "Save as tunnel": a new tunnel for a running forward on its connection's target.
pub fn save_as_tunnel_input(conn: &ConnectionInfo, forward: &ForwardInfo) -> TunnelInput {
    TunnelInput {
        id: None,
        name: None,
        target: conn.target.clone(),
        ssh_options: None,
        kind: forward.kind,
        bind_port: forward.bind_port,
        target_host: forward.target_host.clone(),
        target_port: forward.target_port,
        auto_start: None,
        auto_reconnect: None,
    }
}

/// The forward a tunnel start asks for on a connection.
pub fn tunnel_forward_request(conn_id: &str, tunnel: &TunnelRecord) -> ForwardRequest {
    ForwardRequest {
        conn_id: conn_id.to_owned(),
        kind: tunnel.kind,
        bind_port: tunnel.bind_port,
        target_host: tunnel.target_host.clone(),
        target_port: tunnel.target_port,
        tunnel_id: Some(tunnel.id.clone()),
    }
}

/// An ad-hoc forward on a live connection ("Forward on <target>").
pub fn adhoc_forward_request(conn_id: &str, rule: &ConfigForward) -> ForwardRequest {
    ForwardRequest {
        conn_id: conn_id.to_owned(),
        kind: rule.kind,
        bind_port: rule.bind_port,
        target_host: rule.target_host.clone(),
        target_port: rule.target_port,
        tunnel_id: None,
    }
}

/// A tunnel-owned profile may deliberately use different keys or jumps than a
/// live terminal with the same hostname, so only config-backed tunnels ride
/// an existing connection.
pub fn reusable_connection<'a>(
    tunnel: &TunnelRecord,
    connections: &'a [ConnectionInfo],
) -> Option<&'a ConnectionInfo> {
    if tunnel.ssh_options.is_some() {
        return None;
    }
    connections.iter().find(|conn| conn.target == tunnel.target)
}

/// The tooltip of a session forward's stop button.
pub fn stop_forward_tooltip(forward: &ForwardInfo) -> &'static str {
    if forward.origin == ForwardOrigin::Config {
        "Stop for this session (the config rule stays)"
    } else {
        "Stop forward"
    }
}

/// A session forward's rule, with its error after a dash.
pub fn session_forward_text(forward: &ForwardInfo) -> String {
    match &forward.error {
        Some(error) => format!("{} — {error}", describe_forward_info(forward)),
        None => describe_forward_info(forward),
    }
}

/// The success toast of a tunnel start.
pub fn tunnel_up_message(tunnel: &TunnelRecord) -> String {
    format!("Tunnel up — {}", describe_tunnel(tunnel))
}

pub const SAVED_AS_TUNNEL: &str = "Saved — the tunnel can now be started without a terminal.";
pub const SAVED_TO_CONFIG: &str = "Saved to ssh config — starts with every connection.";
pub const NO_TUNNELS: &str = "Saved tunnels start and stop port forwarding without opening a terminal — closing terminals never takes a running tunnel down.";
pub const NO_CONNECTIONS: &str = "No live SSH connections. Forwards on open sessions appear here.";
pub const ADHOC_CAPTION: &str =
    "Belongs to this terminal session; save it as a tunnel to keep it after the terminal closes.";

#[cfg(test)]
mod tests {
    use muxus_core::api::{ForwardLifecycle, TunnelSshOptions};

    use super::*;

    fn tunnel(id: &str, ssh_options: Option<TunnelSshOptions>) -> TunnelRecord {
        TunnelRecord {
            id: id.into(),
            name: Some("Grafana".into()),
            target: "web-01".into(),
            ssh_options,
            kind: ForwardType::Local,
            bind_port: 3000,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(3000),
            auto_start: false,
            auto_reconnect: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn forward(
        id: &str,
        conn: &str,
        tunnel_id: Option<&str>,
        status: ForwardStatus,
    ) -> ForwardInfo {
        ForwardInfo {
            id: id.into(),
            conn_id: conn.into(),
            kind: ForwardType::Local,
            bind_port: 8080,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(8080),
            origin: ForwardOrigin::Manual,
            lifecycle: ForwardLifecycle::Session,
            status,
            error: None,
            tunnel_id: tunnel_id.map(Into::into),
        }
    }

    fn conn(target: &str, port: u16) -> ConnectionInfo {
        ConnectionInfo {
            id: format!("conn-{target}"),
            target: target.into(),
            host: format!("{target}.prod.internal"),
            port,
            user: "deploy".into(),
            metadata_alias: Some(target.into()),
            sftp_available: true,
        }
    }

    #[test]
    fn describes_where_a_tunnel_connects() {
        assert_eq!(
            describe_tunnel_connection(&tunnel("t", None)),
            "SSH config · web-01"
        );
        let custom = TunnelSshOptions {
            user: Some("ops".into()),
            port: Some(2222),
            proxy_jump: Some(vec!["bastion".into(), "edge".into()]),
            identity_files: Some(vec!["~/.ssh/a".into()]),
            ..TunnelSshOptions::default()
        };
        assert_eq!(
            describe_tunnel_connection(&tunnel("t", Some(custom))),
            "ops@web-01:2222 · 2 jumps · 1 key"
        );
        let password = TunnelSshOptions {
            password_only: Some(true),
            proxy_jump: Some(vec!["bastion".into()]),
            identity_files: Some(vec!["a".into(), "b".into()]),
            ..TunnelSshOptions::default()
        };
        assert_eq!(
            describe_tunnel_connection(&tunnel("t", Some(password))),
            "web-01 · 1 jump · password"
        );
        assert_eq!(
            describe_tunnel_connection(&tunnel("t", Some(TunnelSshOptions::default()))),
            "web-01 · direct · agent / defaults"
        );
    }

    #[test]
    fn groups_forwards_under_their_tunnel_or_connection() {
        let tunnels = [tunnel("t1", None)];
        let forwards = [
            forward("f1", "c1", Some("t1"), ForwardStatus::Active),
            forward("f2", "c1", None, ForwardStatus::Active),
            // A tunnel that no longer exists: shown with its connection.
            forward("f3", "c2", Some("gone"), ForwardStatus::Active),
            forward("f4", "c1", None, ForwardStatus::Error),
        ];
        let running = running_by_tunnel(&forwards);
        assert_eq!(running["t1"].id, "f1");
        assert_eq!(running["gone"].id, "f3");
        let groups = session_forwards_by_conn(&forwards, &tunnels);
        let ids = |conn: &str| {
            groups[conn]
                .iter()
                .map(|f| f.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("c1"), ["f2", "f4"]);
        assert_eq!(ids("c2"), ["f3"]);
    }

    #[test]
    fn shows_each_tunnel_state() {
        let t = tunnel("t1", None);
        assert_eq!(tunnel_dot(None), TunnelDot::Idle);
        assert_eq!(
            tunnel_action(&t, None, false, false),
            TunnelAction::Start {
                tooltip: "Start tunnel (via web-01)".into(),
                disabled: false
            }
        );
        assert_eq!(
            tunnel_action(&t, None, false, true),
            TunnelAction::Start {
                tooltip: "Start tunnel (via web-01)".into(),
                disabled: true
            }
        );
        assert_eq!(tunnel_action(&t, None, true, true), TunnelAction::Busy);

        let starting = forward("f", "", Some("t1"), ForwardStatus::Starting);
        assert_eq!(tunnel_dot(Some(&starting)), TunnelDot::Pending);
        assert_eq!(
            tunnel_status_line(Some(&starting)),
            Some(TunnelStatusLine::Starting)
        );
        assert_eq!(
            tunnel_action(&t, Some(&starting), false, false),
            TunnelAction::Stop {
                tooltip: "Cancel start"
            }
        );

        let mut reconnecting = forward("f", "c", Some("t1"), ForwardStatus::Reconnecting);
        assert_eq!(
            tunnel_status_line(Some(&reconnecting)),
            Some(TunnelStatusLine::Reconnecting {
                text: "Reconnecting…".into(),
                tooltip: String::new()
            })
        );
        reconnecting.error = Some("connection refused".into());
        assert_eq!(
            tunnel_status_line(Some(&reconnecting)),
            Some(TunnelStatusLine::Reconnecting {
                text: "Reconnecting — connection refused".into(),
                tooltip: "connection refused".into()
            })
        );
        assert_eq!(
            tunnel_action(&t, Some(&reconnecting), false, false),
            TunnelAction::Stop {
                tooltip: "Stop reconnecting"
            }
        );

        // The backend gave up: start again with prompts, or dismiss.
        let mut failed = forward("f", "c", Some("t1"), ForwardStatus::Error);
        failed.error = Some("password required".into());
        assert!(needs_start(Some(&failed)));
        assert_eq!(tunnel_dot(Some(&failed)), TunnelDot::Error);
        assert_eq!(
            tunnel_status_line(Some(&failed)),
            Some(TunnelStatusLine::Failed("password required".into()))
        );
        assert_eq!(
            tunnel_action(&t, Some(&failed), false, false),
            TunnelAction::Start {
                tooltip: "Start tunnel and sign in".into(),
                disabled: false
            }
        );
        let active = forward("f", "c", Some("t1"), ForwardStatus::Active);
        assert_eq!(tunnel_dot(Some(&active)), TunnelDot::Active);
        assert_eq!(tunnel_status_line(Some(&active)), None);
        assert_eq!(
            tunnel_action(&t, Some(&active), false, false),
            TunnelAction::Stop {
                tooltip: "Stop tunnel"
            }
        );
        assert_eq!(
            tunnel_rule_line(&t, None),
            "-L 127.0.0.1:3000 → 127.0.0.1:3000"
        );
        assert_eq!(
            tunnel_rule_line(&t, Some("Authenticating…")),
            "Authenticating…"
        );
    }

    #[test]
    fn asks_before_deleting_and_says_what_stops() {
        let t = tunnel("t1", None);
        assert_eq!(
            delete_confirmation(&t, false),
            (
                "Delete tunnel “Grafana”?".into(),
                "The saved forwarding rule is removed. This cannot be undone.".into()
            )
        );
        assert_eq!(
            delete_confirmation(&t, true).1,
            "127.0.0.1:3000 → 127.0.0.1:3000 is running — deleting the tunnel stops it and drops anything using the forwarded port."
        );
        let unnamed = TunnelRecord {
            name: Some(String::new()),
            ..t
        };
        assert_eq!(tunnel_title(&unnamed), "web-01");
    }

    #[test]
    fn builds_the_requests_behind_the_buttons() {
        let t = tunnel("t1", None);
        let toggled = toggled_input(&t, TunnelSwitch::AutoStart);
        assert_eq!(toggled.auto_start, Some(true));
        assert_eq!(toggled.auto_reconnect, Some(true));
        assert_eq!(toggled.id.as_deref(), Some("t1"));
        assert_eq!(
            toggled_input(&t, TunnelSwitch::AutoReconnect).auto_reconnect,
            Some(false)
        );

        let c = conn("web-01", 22);
        assert_eq!(connection_address(&c), "deploy@web-01.prod.internal");
        assert_eq!(
            connection_address(&conn("db", 2222)),
            "deploy@db.prod.internal:2222"
        );
        let f = forward("f2", "conn-web-01", None, ForwardStatus::Active);
        let saved = save_as_tunnel_input(&c, &f);
        assert_eq!(saved.target, "web-01");
        assert_eq!(saved.bind_port, 8080);
        assert_eq!(saved.id, None);

        let request = tunnel_forward_request("conn-1", &t);
        assert_eq!(request.tunnel_id.as_deref(), Some("t1"));
        assert_eq!(request.bind_port, 3000);

        // Only config-backed tunnels reuse a live connection.
        let connections = [c.clone()];
        assert!(reusable_connection(&t, &connections).is_some());
        let custom = tunnel("t2", Some(TunnelSshOptions::default()));
        assert!(reusable_connection(&custom, &connections).is_none());
    }

    #[test]
    fn bookmarks_a_forward_into_the_host_block() {
        let entry: SshHostEntry = serde_json::from_value(serde_json::json!({
            "alias": "web-01",
            "aliases": ["web-01", "web"],
            "file": "/home/u/.ssh/config",
            "options": { "hostname": "web-01.prod.internal", "forwards": [
                { "type": "dynamic", "bindPort": 1080 }
            ] },
            "resolved": {
                "hostname": "web-01.prod.internal", "port": 22, "identityFiles": [],
                "certificateFiles": [], "identitiesOnly": false, "forwardAgent": false,
                "proxyJump": [], "forwards": [], "passwordOnly": false
            }
        }))
        .unwrap();
        let mut dynamic = forward("f", "c", None, ForwardStatus::Active);
        dynamic.kind = ForwardType::Dynamic;
        let request = bookmark_request(&entry, &dynamic);
        assert_eq!(request.previous_alias.as_deref(), Some("web-01"));
        assert_eq!(request.file.as_deref(), Some("/home/u/.ssh/config"));
        let rules = request.options.forwards.unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].target_host, None);
        let c = conn("web", 22);
        assert_eq!(
            host_entry_for(std::slice::from_ref(&entry), &c)
                .unwrap()
                .alias,
            "web-01"
        );
        assert_eq!(
            stop_forward_tooltip(&ForwardInfo {
                origin: ForwardOrigin::Config,
                ..dynamic.clone()
            }),
            "Stop for this session (the config rule stays)"
        );
        let mut failed = forward("f", "c", None, ForwardStatus::Error);
        failed.error = Some("address in use".into());
        assert_eq!(
            session_forward_text(&failed),
            "127.0.0.1:8080 → 127.0.0.1:8080 — address in use"
        );
    }
}

/// What the top bar's forwarding button shows (`TopBar.tsx`): a badge with every forward
/// on every connection, green, or amber while any is interrupted, and a tooltip that counts
/// them by state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForwardsSummary {
    pub total: usize,
    pub active: usize,
    pub starting: usize,
    /// Tunnels whose connection dropped, or that could not start on their own.
    pub interrupted: usize,
}

impl ForwardsSummary {
    pub fn of(forwards: &[ForwardInfo]) -> Self {
        let count = |status: ForwardStatus| forwards.iter().filter(|f| f.status == status).count();
        Self {
            total: forwards.len(),
            active: count(ForwardStatus::Active),
            starting: count(ForwardStatus::Starting),
            interrupted: count(ForwardStatus::Reconnecting) + count(ForwardStatus::Error),
        }
    }

    /// `Port forwarding & tunnels — 2 active, 1 interrupted`.
    pub fn tooltip(&self) -> String {
        let parts: Vec<String> = [
            (self.active, "active"),
            (self.starting, "starting"),
            (self.interrupted, "interrupted"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect();
        if parts.is_empty() {
            "Port forwarding & tunnels".into()
        } else {
            format!("Port forwarding & tunnels — {}", parts.join(", "))
        }
    }

    /// The badge's color: `warning` while any forward is interrupted.
    pub fn warns(&self) -> bool {
        self.interrupted > 0
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn forward(id: &str, status: ForwardStatus) -> ForwardInfo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "connId": "c1",
            "origin": "manual",
            "lifecycle": "session",
            "type": "local",
            "bindPort": 8080,
            "targetHost": "localhost",
            "targetPort": 80,
            "status": serde_json::to_value(status).unwrap(),
        }))
        .unwrap()
    }

    #[test]
    fn counts_forwards_by_state_for_the_badge_and_tooltip() {
        let none = ForwardsSummary::of(&[]);
        assert_eq!(none.total, 0);
        assert_eq!(none.tooltip(), "Port forwarding & tunnels");
        assert!(!none.warns());

        let summary = ForwardsSummary::of(&[
            forward("a", ForwardStatus::Active),
            forward("b", ForwardStatus::Active),
            forward("c", ForwardStatus::Starting),
            forward("d", ForwardStatus::Reconnecting),
            forward("e", ForwardStatus::Error),
        ]);
        assert_eq!(summary.total, 5);
        assert_eq!(
            summary.tooltip(),
            "Port forwarding & tunnels — 2 active, 1 starting, 2 interrupted"
        );
        assert!(summary.warns());
        assert_eq!(
            ForwardsSummary::of(&[forward("a", ForwardStatus::Active)]).tooltip(),
            "Port forwarding & tunnels — 1 active"
        );
    }
}
