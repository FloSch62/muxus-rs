//! Connection diagnostics behind a failed session's "diagnose" key
//! (`server/src/diagnostics/connection-diagnostics.ts`).
//!
//! Cheap checks that need no privileges and no extra tools beyond the system
//! `ping`, so they behave the same on Windows, macOS and Linux. Each layer
//! (name resolution, ICMP, TCP, the service's greeting) explains a different
//! class of failure, and the lowest one that fails names the likely cause.

mod probes;

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use muxus_core::api::{ConnectionCheck, ConnectionCheckStatus, ConnectionDiagnosticsResponse};
use regex::Regex;

pub use probes::{SystemProbes, probe_tcp};

pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const GREETING_TIMEOUT: Duration = Duration::from_secs(3);
pub const PING_TIMEOUT: Duration = Duration::from_secs(3);
/// Addresses past this are listed by the DNS row but not dialed.
const MAX_DIALED_ADDRESSES: usize = 3;
const MAX_LISTED_ADDRESSES: usize = 3;
/// RFC 4253 lets a server send other lines before its identification string.
pub const GREETING_MAX_BYTES: usize = 2048;
const MAX_QUOTED_CHARS: usize = 80;

/// The operating system the ping command is built for.
pub use muxus_core::platform::Platform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingResult {
    /// `rtt` keeps the command's own precision, such as "0.42 ms" or "<1 ms".
    Reply {
        rtt: Option<String>,
    },
    NoReply,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Greeting {
    Ssh { banner: String },
    Other { text: String },
    Closed,
    Silent,
}

/// Why reading a greeting is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreetingEnd {
    Closed,
    Silent,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TcpOutcome {
    Connected,
    Refused,
    Timeout,
    Unreachable,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpProbe {
    pub address: IpAddr,
    pub outcome: TcpOutcome,
    pub ms: Option<u64>,
    /// Error code or message for `Unreachable` and `Error`.
    pub error: Option<String>,
    pub greeting: Option<Greeting>,
}

impl TcpProbe {
    pub fn new(address: IpAddr, outcome: TcpOutcome) -> Self {
        Self {
            address,
            outcome,
            ms: None,
            error: None,
            greeting: None,
        }
    }
}

/// A failed name lookup: `code` is the getaddrinfo error name Node reports
/// (`ENOTFOUND`, `ENODATA`, `EAI_AGAIN`), when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupError {
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentProbe {
    pub available: bool,
    pub keys: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    Ok,
    Missing,
    Unreadable,
}

/// The I/O behind the checks; tests replace the parts that leave the machine.
#[async_trait]
pub trait DiagnosticProbes: Send + Sync {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, LookupError>;
    async fn ping(&self, address: IpAddr) -> PingResult;
    async fn connect(&self, address: IpAddr, port: u16, read_greeting: bool) -> TcpProbe;
    async fn agent(&self, socket: &str) -> AgentProbe;
    async fn readable(&self, file: &str) -> FileAccess;
    /// An environment variable (`SSH_AUTH_SOCK` and the ones IdentityAgent names).
    fn env(&self, name: &str) -> Option<String>;
    /// The user's home directory, shortened to `~` in what the checks print.
    fn home(&self) -> Option<String>;
}

// --- ping -------------------------------------------------------------------

/// The system ping for one echo request, bounded to about two seconds.
pub fn ping_invocation(address: IpAddr, platform: Platform) -> (String, Vec<String>) {
    let text = address.to_string();
    let (file, args): (&str, Vec<&str>) = match platform {
        Platform::Windows => ("ping", vec!["-n", "1", "-w", "2000"]),
        // macOS keeps IPv6 in a separate ping6, whose wait option differs.
        Platform::MacOs | Platform::FreeBsd if address.is_ipv6() => ("ping6", vec!["-c", "1"]),
        Platform::MacOs | Platform::FreeBsd => ("ping", vec!["-c", "1", "-t", "2"]),
        // iputils, BusyBox and inetutils all take -W in seconds.
        Platform::Linux | Platform::Other => ("ping", vec!["-c", "1", "-W", "2"]),
    };
    let mut args: Vec<String> = args.into_iter().map(str::to_string).collect();
    args.push(text);
    (file.to_string(), args)
}

static PING_TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([=<])\s*([0-9]+(?:[.,][0-9]+)?)\s*ms\b").expect("valid regex")
});
static PING_TTL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bTTL[=:]").expect("valid regex"));
static PING_REFUSAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)not permitted|permission denied|socket:|usage:|invalid option|unknown option")
        .expect("valid regex")
});

