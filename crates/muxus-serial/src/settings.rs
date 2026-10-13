//! Serial line settings: the `SerialProfile` of `muxus_core::protocol` and
//! what the serial library is asked for.

use muxus_core::protocol::{
    DEFAULT_BREAK_DURATION_MS, MAX_BREAK_DURATION_MS, SerialFlowControl, SerialParity,
    SerialProfile, StopBits,
};

/// How long Send BREAK holds this line: the host's own duration, else the default.
pub fn break_duration_ms(profile: &SerialProfile) -> u32 {
    profile
        .break_duration_ms
        .unwrap_or(DEFAULT_BREAK_DURATION_MS)
        .clamp(1, MAX_BREAK_DURATION_MS)
}

/// The options a port is opened with (`serialOpenOptions`), in the terms of
/// node-serialport: framing, flow control, an exclusive lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialOpenOptions {
    pub path: String,
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: StopBits,
    pub parity: SerialParity,
    /// RTS/CTS.
    pub rtscts: bool,
    pub xon: bool,
    pub xoff: bool,
    pub xany: bool,
    pub lock: bool,
}

/// Map a profile to open options.
pub fn serial_open_options(profile: &SerialProfile) -> SerialOpenOptions {
    SerialOpenOptions {
        path: profile.path.clone(),
        baud_rate: profile.baud_rate,
        data_bits: profile.data_bits,
        stop_bits: profile.stop_bits,
        parity: profile.parity,
        rtscts: profile.flow_control == SerialFlowControl::Hardware,
        xon: profile.flow_control == SerialFlowControl::Software,
        xoff: profile.flow_control == SerialFlowControl::Software,
        xany: false,
        lock: true,
    }
}
