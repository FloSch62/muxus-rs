//! The Telnet terminal transport over TCP (`server/src/telnet/telnet-transport.ts`).

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::protocol::TelnetProfile;
use muxus_core::stream::{
    BreakOutcome, TerminalSize, TransportChannels, TransportEnds, TransportEvent, TransportInput,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::codec::{BRK, IAC, TelnetCodec};

/// How long the TCP connection may take before the session fails.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// TCP keepalive probe interval, so a dead path is noticed on an idle console.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const CHANNEL_CAPACITY: usize = 256;
/// How long input queued before a close may take to reach the server.
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const READ_BUFFER: usize = 64 * 1024;

/// Why a Telnet session could not be opened, worded for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelnetError(pub String);

impl fmt::Display for TelnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TelnetError {}

fn friendly_error(error: &io::Error, profile: &TelnetProfile) -> TelnetError {
    let (host, port) = (&profile.host, profile.port);
    TelnetError(match error.kind() {
        io::ErrorKind::ConnectionRefused => format!("Telnet connection refused by {host}:{port}"),
        io::ErrorKind::TimedOut => format!("Telnet connection to {host}:{port} timed out"),
        _ => error.to_string(),
    })
}

/// Open a Telnet session.
///
/// The returned channels follow `muxus_core::stream`. Terminal input is
/// encoded (IAC doubled, NVT newlines until BINARY is negotiated), `Resize`
/// becomes a NAWS update once the server asked for it, `Break` is sent as
/// IAC BRK (its duration has no meaning here). The feedback counts each
/// input once the socket has taken it (a paced paste waits for that) and
/// answers every BREAK once IAC BRK is written. The server closing the
/// connection ends the stream with `Exit { code: None, signal: None }`; a
/// socket error with `Error`. After the terminal sends `Close` (or drops its
/// input sender) the socket is closed and no further events arrive.
/// Must be called within a tokio runtime.
pub async fn connect(
    profile: &TelnetProfile,
    size: TerminalSize,
) -> Result<TransportChannels, TelnetError> {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, dial(profile)).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(TelnetError(format!(
                "Telnet connection to {}:{} timed out",
                profile.host, profile.port
            )));
        }
    };
    let _ = stream.set_nodelay(true);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_INTERVAL)
        .with_interval(KEEPALIVE_INTERVAL);
    let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&keepalive);
    Ok(start(stream, size))
}

async fn dial(profile: &TelnetProfile) -> Result<TcpStream, TelnetError> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((profile.host.as_str(), profile.port))
        .await
        .map_err(|_| TelnetError(format!("Telnet host not found: {}", profile.host)))?
        .collect();
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }
    Err(match last_error {
        Some(err) => friendly_error(&err, profile),
        None => TelnetError(format!("Telnet host not found: {}", profile.host)),
    })
}

/// Run the Telnet protocol over an already connected stream.
pub fn start(stream: TcpStream, size: TerminalSize) -> TransportChannels {
    let (channels, ends) = TransportChannels::pair(CHANNEL_CAPACITY);
    ends.feedback.track_written_input();
    ends.feedback.report_breaks();
    let (read_half, write_half) = stream.into_split();
    let session = Arc::new(Session {
        codec: Mutex::new(TelnetCodec::new(size.cols, size.rows)),
        ends,
        finished: AtomicBool::new(false),
    });
    let (wire_tx, wire_rx) = async_channel::bounded::<Wire>(CHANNEL_CAPACITY);
    let reader = tokio::spawn(read_loop(session.clone(), read_half, wire_tx.clone()));
    let writer = tokio::spawn(write_loop(session.clone(), write_half, wire_rx));
    tokio::spawn(input_loop(session, wire_tx, reader, writer));
    channels
}

/// What the writer puts on the socket.
enum Wire {
    /// Protocol replies and NAWS updates.
    Protocol(Vec<u8>),
    /// Encoded terminal input: one `TransportInput::Data`.
    Input(Vec<u8>),
    /// IAC BRK, answered through the feedback once written.
    Break,
}