/// Read a ping run by its exit code, which every platform sets the same way
/// for "got a reply", and take the round trip from the first "=12 ms" or
/// "<1ms" in the output, which survives translated output.
pub fn read_ping_result(
    exit_code: Option<i32>,
    output: &str,
    address: IpAddr,
    platform: Platform,
) -> PingResult {
    // Windows exits 0 when a router answers "destination host unreachable";
    // only a real IPv4 echo reply carries a TTL.
    let replied = exit_code == Some(0)
        && (platform != Platform::Windows || address.is_ipv6() || PING_TTL.is_match(output));
    if replied {
        let rtt = PING_TIME
            .captures(output)
            .map(|time| format!("{}{} ms", if &time[1] == "<" { "<" } else { "" }, &time[2]));
        return PingResult::Reply { rtt };
    }
    if PING_REFUSAL.is_match(output) {
        return PingResult::Unavailable {
            reason: first_line(output),
        };
    }
    PingResult::NoReply
}

// --- greeting ---------------------------------------------------------------

/// The SSH identification line among what the server sent, or (once reading
/// is over: the server closed, went quiet, or sent too much) what it sent
/// instead. `None` while still undecided.
pub fn read_greeting(received: &[u8], end: Option<GreetingEnd>) -> Option<Greeting> {
    let text: String = received.iter().map(|&byte| char::from(byte)).collect();
    let lines: Vec<&str> = text.split('\n').collect();
    for line in &lines[..lines.len() - 1] {
        if line.starts_with("SSH-") {
            return Some(Greeting::Ssh {
                banner: printable(line),
            });
        }
    }
    let end = end?;
    let partial = lines.last().copied().unwrap_or("");
    if partial.starts_with("SSH-") {
        return Some(Greeting::Ssh {
            banner: printable(partial),
        });
    }
    if received.is_empty() {
        return Some(if end == GreetingEnd::Closed {
            Greeting::Closed
        } else {
            Greeting::Silent
        });
    }
    let first = js_trim_start(&text).split('\n').next().unwrap_or("");
    let quoted = printable(first);
    Some(Greeting::Other {
        text: if quoted.is_empty() {
            "binary data".into()
        } else {
            quoted
        },
    })
}

fn js_trim_start(text: &str) -> &str {
    muxus_core::js::js_trim_start(text)
}

/// Printable ASCII only, runs of anything else as one space, at most 80 characters.
fn printable(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut in_other = false;
    for ch in text.chars() {
        if (' '..='~').contains(&ch) {
            clean.push(ch);
            in_other = false;
        } else if !in_other {
            clean.push(' ');
            in_other = true;
        }
    }
    let clean = clean.trim();
    if clean.chars().count() > MAX_QUOTED_CHARS {
        let cut: String = clean.chars().take(MAX_QUOTED_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        clean.to_string()
    }
}

fn first_line(text: &str) -> String {
    printable(
        muxus_core::js::js_trim(text)
            .split('\n')
            .next()
            .unwrap_or(""),
    )
}

// --- one endpoint -----------------------------------------------------------

/// `host:port`, bracketing an IPv6 address.
pub fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Ssh,
    Telnet,
}

/// The checks of one endpoint; `conclusion` is set when a layer failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDiagnosis {
    pub checks: Vec<ConnectionCheck>,
    pub conclusion: Option<String>,
}

fn check(label: &str, status: ConnectionCheckStatus, detail: impl Into<String>) -> ConnectionCheck {
    ConnectionCheck {
        label: label.into(),
        status,
        detail: detail.into(),
    }
}

