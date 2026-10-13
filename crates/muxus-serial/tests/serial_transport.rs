//! Port of `tests/unit/server/serial-transport.test.ts`. A mock line stands in
//! for node-serialport's mock binding; a pty pair carries real bytes.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muxus_core::stream::TransportInput;
use muxus_serial::{
    SerialFlowControl, SerialLine, SerialOpenOptions, SerialParity, SerialProfile, StopBits,
    break_duration_ms, serial_open_options, start,
};

#[test]
fn maps_framing_and_hardware_flow_control_to_the_serial_library() {
    let settings = SerialProfile {
        profile_id: None,
        path: "COM3".into(),
        baud_rate: 921_600,
        data_bits: 8,
        stop_bits: StopBits::One,
        parity: SerialParity::None,
        flow_control: SerialFlowControl::Hardware,
        break_duration_ms: None,
    };
    assert_eq!(
        serial_open_options(&settings),
        SerialOpenOptions {
            path: "COM3".into(),
            baud_rate: 921_600,
            data_bits: 8,
            stop_bits: StopBits::One,
            parity: SerialParity::None,
            rtscts: true,
            xon: false,
            xoff: false,
            xany: false,
            lock: true,
        }
    );
}

#[test]
fn maps_software_flow_control_without_enabling_rts_cts() {
    let settings = SerialProfile {
        profile_id: None,
        path: "/dev/ttyUSB0".into(),
        baud_rate: 9600,
        data_bits: 7,
        stop_bits: StopBits::Two,
        parity: SerialParity::Even,
        flow_control: SerialFlowControl::Software,
        break_duration_ms: None,
    };
    let options = serial_open_options(&settings);
    assert_eq!(
        (options.rtscts, options.xon, options.xoff, options.xany),
        (false, true, true, false)
    );
}

#[test]
fn reads_the_profile_json_with_its_defaults() {
    let settings: SerialProfile =
        serde_json::from_value(serde_json::json!({ "path": "/dev/ttyUSB0" })).unwrap();
    assert_eq!(settings, SerialProfile::new("/dev/ttyUSB0"));
    assert_eq!(break_duration_ms(&settings), 250);
    let settings: SerialProfile = serde_json::from_value(serde_json::json!({
        "path": "COM1", "baudRate": 9600, "dataBits": 7, "stopBits": 1.5,
        "parity": "mark", "flowControl": "software", "breakDurationMs": 400
    }))
    .unwrap();
    assert_eq!(settings.stop_bits, StopBits::OnePointFive);
    assert_eq!(settings.parity, SerialParity::Mark);
    assert_eq!(break_duration_ms(&settings), 400);
    assert_eq!(
        serde_json::to_value(&settings).unwrap()["stopBits"],
        serde_json::json!(1.5)
    );
}

/// What the mock line was asked to do, shared by all its handles.
#[derive(Default)]
struct Wire {
    breaks: Vec<bool>,
    written: Vec<u8>,
    handles: usize,
    fail_set: bool,
}

struct MockLine(Arc<Mutex<Wire>>);

impl MockLine {
    fn new(wire: &Arc<Mutex<Wire>>) -> Self {
        wire.lock().unwrap().handles += 1;
        Self(wire.clone())
    }
}

impl Drop for MockLine {
    fn drop(&mut self) {
        self.0.lock().unwrap().handles -= 1;
    }
}

impl SerialLine for MockLine {
    fn try_clone_line(&self) -> io::Result<Box<dyn SerialLine>> {
        Ok(Box::new(MockLine::new(&self.0)))
    }

    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        std::thread::sleep(Duration::from_millis(5));
        Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().written.extend_from_slice(data);
        Ok(())
    }

    fn set_break(&mut self, on: bool) -> Result<(), String> {
        let mut wire = self.0.lock().unwrap();
        wire.breaks.push(on);
        // A pty, for one: the break ioctl succeeds, setting the modem lines fails.
        if wire.fail_set {
            Err("Inappropriate ioctl for device, cannot set".into())
        } else {
            Ok(())
        }
    }
}

type Outcomes = Arc<Mutex<Vec<Result<(), String>>>>;

fn open_mock(
    break_duration_ms: Option<u32>,
    fail_set: bool,
) -> (
    Arc<Mutex<Wire>>,
    Outcomes,
    muxus_core::stream::TransportChannels,
) {
    let wire = Arc::new(Mutex::new(Wire {
        fail_set,
        ..Wire::default()
    }));
    let outcomes: Outcomes = Arc::default();
    let observer = {
        let outcomes = outcomes.clone();
        Arc::new(move |outcome| outcomes.lock().unwrap().push(outcome))
    };
    let settings = SerialProfile {
        break_duration_ms,
        ..SerialProfile::new("/dev/ttyMOCK0")
    };
    let channels = start(MockLine::new(&wire), &settings, Some(observer)).unwrap();
    (wire, outcomes, channels)
}

