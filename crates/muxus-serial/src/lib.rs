//! Serial port transport.
//!
//! [`connect`] opens a port with a profile's framing and flow control and
//! hands back `muxus_core::stream` channels; [`list_ports`] enumerates the
//! devices for the host editor.

pub mod ports;
pub mod settings;
pub mod transport;

pub use muxus_core::api::{SerialPortInfo, SerialPortsResponse};
pub use muxus_core::protocol::{
    DEFAULT_BREAK_DURATION_MS, MAX_BREAK_DURATION_MS, SerialFlowControl, SerialParity,
    SerialProfile, StopBits,
};
pub use ports::{list_ports, serial_ports_response};
pub use settings::{SerialOpenOptions, break_duration_ms, serial_open_options};
pub use transport::{BreakObserver, SerialError, SerialLine, connect, start};