/// Resolve, ping, dial and (for SSH) read the greeting of one host:port.
pub async fn diagnose_endpoint(
    host: &str,
    port: u16,
    service: Service,
    probes: &dyn DiagnosticProbes,
) -> EndpointDiagnosis {
    let mut checks = Vec::new();
    let addresses: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(literal) => vec![literal],
        Err(_) => {
            let started = Instant::now();
            let (addresses, failure) =
                match tokio::time::timeout(LOOKUP_TIMEOUT, probes.lookup(host)).await {
                    Err(_) => (
                        Vec::new(),
                        Some(format!(
                            "no answer for {host} within {} s",
                            LOOKUP_TIMEOUT.as_secs()
                        )),
                    ),
                    Ok(Ok(found)) => (found, None),
                    Ok(Err(err)) => {
                        let failure = match err.code.as_deref() {
                            Some("ENOTFOUND" | "ENODATA") => format!("{host} does not resolve"),
                            Some("EAI_AGAIN") => {
                                format!("the DNS server did not answer for {host}")
                            }
                            Some(code) => format!("{host}: {code}"),
                            None => format!("{host}: {}", err.message),
                        };
                        (Vec::new(), Some(failure))
                    }
                };
            if failure.is_some() || addresses.is_empty() {
                let detail = failure.unwrap_or_else(|| format!("{host} has no addresses"));
                checks.push(check("DNS", ConnectionCheckStatus::Fail, detail));
                return EndpointDiagnosis {
                    checks,
                    conclusion: Some(format!(
                        "{host} does not resolve from this computer. Check the host name, the DNS settings, or connect the VPN whose DNS knows it."
                    )),
                };
            }
            let listed: Vec<String> = addresses
                .iter()
                .take(MAX_LISTED_ADDRESSES)
                .map(IpAddr::to_string)
                .collect();
            let more = if addresses.len() > MAX_LISTED_ADDRESSES {
                format!(" +{} more", addresses.len() - MAX_LISTED_ADDRESSES)
            } else {
                String::new()
            };
            checks.push(check(
                "DNS",
                ConnectionCheckStatus::Ok,
                format!(
                    "{host} → {}{more} ({} ms)",
                    listed.join(", "),
                    started.elapsed().as_millis()
                ),
            ));
            addresses
        }
    };

    let dialed: Vec<IpAddr> = addresses
        .iter()
        .take(MAX_DIALED_ADDRESSES)
        .copied()
        .collect();
    let read_greeting = service == Service::Ssh;
    let dial = |index: usize| {
        let address = dialed.get(index).copied();
        async move {
            match address {
                Some(address) => Some(probes.connect(address, port, read_greeting).await),
                None => None,
            }
        }
    };
    let (ping, first, second, third) =
        tokio::join!(probes.ping(dialed[0]), dial(0), dial(1), dial(2));
    let tcp: Vec<TcpProbe> = [first, second, third].into_iter().flatten().collect();

    let pinged = dialed[0];
    checks.push(match &ping {
        PingResult::Reply { rtt } => check(
            "Ping",
            ConnectionCheckStatus::Ok,
            format!(
                "{pinged} replied{}",
                rtt.as_ref()
                    .map(|rtt| format!(" in {rtt}"))
                    .unwrap_or_default()
            ),
        ),
        PingResult::NoReply => check(
            "Ping",
            ConnectionCheckStatus::Warn,
            format!("{pinged} did not reply (many hosts and firewalls drop ping)"),
        ),
        PingResult::Unavailable { reason } => check(
            "Ping",
            ConnectionCheckStatus::Skipped,
            format!("could not run ping: {reason}"),
        ),
    });

    let open = tcp
        .iter()
        .find(|probe| probe.outcome == TcpOutcome::Connected);
    for probe in &tcp {
        let address = probe.address.to_string();
        let at = host_port(&address, port);
        if probe.outcome == TcpOutcome::Connected {
            checks.push(check(
                "TCP",
                ConnectionCheckStatus::Ok,
                format!("{at} connected in {} ms", probe.ms.unwrap_or(0)),
            ));
            continue;
        }
        // Clients fall back to the next address, so one dead address is only
        // a warning, and a family this computer has no network for is not news.
        let no_network = probe.error.as_deref() == Some("ENETUNREACH");
        let status = if open.is_none() {
            ConnectionCheckStatus::Fail
        } else if no_network {
            ConnectionCheckStatus::Info
        } else {
            ConnectionCheckStatus::Warn
        };
        let error = probe.error.clone().unwrap_or_default();
        let detail = match probe.outcome {
            TcpOutcome::Refused => format!("{at} refused the connection"),
            TcpOutcome::Timeout => {
                format!("{at} did not answer within {} s", CONNECT_TIMEOUT.as_secs())
            }
            _ if no_network => {
                let family = if probe.address.is_ipv6() { 6 } else { 4 };
                format!("{at}: this computer has no route to IPv{family} networks")
            }
            TcpOutcome::Unreachable => format!("{at}: no route to the host ({error})"),
            _ => format!("{at}: {error}"),
        };
        checks.push(check("TCP", status, detail));
    }

    let Some(open) = open else {
        // An address without a route says nothing about the others.
        let routed: Vec<&TcpProbe> = tcp
            .iter()
            .filter(|probe| probe.outcome != TcpOutcome::Unreachable)
            .collect();
        let outcomes: HashSet<TcpOutcome> = routed.iter().map(|probe| probe.outcome).collect();
        let pinged_ok = matches!(ping, PingResult::Reply { .. });
        let service_name = if service == Service::Ssh {
            "SSH"
        } else {
            "Telnet"
        };
        let conclusion = if routed.is_empty() {
            format!("There is no network route to {host}. Check the network connection and VPN.")
        } else if outcomes.len() == 1 && outcomes.contains(&TcpOutcome::Refused) {
            format!(
                "{host} is up but nothing accepts connections on port {port}. Is the {service_name} server running on that port?"
            )
        } else if outcomes.contains(&TcpOutcome::Timeout) && pinged_ok {
            format!(
                "{host} answers ping, but port {port} does not answer. A firewall is probably dropping the connection."
            )
        } else if outcomes.contains(&TcpOutcome::Timeout) {
            format!(
                "{host} does not answer from this computer. It may be down, or reachable only through a VPN or a jump host."
            )
        } else {
            format!(
                "Could not open a connection to {} ({}).",
                host_port(host, port),
                routed
                    .first()
                    .and_then(|probe| probe.error.clone())
                    .unwrap_or_else(|| "unknown error".into())
            )
        };
        return EndpointDiagnosis {
            checks,
            conclusion: Some(conclusion),
        };
    };

    if service != Service::Ssh {
        return EndpointDiagnosis {
            checks,
            conclusion: None,
        };
    }
    let at = host_port(&open.address.to_string(), port);
    let conclusion = match open.greeting.clone().unwrap_or(Greeting::Silent) {
        Greeting::Ssh { banner } => {
            checks.push(check("SSH", ConnectionCheckStatus::Ok, banner));
            None
        }
        Greeting::Other { text } => {
            checks.push(check(
                "SSH",
                ConnectionCheckStatus::Fail,
                format!("{at} answered \"{text}\""),
            ));
            Some(format!(
                "Port {port} answers, but not with SSH. Check the port number."
            ))
        }
        Greeting::Closed => {
            checks.push(check(
                "SSH",
                ConnectionCheckStatus::Fail,
                format!("{at} closed the connection without a greeting"),
            ));
            Some("The server accepted the connection and closed it before greeting. SSH servers do this when MaxStartups is reached, or when TCP wrappers, fail2ban or an allow list reject this address.".into())
        }
        Greeting::Silent => {
            checks.push(check(
                "SSH",
                ConnectionCheckStatus::Fail,
                format!(
                    "{at} sent no greeting within {} s",
                    GREETING_TIMEOUT.as_secs()
                ),
            ));
            Some(format!(
                "Port {port} accepts the connection but sends no SSH greeting. Another service, or a firewall or load balancer that holds connections open, may be in the way."
            ))
        }
    };
    EndpointDiagnosis { checks, conclusion }
}

