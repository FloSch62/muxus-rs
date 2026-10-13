//! Port of `tests/unit/server/connection-diagnostics.test.ts`. The SSH dial
//! plans the TypeScript built from an ssh_config file are written out as hops.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{ConnectionCheck, ConnectionCheckStatus};
use muxus_session::diagnostics::{
    AgentProbe, DiagnosticHop, DiagnosticProbes, FileAccess, Greeting, GreetingEnd, LookupError,
    PingResult, Platform, Service, TcpOutcome, TcpProbe, diagnose_endpoint, diagnose_ssh,
    diagnose_telnet, ping_invocation, probe_tcp, read_greeting, read_ping_result,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

fn v4() -> IpAddr {
    "192.0.2.10".parse().unwrap()
}

fn v6() -> IpAddr {
    "2001:db8::10".parse().unwrap()
}

type Lookup = Box<dyn Fn(&str) -> Result<Vec<IpAddr>, LookupError> + Send + Sync>;
type Connect = Box<dyn Fn(IpAddr, u16, bool) -> TcpProbe + Send + Sync>;

/// Probes answering from a table.
struct Fake {
    lookup: Lookup,
    ping: PingResult,
    connect: Connect,
    agent: AgentProbe,
    readable: Box<dyn Fn(&str) -> FileAccess + Send + Sync>,
    env: Vec<(String, String)>,
    lookups: Mutex<Vec<String>>,
    connects: Mutex<Vec<(IpAddr, u16, bool)>>,
}

fn connected_ssh(address: IpAddr) -> TcpProbe {
    TcpProbe {
        ms: Some(4),
        greeting: Some(Greeting::Ssh {
            banner: "SSH-2.0-OpenSSH_9.6".into(),
        }),
        ..TcpProbe::new(address, TcpOutcome::Connected)
    }
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            lookup: Box::new(|_| Ok(vec![v4()])),
            ping: PingResult::Reply {
                rtt: Some("1.2 ms".into()),
            },
            connect: Box::new(|address, _, _| connected_ssh(address)),
            agent: AgentProbe {
                available: true,
                keys: 2,
            },
            readable: Box::new(|_| FileAccess::Ok),
            env: Vec::new(),
            lookups: Mutex::default(),
            connects: Mutex::default(),
        }
    }
}

#[async_trait]
impl DiagnosticProbes for Fake {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, LookupError> {
        self.lookups.lock().unwrap().push(host.to_string());
        (self.lookup)(host)
    }

    async fn ping(&self, _address: IpAddr) -> PingResult {
        self.ping.clone()
    }

    async fn connect(&self, address: IpAddr, port: u16, read_greeting: bool) -> TcpProbe {
        self.connects
            .lock()
            .unwrap()
            .push((address, port, read_greeting));
        (self.connect)(address, port, read_greeting)
    }

    async fn agent(&self, _socket: &str) -> AgentProbe {
        self.agent
    }

    async fn readable(&self, file: &str) -> FileAccess {
        (self.readable)(file)
    }

    fn env(&self, name: &str) -> Option<String> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn home(&self) -> Option<String> {
        Some("/home/tester".into())
    }
}

fn labels(checks: &[ConnectionCheck]) -> Vec<&str> {
    checks.iter().map(|check| check.label.as_str()).collect()
}

fn check(label: &str, status: ConnectionCheckStatus, detail: &str) -> ConnectionCheck {
    ConnectionCheck {
        label: label.into(),
        status,
        detail: detail.into(),
    }
}

#[test]
fn builds_a_single_bounded_echo_request_per_platform() {
    let strings = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    assert_eq!(
        ping_invocation(v4(), Platform::Windows),
        (
            "ping".into(),
            strings(&["-n", "1", "-w", "2000", "192.0.2.10"])
        )
    );
    assert_eq!(
        ping_invocation(v4(), Platform::Linux),
        (
            "ping".into(),
            strings(&["-c", "1", "-W", "2", "192.0.2.10"])
        )
    );
    assert_eq!(
        ping_invocation(v4(), Platform::MacOs),
        (
            "ping".into(),
            strings(&["-c", "1", "-t", "2", "192.0.2.10"])
        )
    );
    assert_eq!(ping_invocation(v6(), Platform::MacOs).0, "ping6");
    assert_eq!(ping_invocation(v6(), Platform::Linux).0, "ping");
}

