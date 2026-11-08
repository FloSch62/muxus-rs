//! The serial terminal transport (`server/src/serial/serial-transport.ts`).

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use muxus_core::stream::{
    BreakOutcome, TransportChannels, TransportEnds, TransportEvent, TransportInput,
};
use tokio::time::Sleep;

use muxus_core::protocol::{SerialParity, SerialProfile, StopBits};

use crate::settings::serial_open_options;

const CHANNEL_CAPACITY: usize = 256;
/// Reads wake up this often to notice that the session ended.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const BUSY_RETRY_TOTAL: Duration = Duration::from_millis(2500);
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(150);

/// One open serial line, as the transport needs it. Implemented for the
/// ports of the `serialport` crate; tests supply their own.
pub trait SerialLine: Send + 'static {
    /// An independent handle on the same line, for the reader and the writer.
    fn try_clone_line(&self) -> io::Result<Box<dyn SerialLine>>;
    /// Read what arrived; times out after a short while with `TimedOut`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Wait until everything written has been transmitted on the line, not just queued
    /// (`tcdrain`). Lines without a transmit queue of their own have nothing to wait for.
    fn drain(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Put the line into the break state, or release it. Like node-serialport's
    /// `set({ brk })`, this also asserts DTR and RTS, which can fail on its own
    /// (a pty takes the break but has no modem lines).
    fn set_break(&mut self, on: bool) -> Result<(), String>;
}

/// The message of an OS error, without Rust's "(os error N)" suffix.
fn os_message(err: &dyn fmt::Display) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(index) if text.ends_with(')') => text[..index].to_string(),
        _ => text,
    }
}

fn set_break_on(port: &mut dyn serialport::SerialPort, on: bool) -> Result<(), String> {
    let cannot = |err: serialport::Error| format!("{}, cannot set", os_message(&err));
    if on {
        port.set_break()
    } else {
        port.clear_break()
    }
    .map_err(cannot)?;
    port.write_request_to_send(true).map_err(cannot)?;
    port.write_data_terminal_ready(true).map_err(cannot)?;
    Ok(())
}

impl SerialLine for Box<dyn serialport::SerialPort> {
    fn try_clone_line(&self) -> io::Result<Box<dyn SerialLine>> {
        let clone = self.try_clone().map_err(io::Error::from)?;
        Ok(Box::new(clone))
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(self, buf)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Write::write_all(self, data)
    }

    fn drain(&mut self) -> io::Result<()> {
        // The serial library's flush is tcdrain on Unix and FlushFileBuffers on Windows.
        Write::flush(self)
    }

    fn set_break(&mut self, on: bool) -> Result<(), String> {
        set_break_on(self.as_mut(), on)
    }
}

#[cfg(unix)]
impl SerialLine for serialport::TTYPort {
    fn try_clone_line(&self) -> io::Result<Box<dyn SerialLine>> {
        let clone = self.try_clone_native().map_err(io::Error::from)?;
        Ok(Box::new(clone))
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(self, buf)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Write::write_all(self, data)
    }

    fn drain(&mut self) -> io::Result<()> {
        Write::flush(self)
    }

    fn set_break(&mut self, on: bool) -> Result<(), String> {
        set_break_on(self, on)
    }
}

/// Why a serial port could not be opened, worded for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialError(pub String);

impl fmt::Display for SerialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SerialError {}

fn is_busy(err: &serialport::Error) -> bool {
    err.kind() == serialport::ErrorKind::NoDevice || busy_text(&err.description)
}