// --- sessions ---------------------------------------------------------------

/// One hop of an SSH dial plan, as the SSH layer resolved it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticHop {
    /// The host as written: an alias or the typed name.
    pub host: String,
    /// The resolved HostName.
    pub hostname: String,
    pub port: u16,
    pub user: String,
    /// The ProxyCommand with its `%` tokens expanded, when one is set.
    pub proxy_command: Option<String>,
    /// Key authentication is off for this hop.
    pub password_only: bool,
    pub identity_files: Vec<String>,
    /// IdentityAgent: a path, `$VAR`, `SSH_AUTH_SOCK` or `none`.
    pub identity_agent: Option<String>,
}

fn tilde(file: &str, home: Option<&str>) -> String {
    match home {
        Some(home)
            if file == home
                || file.starts_with(&format!("{home}/"))
                || file.starts_with(&format!("{home}\\")) =>
        {
            format!("~{}", &file[home.len()..])
        }
        _ => file.to_string(),
    }
}

/// The agent socket for one host: the IdentityAgent override when set
/// (`none`, `$VAR`/`${VAR}`, the literal `SSH_AUTH_SOCK`, or a path), otherwise
/// the environment default.
pub fn resolve_agent_socket(
    identity_agent: Option<&str>,
    probes: &dyn DiagnosticProbes,
) -> Option<String> {
    let nonempty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let Some(agent) = identity_agent else {
        return nonempty(probes.env("SSH_AUTH_SOCK"))
            .or_else(|| cfg!(windows).then(|| r"\\.\pipe\openssh-ssh-agent".to_string()));
    };
    if agent.eq_ignore_ascii_case("none") {
        return None;
    }
    if agent == "SSH_AUTH_SOCK" {
        return nonempty(probes.env("SSH_AUTH_SOCK"));
    }
    if let Some(name) = agent.strip_prefix('$') {
        let name = name
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
            .unwrap_or(name);
        return nonempty(probes.env(name));
    }
    match (agent.strip_prefix('~'), probes.home()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            Some(format!("{home}{rest}"))
        }
        _ => Some(agent.to_string()),
    }
}