#[test]
fn reads_the_round_trip_from_linux_macos_and_translated_windows_output() {
    assert_eq!(
        read_ping_result(
            Some(0),
            "64 bytes from 192.0.2.10: icmp_seq=1 ttl=57 time=12.3 ms",
            v4(),
            Platform::Linux
        ),
        PingResult::Reply {
            rtt: Some("12.3 ms".into())
        }
    );
    assert_eq!(
        read_ping_result(
            Some(0),
            "Antwort von 192.0.2.10: Bytes=32 Zeit<1ms TTL=128",
            v4(),
            Platform::Windows
        ),
        PingResult::Reply {
            rtt: Some("<1 ms".into())
        }
    );
    assert_eq!(
        read_ping_result(
            Some(0),
            "16 bytes from 2001:db8::10, icmp_seq=0 hlim=64 time=0.081 ms",
            v6(),
            Platform::MacOs
        ),
        PingResult::Reply {
            rtt: Some("0.081 ms".into())
        }
    );
}

#[test]
fn does_not_count_a_windows_destination_host_unreachable_answer_as_a_reply() {
    assert_eq!(
        read_ping_result(
            Some(0),
            "Reply from 192.0.2.1: Destination host unreachable.",
            v4(),
            Platform::Windows
        ),
        PingResult::NoReply
    );
}

#[test]
fn separates_a_ping_that_could_not_run_from_one_that_got_no_answer() {
    assert_eq!(
        read_ping_result(Some(2), "", v4(), Platform::MacOs),
        PingResult::NoReply
    );
    assert_eq!(
        read_ping_result(
            Some(2),
            "ping: socket: Operation not permitted\n",
            v4(),
            Platform::Linux
        ),
        PingResult::Unavailable {
            reason: "ping: socket: Operation not permitted".into()
        }
    );
}

#[test]
fn skips_lines_a_server_may_send_before_its_identification() {
    assert_eq!(
        read_greeting(
            b"Welcome to example\r\nSSH-2.0-OpenSSH_9.6p1 Ubuntu\r\n",
            None
        ),
        Some(Greeting::Ssh {
            banner: "SSH-2.0-OpenSSH_9.6p1 Ubuntu".into()
        })
    );
}

#[test]
fn waits_on_a_partial_line_until_reading_is_over() {
    assert_eq!(read_greeting(b"SSH-2.0-Op", None), None);
    assert_eq!(read_greeting(b"HTTP/1.1 400 Bad Request\r\n", None), None);
    assert_eq!(
        read_greeting(b"HTTP/1.1 400 Bad Request\r\n", Some(GreetingEnd::Silent)),
        Some(Greeting::Other {
            text: "HTTP/1.1 400 Bad Request".into()
        })
    );
    assert_eq!(
        read_greeting(b"", Some(GreetingEnd::Closed)),
        Some(Greeting::Closed)
    );
    assert_eq!(
        read_greeting(b"", Some(GreetingEnd::Silent)),
        Some(Greeting::Silent)
    );
}

#[test]
fn keeps_control_bytes_out_of_what_it_quotes() {
    let telnet = [0xff, 0xfd, 0x18, 0x6c, 0x6f, 0x67, 0x69, 0x6e, 0x3a];
    assert_eq!(
        read_greeting(&telnet, Some(GreetingEnd::Silent)),
        Some(Greeting::Other {
            text: "login:".into()
        })
    );
}

async fn listen(on_connection: impl Fn(tokio::net::TcpStream) + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            on_connection(socket);
        }
    });
    port
}

fn loopback() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

async fn probe(port: u16) -> TcpProbe {
    probe_tcp(
        loopback(),
        port,
        true,
        Duration::from_secs(2),
        Duration::from_millis(200),
    )
    .await
}

#[tokio::test]
async fn reads_the_greeting_an_early_close_and_silence() {
    let ssh = listen(|mut socket| {
        tokio::spawn(async move {
            let _ = socket.write_all(b"SSH-2.0-Test\r\n").await;
            let _ = socket.shutdown().await;
        });
    })
    .await;
    let closer = listen(drop).await;
    let held: Arc<Mutex<Vec<tokio::net::TcpStream>>> = Arc::default();
    let keep = held.clone();
    let silent = listen(move |socket| keep.lock().unwrap().push(socket)).await;

    let result = probe(ssh).await;
    assert_eq!(result.outcome, TcpOutcome::Connected);
    assert_eq!(
        result.greeting,
        Some(Greeting::Ssh {
            banner: "SSH-2.0-Test".into()
        })
    );
    assert_eq!(probe(closer).await.greeting, Some(Greeting::Closed));
    let quiet = probe(silent).await;
    assert_eq!(
        (quiet.outcome, quiet.greeting),
        (TcpOutcome::Connected, Some(Greeting::Silent))
    );
}