fn breaks(wire: &Arc<Mutex<Wire>>) -> Vec<bool> {
    wire.lock().unwrap().breaks.clone()
}

/// Let the transport's tasks run without moving the paused clock.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

async fn advance(ms: u64) {
    settle().await;
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn holds_the_line_in_break_for_the_host_duration_then_releases_it() {
    let (wire, outcomes, channels) = open_mock(Some(400), false);
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    advance(0).await;
    assert_eq!(breaks(&wire), vec![true]);
    advance(399).await;
    assert_eq!(breaks(&wire), vec![true]);
    assert!(outcomes.lock().unwrap().is_empty());
    advance(1).await;
    assert_eq!(breaks(&wire), vec![true, false]);
    assert_eq!(*outcomes.lock().unwrap(), vec![Ok(())]);
    channels.input.send(TransportInput::Close).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn defaults_to_250_ms_and_ignores_a_second_press_while_the_line_is_held() {
    let (wire, outcomes, channels) = open_mock(None, false);
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    advance(100).await;
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    advance(149).await;
    assert_eq!(breaks(&wire), vec![true]);
    // The second press answers at once and changes nothing on the wire.
    assert_eq!(*outcomes.lock().unwrap(), vec![Ok(())]);
    advance(1).await;
    assert_eq!(breaks(&wire), vec![true, false]);
    assert_eq!(*outcomes.lock().unwrap(), vec![Ok(()), Ok(())]);
    channels.input.send(TransportInput::Close).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn releases_a_held_break_before_closing_the_port() {
    let (wire, outcomes, channels) = open_mock(Some(5_000), false);
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    advance(10).await;
    channels.input.send(TransportInput::Close).await.unwrap();
    advance(0).await;
    assert_eq!(breaks(&wire), vec![true, false]);
    assert_eq!(*outcomes.lock().unwrap(), vec![Ok(())]);
    // Every handle on the line is closed shortly after; no events follow.
    for _ in 0..100 {
        if wire.lock().unwrap().handles == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(wire.lock().unwrap().handles, 0);
    assert!(channels.events.recv().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn reports_a_port_that_cannot_take_the_break_releasing_it_anyway() {
    let (wire, outcomes, channels) = open_mock(None, true);
    channels
        .input
        .send(TransportInput::Break { duration_ms: 250 })
        .await
        .unwrap();
    advance(0).await;
    assert_eq!(breaks(&wire), vec![true, false]);
    let outcomes = outcomes.lock().unwrap().clone();
    assert_eq!(outcomes.len(), 1);
    assert!(
        outcomes[0]
            .as_ref()
            .unwrap_err()
            .contains("Inappropriate ioctl")
    );
    channels.input.send(TransportInput::Close).await.unwrap();
}

#[tokio::test]
async fn writes_terminal_input_to_the_line() {
    let (wire, _outcomes, channels) = open_mock(None, false);
    channels
        .input
        .send(TransportInput::Data(b"show version\r".to_vec()))
        .await
        .unwrap();
    channels
        .input
        .send(TransportInput::Resize(Default::default()))
        .await
        .unwrap();
    for _ in 0..100 {
        if !wire.lock().unwrap().written.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(wire.lock().unwrap().written, b"show version\r");
    channels.input.send(TransportInput::Close).await.unwrap();
}

#[cfg(unix)]
mod pty {
    use std::io::{Read, Write};
    use std::time::Duration;

    use muxus_core::stream::{TransportEvent, TransportInput};
    use muxus_serial::{SerialProfile, connect, start};
    use serialport::{SerialPort, TTYPort};
    use tokio::time::timeout;

    async fn next_data(channels: &muxus_core::stream::TransportChannels, until: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        while !received.windows(until.len()).any(|window| window == until) {
            match timeout(Duration::from_secs(5), channels.events.recv())
                .await
                .unwrap()
                .unwrap()
            {
                TransportEvent::Data(data) => received.extend(data),
                other => panic!("unexpected {other:?}"),
            }
        }
        received
    }

    fn read_master(master: &mut TTYPort, until: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut buffer = [0u8; 256];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !received.windows(until.len()).any(|window| window == until) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out: {received:?}"
            );
            match master.read(&mut buffer) {
                Ok(read) => received.extend_from_slice(&buffer[..read]),
                Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {}
                Err(err) => panic!("{err}"),
            }
        }
        received
    }

    #[tokio::test]
    async fn carries_bytes_both_ways_over_a_pty_pair() {
        let (mut master, slave) = TTYPort::pair().unwrap();
        let settings = SerialProfile::new(slave.name().unwrap());
        let channels = start(slave, &settings, None).unwrap();
        master.write_all(b"login: ").unwrap();
        assert_eq!(next_data(&channels, b"login: ").await, b"login: ");
        channels
            .input
            .send(TransportInput::Data(b"admin\r".to_vec()))
            .await
            .unwrap();
        let mut master = tokio::task::spawn_blocking(move || {
            assert_eq!(read_master(&mut master, b"admin\r"), b"admin\r");
            master
        })
        .await
        .unwrap();
        channels.input.send(TransportInput::Close).await.unwrap();
        assert!(
            timeout(Duration::from_secs(5), channels.events.recv())
                .await
                .unwrap()
                .is_err()
        );
        let _ = master.flush();
    }

    #[tokio::test]
    async fn opens_a_device_by_path_with_the_profile_framing() {
        let (mut master, slave) = TTYPort::pair().unwrap();
        let path = slave.name().unwrap();
        let channels = connect(&SerialProfile::new(path.clone()), None)
            .await
            .unwrap();
        master.write_all(b"ready\r\n").unwrap();
        assert_eq!(next_data(&channels, b"ready\r\n").await, b"ready\r\n");
        channels.input.send(TransportInput::Close).await.unwrap();
        drop(slave);
    }

    #[tokio::test]
    async fn ends_the_session_when_the_device_goes_away() {
        let (master, slave) = TTYPort::pair().unwrap();
        let channels = start(slave, &SerialProfile::new("/dev/pts/x"), None).unwrap();
        drop(master);
        let event = timeout(Duration::from_secs(5), channels.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            TransportEvent::Exit {
                code: None,
                signal: None
            }
        );
    }

    #[tokio::test]
    async fn words_open_failures_for_the_user() {
        let missing = connect(&SerialProfile::new("/dev/ttyMUXUS-does-not-exist"), None)
            .await
            .unwrap_err();
        assert_eq!(
            missing.to_string(),
            "Serial port not found: /dev/ttyMUXUS-does-not-exist"
        );

        let (_master, slave) = TTYPort::pair().unwrap();
        let mut space = SerialProfile::new(slave.name().unwrap());
        space.parity = muxus_serial::SerialParity::Space;
        let refused = connect(&space, None).await.unwrap_err();
        assert!(refused.to_string().contains("parity"), "{refused}");
    }

    #[tokio::test]
    async fn retries_a_busy_port_and_then_says_it_is_in_use() {
        let (_master, slave) = TTYPort::pair().unwrap();
        let path = slave.name().unwrap();
        let first = connect(&SerialProfile::new(path.clone()), None)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let busy = connect(&SerialProfile::new(path.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(
            busy.to_string(),
            format!("Serial port is already in use: {path}")
        );
        assert!(
            started.elapsed() >= Duration::from_millis(2000),
            "gave up after {:?}",
            started.elapsed()
        );

        // Closing the first session frees the port for the next one.
        first.input.send(TransportInput::Close).await.unwrap();
        let reopened = connect(&SerialProfile::new(path), None).await.unwrap();
        reopened.input.send(TransportInput::Close).await.unwrap();
    }

    /// The exclusive lock is the terminal's TIOCEXCL flag. Closing must clear it while anything
    /// else keeps the device open (socat's pty pair, a console server's line), or every later
    /// open fails with "already in use".
    #[tokio::test]
    async fn closing_a_session_releases_the_lock_for_the_next_one() {
        // The slave stays open elsewhere, as socat keeps it.
        let (_master, slave) = TTYPort::pair().unwrap();
        let path = slave.name().unwrap();
        for _ in 0..2 {
            let session = connect(&SerialProfile::new(path.clone()), None)
                .await
                .unwrap();
            session.input.send(TransportInput::Close).await.unwrap();
            assert!(
                timeout(Duration::from_secs(5), session.events.recv())
                    .await
                    .unwrap()
                    .is_err()
            );
            // Give the session's control task a moment to drop the line.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Something else may open it too (it would see EBUSY under a lingering lock).
        let plain = serialport::new(&path, 9600).exclusive(false).open();
        assert!(plain.is_ok(), "{:?}", plain.err());
        drop(plain);
        let started = std::time::Instant::now();
        let again = connect(&SerialProfile::new(path.clone()), None).await;
        assert!(again.is_ok(), "{:?}", again.err());
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn reports_why_a_pty_cannot_take_a_break() {
        let (_master, slave) = TTYPort::pair().unwrap();
        let outcomes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer = {
            let outcomes = outcomes.clone();
            std::sync::Arc::new(move |outcome| outcomes.lock().unwrap().push(outcome))
        };
        let settings = SerialProfile::new("/dev/pts/x");
        let channels = start(slave, &settings, Some(observer)).unwrap();
        channels
            .input
            .send(TransportInput::Break { duration_ms: 50 })
            .await
            .unwrap();
        for _ in 0..200 {
            if !outcomes.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let outcome: Vec<Result<(), String>> = outcomes.lock().unwrap().clone();
        assert_eq!(outcome.len(), 1);
        assert!(
            outcome[0].as_ref().unwrap_err().ends_with(", cannot set"),
            "{outcome:?}"
        );
        channels.input.send(TransportInput::Close).await.unwrap();
    }
}