fn busy_text(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "cannot lock port",
        "resource busy",
        "access is denied",
        "lock on serial port",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn friendly_error(err: &serialport::Error, path: &str) -> SerialError {
    let lower = err.description.to_lowercase();
    let permission = matches!(
        err.kind(),
        serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied)
    ) || lower.contains("permission denied");
    if permission {
        let hint = if cfg!(target_os = "linux") {
            " Check that your user belongs to the device’s serial-access group (commonly dialout or uucp)."
        } else {
            ""
        };
        return SerialError(format!(
            "Permission denied opening serial port {path}.{hint}"
        ));
    }
    let missing = matches!(
        err.kind(),
        serialport::ErrorKind::Io(io::ErrorKind::NotFound)
    ) || lower.contains("no such file")
        || lower.contains("file not found");
    if missing {
        return SerialError(format!("Serial port not found: {path}"));
    }
    if is_busy(err) {
        return SerialError(format!("Serial port is already in use: {path}"));
    }
    SerialError(err.description.clone())
}

/// Open the device with the profile's framing. The serial library has no
/// mark or space parity and no 1.5 stop bits: on Windows the port is opened
/// without them and they are set in its DCB, as node-serialport sets them;
/// elsewhere they are refused, as node-serialport refuses them on Unix.
pub fn open_port(profile: &SerialProfile) -> Result<Box<dyn SerialLine>, serialport::Error> {
    let options = serial_open_options(profile);
    let invalid =
        |message: String| serialport::Error::new(serialport::ErrorKind::InvalidInput, message);
    let data_bits = match options.data_bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        8 => serialport::DataBits::Eight,
        other => return Err(invalid(format!("Invalid data bits setting {other}"))),
    };
    let overrides = DcbOverrides::for_framing(options.parity, options.stop_bits);
    if cfg!(not(windows)) {
        if overrides.parity.is_some() {
            return Err(invalid(
                "Mark and space parity are not supported on this computer".into(),
            ));
        }
        if overrides.stop_bits.is_some() {
            return Err(invalid(
                "1.5 stop bits are not supported on this computer".into(),
            ));
        }
    }
    let parity = match options.parity {
        SerialParity::None => serialport::Parity::None,
        SerialParity::Even => serialport::Parity::Even,
        SerialParity::Odd => serialport::Parity::Odd,
        // Opened with parity checking on; the DCB then names the parity.
        SerialParity::Mark | SerialParity::Space => serialport::Parity::Odd,
    };
    let stop_bits = match options.stop_bits {
        StopBits::One | StopBits::OnePointFive => serialport::StopBits::One,
        StopBits::Two => serialport::StopBits::Two,
    };
    let flow_control = if options.rtscts {
        serialport::FlowControl::Hardware
    } else if options.xon || options.xoff {
        serialport::FlowControl::Software
    } else {
        serialport::FlowControl::None
    };
    let builder = serialport::new(&options.path, options.baud_rate)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow_control)
        .timeout(READ_TIMEOUT);
    #[cfg(target_os = "macos")]
    {
        let builder = builder.exclusive(options.lock);
        Ok(Box::new(open_with_speed(builder, options.baud_rate)?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // The serial library takes the lock (TIOCEXCL and flock) and clears it on close.
        let builder = builder.exclusive(options.lock);
        Ok(Box::new(builder.open_native()?))
    }
    #[cfg(windows)]
    {
        let port = builder.open_native()?;
        overrides.apply(&port)?;
        let port: Box<dyn serialport::SerialPort> = Box::new(port);
        Ok(Box::new(port))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = overrides;
        Ok(Box::new(builder.open()?))
    }
}

/// Open the device on macOS, where the serial library sets the speed with the IOSSIOSPEED
/// ioctl. A pseudo-terminal (socat's pty pair, a console server's line) has no speed and
/// refuses it with ENOTTY, which failed the whole open: open without a speed, then set it on
/// devices that take one, flushing what arrived before as the library does.
#[cfg(target_os = "macos")]
fn open_with_speed(
    builder: serialport::SerialPortBuilder,
    baud_rate: u32,
) -> Result<serialport::TTYPort, serialport::Error> {
    use serialport::SerialPort as _;

    // The serial library takes the lock (TIOCEXCL and flock) and clears it on close.
    let mut port = builder.baud_rate(0).open_native()?;
    match port.set_baud_rate(baud_rate) {
        Ok(()) => port.clear(serialport::ClearBuffer::All)?,
        // ENOTTY, worded as the serial library's errno table words it.
        Err(err) if err.description == "Not a typewriter" => {}
        Err(err) => return Err(err),
    }
    Ok(port)
}

/// `DCB.Parity` for mark parity (winbase.h `MARKPARITY`).
const DCB_MARK_PARITY: u8 = 3;
/// `DCB.Parity` for space parity (winbase.h `SPACEPARITY`).
const DCB_SPACE_PARITY: u8 = 4;
/// `DCB.StopBits` for 1.5 stop bits (winbase.h `ONE5STOPBITS`).
const DCB_ONE5_STOP_BITS: u8 = 1;

#[cfg(windows)]
const _: () = {
    use windows_sys::Win32::Devices::Communication::{MARKPARITY, ONE5STOPBITS, SPACEPARITY};
    assert!(DCB_MARK_PARITY == MARKPARITY);
    assert!(DCB_SPACE_PARITY == SPACEPARITY);
    assert!(DCB_ONE5_STOP_BITS == ONE5STOPBITS);
};

/// The framing the serial library cannot set, as Windows DCB values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DcbOverrides {
    parity: Option<u8>,
    stop_bits: Option<u8>,
}