fn describe_route(chain: &[DiagnosticHop]) -> String {
    let target = chain.last().expect("a dial plan has a target");
    let endpoint = format!(
        "{}@{}",
        target.user,
        host_port(&target.hostname, target.port)
    );
    let mut parts = vec![if target.host == target.hostname {
        endpoint
    } else {
        format!("{} → {endpoint}", target.host)
    }];
    let jumps: Vec<&str> = chain[..chain.len() - 1]
        .iter()
        .map(|hop| hop.host.as_str())
        .collect();
    if !jumps.is_empty() {
        parts.push(format!("via {}", jumps.join(" → ")));
    }
    if chain[0].proxy_command.is_some() {
        parts.push("through a ProxyCommand".into());
    }
    parts.join(" ")
}

/// Agent and key-file rows, and the configured key files that are missing.
async fn auth_checks(
    chain: &[DiagnosticHop],
    probes: &dyn DiagnosticProbes,
) -> (Vec<ConnectionCheck>, Vec<String>) {
    let mut checks = Vec::new();
    let key_hops: Vec<&DiagnosticHop> = chain.iter().filter(|hop| !hop.password_only).collect();
    if key_hops.is_empty() {
        return (checks, Vec::new());
    }
    let home = probes.home();
    let identity_agent = chain.last().and_then(|hop| hop.identity_agent.as_deref());
    if !identity_agent.is_some_and(|agent| agent.eq_ignore_ascii_case("none")) {
        match resolve_agent_socket(identity_agent, probes) {
            None => checks.push(check(
                "Agent",
                ConnectionCheckStatus::Info,
                "no SSH agent configured (SSH_AUTH_SOCK is not set)",
            )),
            Some(socket) => {
                let agent = probes.agent(&socket).await;
                // The Windows pipe is only a default; an agent nobody asked for is not a problem.
                let expected = identity_agent.is_some()
                    || probes
                        .env("SSH_AUTH_SOCK")
                        .is_some_and(|sock| !sock.is_empty());
                checks.push(if !agent.available {
                    check(
                        "Agent",
                        if expected {
                            ConnectionCheckStatus::Warn
                        } else {
                            ConnectionCheckStatus::Info
                        },
                        format!("no agent answered at {}", tilde(&socket, home.as_deref())),
                    )
                } else if agent.keys == 0 {
                    check(
                        "Agent",
                        ConnectionCheckStatus::Warn,
                        "the agent is running but holds no keys",
                    )
                } else {
                    check(
                        "Agent",
                        ConnectionCheckStatus::Ok,
                        format!(
                            "{} {} loaded",
                            agent.keys,
                            if agent.keys == 1 { "key" } else { "keys" }
                        ),
                    )
                });
            }
        }
    }

    let mut seen = HashSet::new();
    let files: Vec<&String> = key_hops
        .iter()
        .flat_map(|hop| hop.identity_files.iter())
        .filter(|file| seen.insert(file.as_str()))
        .collect();
    let mut problems = Vec::new();
    for file in &files {
        let state = probes.readable(file).await;
        if state != FileAccess::Ok {
            problems.push((*file, state));
        }
    }
    if !files.is_empty() {
        checks.push(if problems.is_empty() {
            check(
                "Key files",
                ConnectionCheckStatus::Ok,
                format!(
                    "{} configured {} found",
                    files.len(),
                    if files.len() == 1 { "file" } else { "files" }
                ),
            )
        } else {
            let detail: Vec<String> = problems
                .iter()
                .map(|(file, state)| {
                    let what = if *state == FileAccess::Missing {
                        "not found"
                    } else {
                        "not readable"
                    };
                    format!("{} {what}", tilde(file, home.as_deref()))
                })
                .collect();
            check("Key files", ConnectionCheckStatus::Warn, detail.join("; "))
        });
    }
    let missing = problems
        .iter()
        .map(|(file, _)| tilde(file, home.as_deref()))
        .collect();
    (checks, missing)
}