#[tokio::test]
async fn reports_a_refused_port() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    assert_eq!(probe(port).await.outcome, TcpOutcome::Refused);
}

#[tokio::test]
async fn stops_at_a_name_that_does_not_resolve() {
    let probes = Fake {
        lookup: Box::new(|_| {
            Err(LookupError {
                code: Some("ENOTFOUND".into()),
                message: "getaddrinfo ENOTFOUND".into(),
            })
        }),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    assert_eq!(
        result.checks,
        vec![check(
            "DNS",
            ConnectionCheckStatus::Fail,
            "web.example.com does not resolve"
        )]
    );
    assert!(
        result
            .conclusion
            .unwrap()
            .contains("does not resolve from this computer")
    );
    assert!(probes.connects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn blames_a_firewall_when_the_host_pings_but_the_port_stays_silent() {
    let probes = Fake {
        connect: Box::new(|address, _, _| TcpProbe::new(address, TcpOutcome::Timeout)),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    let statuses: Vec<(&str, ConnectionCheckStatus)> = result
        .checks
        .iter()
        .map(|check| (check.label.as_str(), check.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("DNS", ConnectionCheckStatus::Ok),
            ("Ping", ConnectionCheckStatus::Ok),
            ("TCP", ConnectionCheckStatus::Fail)
        ]
    );
    assert!(
        result
            .conclusion
            .unwrap()
            .contains("answers ping, but port 22 does not answer")
    );
}

#[tokio::test]
async fn calls_a_host_that_answers_nothing_down_or_out_of_reach() {
    let probes = Fake {
        ping: PingResult::NoReply,
        connect: Box::new(|address, _, _| TcpProbe::new(address, TcpOutcome::Timeout)),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    assert!(
        result
            .conclusion
            .unwrap()
            .contains("does not answer from this computer")
    );
}

#[tokio::test]
async fn names_a_refused_port() {
    let probes = Fake {
        connect: Box::new(|address, _, _| TcpProbe::new(address, TcpOutcome::Refused)),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    assert!(
        result
            .conclusion
            .unwrap()
            .contains("nothing accepts connections on port 22")
    );
}

fn no_ipv6_route(address: IpAddr, otherwise: TcpProbe) -> TcpProbe {
    if address.is_ipv6() {
        TcpProbe {
            error: Some("ENETUNREACH".into()),
            ..TcpProbe::new(address, TcpOutcome::Unreachable)
        }
    } else {
        otherwise
    }
}

#[tokio::test]
async fn does_not_let_a_missing_ipv6_route_hide_a_working_ipv4_address() {
    let probes = Fake {
        lookup: Box::new(|_| Ok(vec![v6(), v4()])),
        connect: Box::new(|address, _, _| {
            no_ipv6_route(
                address,
                TcpProbe {
                    ms: Some(3),
                    greeting: Some(Greeting::Ssh {
                        banner: "SSH-2.0-x".into(),
                    }),
                    ..TcpProbe::new(address, TcpOutcome::Connected)
                },
            )
        }),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    assert_eq!(result.conclusion, None);
    let tcp: Vec<ConnectionCheck> = result
        .checks
        .into_iter()
        .filter(|check| check.label == "TCP")
        .collect();
    assert_eq!(
        tcp,
        vec![
            check(
                "TCP",
                ConnectionCheckStatus::Info,
                "[2001:db8::10]:22: this computer has no route to IPv6 networks"
            ),
            check(
                "TCP",
                ConnectionCheckStatus::Ok,
                "192.0.2.10:22 connected in 3 ms"
            ),
        ]
    );
}

#[tokio::test]
async fn judges_a_timeout_by_the_addresses_that_have_a_route() {
    let probes = Fake {
        lookup: Box::new(|_| Ok(vec![v6(), v4()])),
        connect: Box::new(|address, _, _| {
            no_ipv6_route(address, TcpProbe::new(address, TcpOutcome::Timeout))
        }),
        ..Fake::default()
    };
    let result = diagnose_endpoint("web.example.com", 22, Service::Ssh, &probes).await;
    assert!(
        result
            .conclusion
            .unwrap()
            .contains("answers ping, but port 22 does not answer")
    );
}

#[tokio::test]
async fn skips_dns_for_an_ip_address_and_flags_a_port_that_does_not_speak_ssh() {
    let probes = Fake {
        lookup: Box::new(|_| panic!("not called")),
        connect: Box::new(|address, _, _| TcpProbe {
            ms: Some(1),
            greeting: Some(Greeting::Other {
                text: "HTTP/1.1 400 Bad Request".into(),
            }),
            ..TcpProbe::new(address, TcpOutcome::Connected)
        }),
        ..Fake::default()
    };
    let result = diagnose_endpoint("192.0.2.10", 80, Service::Ssh, &probes).await;
    assert_eq!(labels(&result.checks), ["Ping", "TCP", "SSH"]);
    assert_eq!(
        result.conclusion.as_deref(),
        Some("Port 80 answers, but not with SSH. Check the port number.")
    );
}

fn hop(host: &str, hostname: &str, port: u16, user: &str) -> DiagnosticHop {
    DiagnosticHop {
        host: host.into(),
        hostname: hostname.into(),
        port,
        user: user.into(),
        ..DiagnosticHop::default()
    }
}

#[tokio::test]
async fn checks_a_direct_host_down_to_its_keys() {
    // Outside the home directory, so it is quoted as written on every platform.
    let missing = "/nonexistent/muxus/id_missing";
    let chain = [DiagnosticHop {
        identity_files: vec![missing.into()],
        ..hop("web", "web.example.com", 22, "deploy")
    }];
    let probes = Fake {
        agent: AgentProbe {
            available: true,
            keys: 0,
        },
        readable: Box::new(move |file| {
            if file == missing {
                FileAccess::Missing
            } else {
                FileAccess::Ok
            }
        }),
        env: vec![("SSH_AUTH_SOCK".into(), "/run/agent.sock".into())],
        ..Fake::default()
    };
    let report = diagnose_ssh(&chain, &probes).await;
    assert_eq!(report.checked, "web.example.com:22");
    let dns = &report.checks[1];
    assert!(
        dns.detail.starts_with("web.example.com → 192.0.2.10 (") && dns.detail.ends_with(" ms)"),
        "{dns:?}"
    );
    let mut checks = report.checks.clone();
    checks.remove(1);
    assert_eq!(
        checks,
        vec![
            check(
                "Route",
                ConnectionCheckStatus::Info,
                "web → deploy@web.example.com:22"
            ),
            check(
                "Ping",
                ConnectionCheckStatus::Ok,
                "192.0.2.10 replied in 1.2 ms"
            ),
            check(
                "TCP",
                ConnectionCheckStatus::Ok,
                "192.0.2.10:22 connected in 4 ms"
            ),
            check("SSH", ConnectionCheckStatus::Ok, "SSH-2.0-OpenSSH_9.6"),
            check(
                "Agent",
                ConnectionCheckStatus::Warn,
                "the agent is running but holds no keys"
            ),
            check(
                "Key files",
                ConnectionCheckStatus::Warn,
                &format!("{missing} not found")
            ),
        ]
    );
    assert!(
        report
            .conclusion
            .contains("configured key files are missing")
    );
}

#[tokio::test]
async fn checks_the_first_jump_host_the_only_hop_in_reach() {
    let chain = [
        hop("bastion", "bastion.example.com", 2200, "tester"),
        DiagnosticHop {
            identity_agent: Some("none".into()),
            ..hop("app", "app.internal", 22, "tester")
        },
    ];
    let probes = Fake::default();
    let report = diagnose_ssh(&chain, &probes).await;
    assert_eq!(*probes.lookups.lock().unwrap(), ["bastion.example.com"]);
    assert_eq!(report.checked, "bastion.example.com:2200");
    assert_eq!(
        report.checks[0].detail,
        "app → tester@app.internal:22 via bastion"
    );
    assert!(!labels(&report.checks).contains(&"Agent"));
    assert!(
        report
            .conclusion
            .starts_with("bastion is reachable and speaks SSH. app is dialed through it")
    );
}

#[tokio::test]
async fn does_not_dial_around_a_proxy_command() {
    let chain = [DiagnosticHop {
        proxy_command: Some("cloudflared access ssh --hostname edge.internal".into()),
        identity_agent: Some("none".into()),
        ..hop("edge", "edge.internal", 22, "tester")
    }];
    let probes = Fake::default();
    let report = diagnose_ssh(&chain, &probes).await;
    assert!(probes.connects.lock().unwrap().is_empty());
    let statuses: Vec<(&str, ConnectionCheckStatus)> = report
        .checks
        .iter()
        .map(|check| (check.label.as_str(), check.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("Route", ConnectionCheckStatus::Info),
            ("Network", ConnectionCheckStatus::Skipped)
        ]
    );
    assert!(
        report
            .conclusion
            .contains("cloudflared access ssh --hostname edge.internal")
    );
}

#[tokio::test]
async fn leaves_key_checks_out_for_a_password_only_host() {
    let chain = [DiagnosticHop {
        password_only: true,
        ..hop("legacy", "192.0.2.10", 22, "tester")
    }];
    let report = diagnose_ssh(&chain, &Fake::default()).await;
    assert_eq!(labels(&report.checks), ["Route", "Ping", "TCP", "SSH"]);
    assert!(report.conclusion.contains("failed after the greeting"));
}

#[tokio::test]
async fn names_agents_and_key_files_relative_to_home() {
    let chain = [DiagnosticHop {
        identity_files: vec!["/home/tester/.ssh/id_ed25519".into()],
        identity_agent: Some("~/.1password/agent.sock".into()),
        ..hop("web", "web.example.com", 22, "deploy")
    }];
    let probes = Fake {
        agent: AgentProbe {
            available: false,
            keys: 0,
        },
        readable: Box::new(|_| FileAccess::Unreadable),
        ..Fake::default()
    };
    let report = diagnose_ssh(&chain, &probes).await;
    let tail: Vec<ConnectionCheck> = report.checks[report.checks.len() - 2..].to_vec();
    assert_eq!(
        tail,
        vec![
            check(
                "Agent",
                ConnectionCheckStatus::Warn,
                "no agent answered at ~/.1password/agent.sock"
            ),
            check(
                "Key files",
                ConnectionCheckStatus::Warn,
                "~/.ssh/id_ed25519 not readable"
            ),
        ]
    );
}

#[tokio::test]
async fn reads_no_greeting_and_calls_an_open_telnet_port_fine() {
    let probes = Fake {
        connect: Box::new(|address, _, read_greeting| {
            assert!(!read_greeting);
            TcpProbe {
                ms: Some(2),
                ..TcpProbe::new(address, TcpOutcome::Connected)
            }
        }),
        ..Fake::default()
    };
    let report = diagnose_telnet("switch.example.com", 23, &probes).await;
    assert_eq!(report.checked, "switch.example.com:23");
    assert!(
        report
            .conclusion
            .contains("accepts connections, so the network path is fine")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn system_probes_read_this_machine() {
    use muxus_session::diagnostics::SystemProbes;
    use tokio::io::AsyncReadExt;

    let scratch = tempfile::tempdir().unwrap();
    // A fake SSH agent holding three keys.
    let socket = scratch.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 5];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [0, 0, 0, 1, 11]);
        stream
            .write_all(&[0, 0, 0, 5, 12, 0, 0, 0, 3])
            .await
            .unwrap();
    });
    let probes = SystemProbes;
    assert_eq!(
        probes.agent(socket.to_str().unwrap()).await,
        AgentProbe {
            available: true,
            keys: 3
        }
    );
    assert_eq!(
        probes
            .agent(scratch.path().join("none.sock").to_str().unwrap())
            .await,
        AgentProbe {
            available: false,
            keys: 0
        }
    );

    let key = scratch.path().join("id_test");
    std::fs::write(&key, "key").unwrap();
    assert_eq!(probes.readable(key.to_str().unwrap()).await, FileAccess::Ok);
    assert_eq!(
        probes
            .readable(scratch.path().join("missing").to_str().unwrap())
            .await,
        FileAccess::Missing
    );

    let local = probes.lookup("localhost").await.unwrap();
    assert!(
        local.iter().all(IpAddr::is_loopback) && !local.is_empty(),
        "{local:?}"
    );
    // The loopback answers ping wherever ping may run at all.
    assert!(!matches!(
        probes.ping(loopback()).await,
        PingResult::NoReply
    ));
}