struct Session {
    codec: Mutex<TelnetCodec>,
    ends: TransportEnds,
    /// Set once the session has ended, from either side.
    finished: AtomicBool,
}

impl Session {
    fn codec(&self) -> std::sync::MutexGuard<'_, TelnetCodec> {
        self.codec
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// End the session from the network side with a last event.
    async fn finish(&self, event: TransportEvent) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.ends.events.send(event).await;
        self.ends.events.close();
        // Wake the input loop so it tears the socket down.
        self.ends.input.close();
        self.ends.feedback.close();
    }
}

async fn read_loop(
    session: Arc<Session>,
    mut socket: OwnedReadHalf,
    wire: async_channel::Sender<Wire>,
) {
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = match socket.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                session.finish(TransportEvent::Error(err.to_string())).await;
                return;
            }
        };
        let fed = session.codec().feed(&buffer[..read]);
        for reply in fed.replies {
            if wire.send(Wire::Protocol(reply)).await.is_err() {
                return;
            }
        }
        // A full event queue holds the socket unread: TCP flow control.
        if !fed.terminal.is_empty()
            && session
                .ends
                .events
                .send(TransportEvent::Data(fed.terminal))
                .await
                .is_err()
        {
            return;
        }
    }
    let tail = session.codec().flush();
    if let Some(tail) = tail
        && !session.finished.load(Ordering::SeqCst)
    {
        let _ = session.ends.events.send(TransportEvent::Data(tail)).await;
    }
    session
        .finish(TransportEvent::Exit {
            code: None,
            signal: None,
        })
        .await;
}

async fn write_loop(
    session: Arc<Session>,
    mut socket: OwnedWriteHalf,
    wire: async_channel::Receiver<Wire>,
) {
    let feedback = session.ends.feedback.clone();
    while let Ok(item) = wire.recv().await {
        let written = match &item {
            Wire::Protocol(data) | Wire::Input(data) => socket.write_all(data).await,
            // A bare IAC BRK: input escaping does not apply to commands.
            Wire::Break => socket.write_all(&[IAC, BRK]).await,
        };
        if let Err(err) = written {
            if matches!(item, Wire::Break) {
                feedback.break_sent(BreakOutcome::Failed(err.to_string()));
            }
            session.finish(TransportEvent::Error(err.to_string())).await;
            return;
        }
        match item {
            Wire::Input(_) => feedback.input_written(),
            Wire::Break => feedback.break_sent(BreakOutcome::Sent),
            Wire::Protocol(_) => {}
        }
    }
    let _ = socket.shutdown().await;
    feedback.close();
}

async fn input_loop(
    session: Arc<Session>,
    wire: async_channel::Sender<Wire>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
) {
    loop {
        let input = session.ends.input.recv().await;
        let sent = match input {
            Ok(TransportInput::Data(data)) => {
                let encoded = session.codec().encode(&data);
                wire.send(Wire::Input(encoded)).await
            }
            Ok(TransportInput::Resize(size)) => {
                let update = session.codec().resize(size.cols, size.rows);
                match update {
                    Some(update) => wire.send(Wire::Protocol(update)).await,
                    None => Ok(()),
                }
            }
            Ok(TransportInput::Break { .. }) => wire.send(Wire::Break).await,
            Ok(TransportInput::Close) | Err(_) => break,
        };
        if sent.is_err() {
            break;
        }
    }
    // The terminal closed the session, or the network side already ended it.
    session.finished.store(true, Ordering::SeqCst);
    reader.abort();
    session.ends.events.close();
    // What was typed before the close still goes out, unless the server
    // stopped reading; then the socket is dropped.
    wire.close();
    let abort = writer.abort_handle();
    if tokio::time::timeout(CLOSE_FLUSH_TIMEOUT, writer)
        .await
        .is_err()
    {
        abort.abort();
    }
    session.ends.feedback.close();
}