/// Diagnose an SSH session from its dial plan: the first hop is the only one in reach.
pub async fn diagnose_ssh(
    chain: &[DiagnosticHop],
    probes: &dyn DiagnosticProbes,
) -> ConnectionDiagnosticsResponse {
    let first = &chain[0];
    let target = chain.last().expect("a dial plan has a target");
    let checked = host_port(&first.hostname, first.port);
    let mut checks = vec![check(
        "Route",
        ConnectionCheckStatus::Info,
        describe_route(chain),
    )];

    let mut conclusion = None;
    if let Some(proxy_command) = &first.proxy_command {
        checks.push(check(
            "Network",
            ConnectionCheckStatus::Skipped,
            format!(
                "{} is reached through its ProxyCommand, not over a direct connection",
                first.host
            ),
        ));
        conclusion = Some(format!(
            "Muxus cannot test a ProxyCommand on its own. Run it in a terminal to see its errors: {proxy_command}"
        ));
    } else {
        let network = diagnose_endpoint(&first.hostname, first.port, Service::Ssh, probes).await;
        checks.extend(network.checks);
        if let Some(conclusion) = network.conclusion {
            return ConnectionDiagnosticsResponse {
                checked,
                checks,
                conclusion,
            };
        }
    }

    let (auth, missing_keys) = auth_checks(chain, probes).await;
    checks.extend(auth);
    if let Some(conclusion) = conclusion {
        return ConnectionDiagnosticsResponse {
            checked,
            checks,
            conclusion,
        };
    }
    let conclusion = if !missing_keys.is_empty() {
        format!(
            "The SSH server answers, but configured key files are missing ({}). Authentication fails without them unless another method is accepted.",
            missing_keys.join(", ")
        )
    } else if chain.len() > 1 {
        format!(
            "{} is reachable and speaks SSH. {} is dialed through it and cannot be checked from this computer; the failure reason above names the hop that failed.",
            first.host, target.host
        )
    } else {
        "The network path and the SSH server are fine, so the session failed after the greeting: at authentication, the host key, or a server-side limit. The failure reason above has the details.".into()
    };
    ConnectionDiagnosticsResponse {
        checked,
        checks,
        conclusion,
    }
}

pub async fn diagnose_telnet(
    host: &str,
    port: u16,
    probes: &dyn DiagnosticProbes,
) -> ConnectionDiagnosticsResponse {
    let checked = host_port(host, port);
    let network = diagnose_endpoint(host, port, Service::Telnet, probes).await;
    let conclusion = network.conclusion.unwrap_or_else(|| {
        format!(
            "{checked} accepts connections, so the network path is fine. The session failed after connecting; the failure reason above has the details."
        )
    });
    ConnectionDiagnosticsResponse {
        checked,
        checks: network.checks,
        conclusion,
    }
}
