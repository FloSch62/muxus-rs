//! The real network, process and file probes behind the diagnostics.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{
    AgentProbe, CONNECT_TIMEOUT, DiagnosticProbes, FileAccess, GREETING_MAX_BYTES,
    GREETING_TIMEOUT, GreetingEnd, LookupError, PING_TIMEOUT, PingResult, Platform, TcpOutcome,
    TcpProbe, ping_invocation, read_greeting, read_ping_result,
};

/// How long an SSH agent may take to list its keys.
const AGENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Probes that touch the real network, the system `ping`, the SSH agent and the disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbes;

#[async_trait]
impl DiagnosticProbes for SystemProbes {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, LookupError> {
        let host = host.to_string();
        tokio::task::spawn_blocking(move || system_lookup(&host))
            .await
            .map_err(|err| LookupError {
                code: None,
                message: err.to_string(),
            })?
    }

    async fn ping(&self, address: IpAddr) -> PingResult {
        system_ping(address).await
    }

    async fn connect(&self, address: IpAddr, port: u16, read_greeting: bool) -> TcpProbe {
        probe_tcp(
            address,
            port,
            read_greeting,
            CONNECT_TIMEOUT,
            GREETING_TIMEOUT,
        )
        .await
    }

    async fn agent(&self, socket: &str) -> AgentProbe {
        match tokio::time::timeout(AGENT_TIMEOUT, agent_key_count(socket)).await {
            Ok(Ok(keys)) => AgentProbe {
                available: true,
                keys,
            },
            _ => AgentProbe {
                available: false,
                keys: 0,
            },
        }
    }

    async fn readable(&self, file: &str) -> FileAccess {
        match tokio::fs::File::open(file).await {
            Ok(_) => FileAccess::Ok,
            Err(err) if err.kind() == io::ErrorKind::NotFound => FileAccess::Missing,
            Err(_) => FileAccess::Unreadable,
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn home(&self) -> Option<String> {
        std::env::home_dir().map(|home| home.to_string_lossy().into_owned())
    }
}

/// `dns.lookup(host, { all: true })`: the system resolver, every address once.
fn system_lookup(host: &str) -> Result<Vec<IpAddr>, LookupError> {
    let hints = dns_lookup::AddrInfoHints {
        socktype: dns_lookup::SockType::Stream.into(),
        ..Default::default()
    };
    let found = dns_lookup::getaddrinfo(Some(host), None, Some(hints)).map_err(|err| {
        let code = match err.kind() {
            dns_lookup::LookupErrorKind::NoName | dns_lookup::LookupErrorKind::NoData => {
                Some("ENOTFOUND")
            }
            dns_lookup::LookupErrorKind::Again => Some("EAI_AGAIN"),
            _ => None,
        };
        LookupError {
            code: code.map(str::to_string),
            message: io::Error::from(err).to_string(),
        }
    })?;
    let mut addresses = Vec::new();
    for info in found.flatten() {
        let address = info.sockaddr.ip();
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    Ok(addresses)
}

async fn system_ping(address: IpAddr) -> PingResult {
    let platform = Platform::current();
    let (file, args) = ping_invocation(address, platform);
    let mut command = tokio::process::Command::new(&file);
    command
        .args(&args)
        // English output where the platform honours it; the parser does not need it.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return PingResult::Unavailable {
                reason: format!("{file} is not installed"),
            };
        }
        Err(err) => {
            return PingResult::Unavailable {
                reason: err.to_string(),
            };
        }
    };
    match tokio::time::timeout(PING_TIMEOUT, child.wait_with_output()).await {
        Err(_) => PingResult::NoReply,
        Ok(Err(err)) => PingResult::Unavailable {
            reason: err.to_string(),
        },
        Ok(Ok(output)) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            read_ping_result(output.status.code(), &text, address, platform)
        }
    }
}