impl DcbOverrides {
    fn for_framing(parity: SerialParity, stop_bits: StopBits) -> Self {
        Self {
            parity: match parity {
                SerialParity::Mark => Some(DCB_MARK_PARITY),
                SerialParity::Space => Some(DCB_SPACE_PARITY),
                SerialParity::None | SerialParity::Even | SerialParity::Odd => None,
            },
            stop_bits: (stop_bits == StopBits::OnePointFive).then_some(DCB_ONE5_STOP_BITS),
        }
    }

    /// Write the overrides into the open port's DCB (`GetCommState`/`SetCommState`, as
    /// node-serialport's Windows binding does).
    #[cfg(windows)]
    fn apply(self, port: &serialport::COMPort) -> Result<(), serialport::Error> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Devices::Communication::{DCB, GetCommState, SetCommState};
        use windows_sys::Win32::Foundation::HANDLE;

        if self == Self::default() {
            return Ok(());
        }
        let handle = port.as_raw_handle() as HANDLE;
        // SAFETY: DCB is plain data; all-zero is a valid value to fill in.
        let mut dcb: DCB = unsafe { std::mem::zeroed() };
        dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
        // SAFETY: `handle` is the open port's own handle and `dcb` is a correctly sized DCB.
        if unsafe { GetCommState(handle, &mut dcb) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if let Some(parity) = self.parity {
            dcb.Parity = parity;
            // fParity: check and report parity errors.
            dcb._bitfield |= 1 << 1;
        }
        if let Some(stop_bits) = self.stop_bits {
            dcb.StopBits = stop_bits;
        }
        // SAFETY: as above; SetCommState only reads the DCB.
        if unsafe { SetCommState(handle, &dcb) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

impl SerialLine for Box<dyn SerialLine> {
    fn try_clone_line(&self) -> io::Result<Box<dyn SerialLine>> {
        self.as_ref().try_clone_line()
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.as_mut().read(buf)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.as_mut().write_all(data)
    }

    fn drain(&mut self) -> io::Result<()> {
        self.as_mut().drain()
    }

    fn set_break(&mut self, on: bool) -> Result<(), String> {
        self.as_mut().set_break(on)
    }
}

/// Reports the outcome of each BREAK: `Err` says why it did not go out. The outcome also reaches
/// the stream's `TransportFeedback`.
pub type BreakObserver = Arc<dyn Fn(Result<(), String>) + Send + Sync>;

/// Open a serial session.
///
/// Replacing a live session reopens the device while the previous handle is
/// still releasing its exclusive lock, so a busy port gets a short grace
/// period before failing. The returned channels follow `muxus_core::stream`:
/// see [`start`]. Must be called within a tokio runtime.
pub async fn connect(
    profile: &SerialProfile,
    on_break: Option<BreakObserver>,
) -> Result<TransportChannels, SerialError> {
    let deadline = Instant::now() + BUSY_RETRY_TOTAL;
    loop {
        let attempt = {
            let profile = profile.clone();
            tokio::task::spawn_blocking(move || open_port(&profile))
                .await
                .map_err(|err| SerialError(err.to_string()))?
        };
        match attempt {
            Ok(port) => {
                return start(port, profile, on_break).map_err(|err| SerialError(err.to_string()));
            }
            Err(err) if is_busy(&err) && Instant::now() < deadline => {
                tokio::time::sleep(BUSY_RETRY_DELAY).await;
            }
            Err(err) => return Err(friendly_error(&err, &profile.path)),
        }
    }
}

/// Run a serial session over an open line.
///
/// Bytes read from the line arrive as `Data`; terminal `Data` is written as
/// is. `Resize` has no meaning on a serial link. `Break` holds the line in
/// the break state for the host's break duration ([`crate::settings::break_duration_ms`]
/// when the profile sets one, else the requested duration), then releases
/// it; a second BREAK while the line is held changes nothing on the wire.
/// The line failing ends the stream with `Exit` (the device went away) or
/// `Error` (a write failed). `Close` releases a held break, closes the line
/// and ends the stream without further events. The feedback counts each input
/// once it has been transmitted on the line, and answers every BREAK when the
/// line is released (or the break could not be set).
pub fn start(
    line: impl SerialLine,
    profile: &SerialProfile,
    on_break: Option<BreakObserver>,
) -> io::Result<TransportChannels> {
    let (channels, ends) = TransportChannels::pair(CHANNEL_CAPACITY);
    ends.feedback.track_written_input();
    ends.feedback.report_breaks();
    let reader = line.try_clone_line()?;
    let writer = line.try_clone_line()?;
    let ended = Arc::new(AtomicBool::new(false));
    let (write_tx, write_rx) = async_channel::bounded::<Vec<u8>>(CHANNEL_CAPACITY);
    spawn_reader(reader, ends.clone(), ended.clone());
    spawn_writer(writer, write_rx, ends.clone(), ended.clone());
    let control = Control {
        line: Box::new(line),
        ends,
        writes: write_tx,
        ended,
        profile_break_ms: profile.break_duration_ms,
        on_break,
    };
    tokio::spawn(control.run());
    Ok(channels)
}

fn finish(ends: &TransportEnds, ended: &AtomicBool, event: TransportEvent) {
    if ended.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = ends.events.send_blocking(event);
    ends.events.close();
    ends.input.close();
    ends.feedback.close();
}

fn spawn_reader(mut line: Box<dyn SerialLine>, ends: TransportEnds, ended: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("muxus-serial-read".into())
        .spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            while !ended.load(Ordering::SeqCst) {
                match line.read(&mut buffer) {
                    Ok(0) => continue,
                    Ok(read) => {
                        // A full event queue stops reading: the driver and, with
                        // flow control, the device hold the rest.
                        if ends
                            .events
                            .send_blocking(TransportEvent::Data(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                                | io::ErrorKind::WouldBlock
                        ) => {}
                    Err(_) => {
                        // The device went away (unplugged, hung up): the session is over.
                        finish(
                            &ends,
                            &ended,
                            TransportEvent::Exit {
                                code: None,
                                signal: None,
                            },
                        );
                        break;
                    }
                }
            }
        });
}

fn spawn_writer(
    mut line: Box<dyn SerialLine>,
    queue: async_channel::Receiver<Vec<u8>>,
    ends: TransportEnds,
    ended: Arc<AtomicBool>,
) {
    let _ = thread::Builder::new()
        .name("muxus-serial-write".into())
        .spawn(move || {
            while let Ok(data) = queue.recv_blocking() {
                if let Err(err) = line.write_all(&data) {
                    finish(&ends, &ended, TransportEvent::Error(os_message(&err)));
                    break;
                }
                // Paced pastes time their delays from the line, not the driver queue.
                let _ = line.drain();
                ends.feedback.input_written();
            }
        });
}

struct Control {
    line: Box<dyn SerialLine>,
    ends: TransportEnds,
    writes: async_channel::Sender<Vec<u8>>,
    ended: Arc<AtomicBool>,
    profile_break_ms: Option<u32>,
    on_break: Option<BreakObserver>,
}

impl Control {
    fn report(&self, outcome: Result<(), String>) {
        self.ends.feedback.break_sent(match &outcome {
            Ok(()) => BreakOutcome::Sent,
            Err(problem) => BreakOutcome::Failed(problem.clone()),
        });
        if let Some(observer) = &self.on_break {
            observer(outcome);
        }
    }

    /// Release a held break, reporting how the BREAK went.
    fn release(&mut self) {
        let outcome = self.line.set_break(false);
        self.report(outcome);
    }

    async fn run(mut self) {
        let mut held: Option<std::pin::Pin<Box<Sleep>>> = None;
        loop {
            let input = tokio::select! {
                input = self.ends.input.recv() => input,
                () = async { held.as_mut().expect("guarded").await }, if held.is_some() => {
                    held = None;
                    self.release();
                    continue;
                }
            };
            match input {
                Ok(TransportInput::Data(data)) => {
                    if self.writes.send(data).await.is_err() {
                        break;
                    }
                }
                Ok(TransportInput::Resize(_)) => {}
                Ok(TransportInput::Break { duration_ms }) => {
                    // A second press while the line is held changes nothing on the wire.
                    if held.is_some() {
                        self.report(Ok(()));
                        continue;
                    }
                    if let Err(problem) = self.line.set_break(true) {
                        // The modem lines are set after the break, so a failed
                        // call may still have left the line in break: release it anyway.
                        let _ = self.line.set_break(false);
                        self.report(Err(problem));
                        continue;
                    }
                    let duration = self.profile_break_ms.unwrap_or(duration_ms).max(1);
                    held = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                        duration.into(),
                    ))));
                }
                Ok(TransportInput::Close) | Err(_) => break,
            }
        }
        let user_closed = !self.ended.swap(true, Ordering::SeqCst);
        // Not every driver clears a held break when the port closes.
        if held.is_some() {
            self.release();
        }
        self.writes.close();
        if user_closed {
            self.ends.events.close();
        }
        self.ends.feedback.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_space_parity_and_one_and_a_half_stop_bits_become_dcb_values() {
        assert_eq!(
            DcbOverrides::for_framing(SerialParity::Mark, StopBits::One),
            DcbOverrides {
                parity: Some(3),
                stop_bits: None
            }
        );
        assert_eq!(
            DcbOverrides::for_framing(SerialParity::Space, StopBits::OnePointFive),
            DcbOverrides {
                parity: Some(4),
                stop_bits: Some(1)
            }
        );
        for parity in [SerialParity::None, SerialParity::Even, SerialParity::Odd] {
            for stop_bits in [StopBits::One, StopBits::Two] {
                assert_eq!(
                    DcbOverrides::for_framing(parity, stop_bits),
                    DcbOverrides::default()
                );
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn refuses_mark_space_parity_and_one_and_a_half_stop_bits_off_windows() {
        let refusal = |profile: SerialProfile| match open_port(&profile) {
            Err(err) => (err.kind(), err.description),
            Ok(_) => panic!("the framing should be refused"),
        };
        let mut profile = SerialProfile::new("/dev/muxus-test-no-such-port");
        profile.parity = SerialParity::Space;
        assert_eq!(
            refusal(profile.clone()),
            (
                serialport::ErrorKind::InvalidInput,
                "Mark and space parity are not supported on this computer".to_string()
            )
        );
        profile.parity = SerialParity::None;
        profile.stop_bits = StopBits::OnePointFive;
        assert_eq!(
            refusal(profile),
            (
                serialport::ErrorKind::InvalidInput,
                "1.5 stop bits are not supported on this computer".to_string()
            )
        );
    }
}