/// The errno name Node reports for a socket error.
fn errno_code(err: &io::Error) -> Option<&'static str> {
    let code = err.raw_os_error()?;
    #[cfg(unix)]
    {
        let names = [
            (libc::ECONNREFUSED, "ECONNREFUSED"),
            (libc::ETIMEDOUT, "ETIMEDOUT"),
            (libc::EHOSTUNREACH, "EHOSTUNREACH"),
            (libc::ENETUNREACH, "ENETUNREACH"),
            (libc::ENETDOWN, "ENETDOWN"),
            (libc::EHOSTDOWN, "EHOSTDOWN"),
            (libc::ECONNRESET, "ECONNRESET"),
            (libc::EADDRNOTAVAIL, "EADDRNOTAVAIL"),
            (libc::EACCES, "EACCES"),
            (libc::EPERM, "EPERM"),
            (libc::EINVAL, "EINVAL"),
        ];
        names
            .iter()
            .find(|(errno, _)| *errno == code)
            .map(|(_, name)| *name)
    }
    #[cfg(windows)]
    {
        let names = [
            (10061, "ECONNREFUSED"),
            (10060, "ETIMEDOUT"),
            (10065, "EHOSTUNREACH"),
            (10051, "ENETUNREACH"),
            (10050, "ENETDOWN"),
            (10064, "EHOSTDOWN"),
            (10054, "ECONNRESET"),
            (10049, "EADDRNOTAVAIL"),
            (10013, "EACCES"),
        ];
        names
            .iter()
            .find(|(errno, _)| *errno == code)
            .map(|(_, name)| *name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        None
    }
}

/// Open one TCP connection and, for SSH, wait for the server's identification
/// line. Nothing is sent: the server greets first, and closing before our own
/// identification leaves only a pre-auth disconnect in its log.
pub async fn probe_tcp(
    address: IpAddr,
    port: u16,
    read_greeting_too: bool,
    connect_timeout: Duration,
    greeting_timeout: Duration,
) -> TcpProbe {
    let started = Instant::now();
    let mut stream = match tokio::time::timeout(
        connect_timeout,
        TcpStream::connect(SocketAddr::new(address, port)),
    )
    .await
    {
        Err(_) => return TcpProbe::new(address, TcpOutcome::Timeout),
        Ok(Err(err)) => {
            let code = errno_code(&err);
            let outcome = match (err.kind(), code) {
                (io::ErrorKind::ConnectionRefused, _) | (_, Some("ECONNREFUSED")) => {
                    TcpOutcome::Refused
                }
                (io::ErrorKind::TimedOut, _) | (_, Some("ETIMEDOUT")) => TcpOutcome::Timeout,
                (_, Some("EHOSTUNREACH" | "ENETUNREACH" | "ENETDOWN" | "EHOSTDOWN")) => {
                    TcpOutcome::Unreachable
                }
                _ => TcpOutcome::Error,
            };
            let mut probe = TcpProbe::new(address, outcome);
            if matches!(outcome, TcpOutcome::Unreachable | TcpOutcome::Error) {
                probe.error = Some(code.map_or_else(|| err.to_string(), str::to_string));
            }
            return probe;
        }
        Ok(Ok(stream)) => stream,
    };
    let mut probe = TcpProbe::new(address, TcpOutcome::Connected);
    probe.ms = Some(started.elapsed().as_millis() as u64);
    if !read_greeting_too {
        return probe;
    }
    let mut received = Vec::new();
    let deadline = tokio::time::sleep(greeting_timeout);
    tokio::pin!(deadline);
    let mut buffer = [0u8; 1024];
    let greeting = loop {
        tokio::select! {
            read = stream.read(&mut buffer) => match read {
                // An error after the connection is up reads as the server closing it.
                Ok(0) | Err(_) => break read_greeting(&received, Some(GreetingEnd::Closed)),
                Ok(read) => {
                    received.extend_from_slice(&buffer[..read]);
                    received.truncate(GREETING_MAX_BYTES);
                    let full = received.len() >= GREETING_MAX_BYTES;
                    if let Some(greeting) = read_greeting(&received, full.then_some(GreetingEnd::Full)) {
                        break Some(greeting);
                    }
                }
            },
            () = &mut deadline => break read_greeting(&received, Some(GreetingEnd::Silent)),
        }
    };
    let _ = stream.shutdown().await;
    probe.greeting = greeting;
    probe
}

const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;

/// Ask an SSH agent how many identities it holds.
async fn agent_key_count(socket: &str) -> io::Result<usize> {
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    #[cfg(windows)]
    let mut stream = tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?;
    #[cfg(not(any(unix, windows)))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        socket.to_string(),
    ));

    #[cfg(any(unix, windows))]
    {
        stream
            .write_all(&[0, 0, 0, 1, SSH_AGENTC_REQUEST_IDENTITIES])
            .await?;
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).await?;
        let length = u32::from_be_bytes(length) as usize;
        if !(5..=256 * 1024).contains(&length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected agent answer",
            ));
        }
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await?;
        if body[0] != SSH_AGENT_IDENTITIES_ANSWER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected agent answer",
            ));
        }
        Ok(u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize)
    }
}
