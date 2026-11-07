//! Serial port enumeration for the saved-host editor (`server/src/routes/serial.ts`).
//!
//! The metadata is node-serialport's (@serialport/bindings-cpp, MIT, Copyright 2010
//! Christopher Williams), read the same way on each system:
//!
//! - Linux: `udevadm info -e`, parsed as `linux-list.js` parses it.
//! - Windows: the Ports device class through SetupAPI, as `serialport_win.cpp` lists it
//!   (`pnpId` is the device instance ID, `locationId` its location information, the serial
//!   number from the USB container or else the instance ID as `win32-sn-parser.js` reads it).
//! - macOS: the `serialport` crate's IOKit enumeration, keeping only the dial-in devices
//!   (`/dev/tty.*`) `darwin_list.cpp` reports, with the USB `locationID` as eight hex digits.

use std::cmp::Ordering;
use std::io;

use muxus_core::api::{SerialPortInfo, SerialPortsResponse};

#[cfg(windows)]
mod windows;

/// The ports, naturally sorted by path (COM2 before COM10).
pub fn serial_ports_response(mut ports: Vec<SerialPortInfo>) -> SerialPortsResponse {
    ports.sort_by(|a, b| natural_cmp(&a.path, &b.path));
    SerialPortsResponse { ports }
}

/// `localeCompare` with `{ numeric: true }`: digit runs compare by value,
/// letters ignoring case first.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(ch) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(ch);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut left), take(&mut right));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                left.next();
                right.next();
            }
        }
    }
}

/// A port with nothing known about it but its path.
pub fn empty_port(path: String) -> SerialPortInfo {
    SerialPortInfo {
        path,
        manufacturer: None,
        serial_number: None,
        pnp_id: None,
        location_id: None,
        product_id: None,
        vendor_id: None,
    }
}

/// Serial device names among udev's devices.
fn is_serial_device(name: &str) -> bool {
    [
        "ttyS", "ttyWCH", "ttyACM", "ttyUSB", "ttyAMA", "ttyMFD", "ttyO", "ttyXRUSB", "rfcomm",
    ]
    .iter()
    .any(|prefix| name.contains(prefix))
}

#[derive(Clone, Copy)]
enum Field {
    Path,
    Manufacturer,
    SerialNumber,
    VendorId,
    ProductId,
    PnpId,
}

fn field(name: &str) -> Option<Field> {
    Some(match name.to_uppercase().as_str() {
        "DEVNAME" => Field::Path,
        "ID_VENDOR_ENC" => Field::Manufacturer,
        "ID_SERIAL_SHORT" => Field::SerialNumber,
        "ID_VENDOR_ID" => Field::VendorId,
        "ID_MODEL_ID" => Field::ProductId,
        "DEVLINKS" => Field::PnpId,
        // Workaround for a systemd defect (serialport/bindings-cpp#115).
        "ID_USB_VENDOR_ENC" => Field::Manufacturer,
        "ID_USB_SERIAL_SHORT" => Field::SerialNumber,
        "ID_USB_VENDOR_ID" => Field::VendorId,
        "ID_USB_MODEL_ID" => Field::ProductId,
        _ => return None,
    })
}

/// udev escapes unsafe bytes as `\xNN`.
fn decode_hex_escape(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find("\\x") {
        out.push_str(&rest[..index]);
        let hex = rest.get(index + 2..index + 4);
        match hex.and_then(|hex| {
            u8::from_str_radix(hex, 16)
                .ok()
                .filter(|_| hex.chars().all(|c| c.is_ascii_hexdigit()))
        }) {
            Some(byte) => {
                out.push(char::from(byte));
                rest = &rest[index + 4..];
            }
            None => {
                out.push_str("\\x");
                rest = &rest[index + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn field_value(field: Field, value: &str) -> Option<String> {
    match field {
        Field::PnpId => {
            let start = value.find("/by-id/")? + "/by-id/".len();
            let id: String = value[start..]
                .chars()
                .take_while(|ch| !ch.is_whitespace())
                .collect();
            (!id.is_empty()).then_some(id)
        }
        Field::Manufacturer => Some(decode_hex_escape(value)),
        _ => Some(value.strip_prefix("0x").unwrap_or(value).to_string()),
    }
}

/// Parse `udevadm info -e` output into serial ports.
pub fn parse_udevadm_ports(output: &str) -> Vec<SerialPortInfo> {
    let mut ports = Vec::new();
    let mut current: Option<SerialPortInfo> = None;
    let mut skip = false;
    for line in output.lines() {
        let kind = line.get(..1).unwrap_or("");
        let data = line.get(3..).unwrap_or("");
        if kind == "P" {
            ports.extend(current.take());
            skip = false;
            continue;
        }
        if skip {
            continue;
        }
        // The device name decides whether this entry is a serial port at all.
        if kind == "N" {
            if is_serial_device(data) {
                current = Some(empty_port(String::new()));
            } else {
                skip = true;
            }
            continue;
        }
        if kind == "E"
            && let Some(port) = current.as_mut()
            && let Some((key, value)) = data.split_once('=')
            && !key.is_empty()
            && let Some(field) = field(key)
        {
            let value = field_value(field, value);
            match field {
                Field::Path => port.path = value.unwrap_or_default(),
                Field::Manufacturer => port.manufacturer = value,
                Field::SerialNumber => port.serial_number = value,
                Field::VendorId => port.vendor_id = value,
                Field::ProductId => port.product_id = value,
                Field::PnpId => port.pnp_id = value,
            }
        }
    }
    ports.extend(current);
    ports
}

/// The serial devices this computer has, for the saved-host editor. Blocking:
/// [`list_ports`] runs it off the async executor.
pub fn list_ports_blocking() -> io::Result<Vec<SerialPortInfo>> {
    #[cfg(windows)]
    {
        windows::list_ports()
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("udevadm")
                .args(["info", "-e"])
                .output();
            if let Ok(output) = output
                && output.status.success()
            {
                return Ok(parse_udevadm_ports(&String::from_utf8_lossy(
                    &output.stdout,
                )));
            }
        }
        let ports = serialport::available_ports().map_err(io::Error::from)?;
        Ok(ports
            .into_iter()
            .filter(|port| !cfg!(target_os = "macos") || !is_callout_device(&port.port_name))
            .map(port_info)
            .collect())
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn port_info(port: serialport::SerialPortInfo) -> SerialPortInfo {
    let mut info = empty_port(port.port_name);
    if let serialport::SerialPortType::UsbPort(usb) = port.port_type {
        info.manufacturer = usb.manufacturer;
        info.serial_number = usb.serial_number;
        info.vendor_id = Some(format!("{:04x}", usb.vid));
        info.product_id = Some(format!("{:04x}", usb.pid));
        // Only darwin_list.cpp reports one; the Linux fallback has none, as linux-list.js.
        if cfg!(target_os = "macos") {
            info.location_id = usb
                .location
                .and_then(|location| mac_location_id(location.bus_id(), location.port_chain()));
        }
    }
    info
}

/// macOS lists each port twice, as a callout (`/dev/cu.*`) and a dial-in (`/dev/tty.*`)
/// device; node-serialport reports the dial-in one (`kIODialinDeviceKey`).
pub fn is_callout_device(path: &str) -> bool {
    path.starts_with("/dev/cu.")
}

/// The USB `locationID` (`0xBBPPPPPP`: bus, then one port per nibble) the `serialport` crate
/// split into a bus and its port chain, as `darwin_list.cpp` prints it (`%08x`).
pub fn mac_location_id(bus_id: &str, port_chain: &[u8]) -> Option<String> {
    let bus: u8 = bus_id.parse().ok()?;
    if port_chain.len() > 6 || port_chain.iter().any(|&port| port > 0xf) {
        return None;
    }
    let ports = port_chain
        .iter()
        .chain(std::iter::repeat(&0))
        .take(6)
        .fold(0u32, |id, &port| (id << 4) | u32::from(port));
    Some(format!("{:08x}", (u32::from(bus) << 24) | ports))
}

/// Up to four characters after `marker` (`VID_`, `PID_`) in a Windows device instance ID,
/// as `serialport_win.cpp` cuts the USB ids out of it.
pub fn instance_id_field(pnp_id: &str, marker: &str) -> Option<String> {
    let start = pnp_id.find(marker)? + marker.len();
    Some(pnp_id[start..].chars().take(4).collect())
}

/// A serial number in a Windows device instance ID (`win32-sn-parser.js`): what follows the
/// last backslash of a `USB\` ID, or the FTDI serial of an `FTDIBUS\` ID without its interface
/// letter (`/USB\\(?:.+)\\(.+)/`, `/FTDIBUS\\(?:.+)\+(.+?)A?\\.+/`).
pub fn win32_serial_number(pnp_id: &str) -> Option<String> {
    let chars: Vec<char> = pnp_id.chars().collect();
    usb_instance_serial(&chars).or_else(|| ftdi_instance_serial(&chars))
}

/// Where `prefix` starts in `chars`, leftmost first, as a regular expression scans.
fn match_starts<'a>(chars: &'a [char], prefix: &'a str) -> impl Iterator<Item = usize> + 'a {
    let prefix: Vec<char> = prefix.chars().collect();
    (0..chars.len()).filter(move |&start| chars[start..].starts_with(&prefix))
}

/// `/USB\\(?:.+)\\(.+)/`: the greedy `.+` puts the capture after the last backslash that
/// still has a character after it.
fn usb_instance_serial(chars: &[char]) -> Option<String> {
    match_starts(chars, "USB\\").find_map(|start| {
        let rest = &chars[start + 4..];
        (1..rest.len().saturating_sub(1))
            .rev()
            .find(|&split| rest[split] == '\\')
            .map(|split| rest[split + 1..].iter().collect())
    })
}

/// `/FTDIBUS\\(?:.+)\+(.+?)A?\\.+/`: the last `+` that lets the rest match, then the
/// shortest serial followed by an optional `A`, a backslash and at least one character.
fn ftdi_instance_serial(chars: &[char]) -> Option<String> {
    // `\\.+`: a backslash with at least one character after it.
    let backslash_and_more = |tail: &[char]| tail.first() == Some(&'\\') && tail.len() > 1;
    match_starts(chars, "FTDIBUS\\").find_map(|start| {
        let rest = &chars[start + 8..];
        (1..rest.len())
            .rev()
            .filter(|&plus| rest[plus] == '+')
            .find_map(|plus| {
                let after = &rest[plus + 1..];
                (1..after.len()).find_map(|length| {
                    let tail = &after[length..];
                    let matched = backslash_and_more(tail)
                        || (tail.first() == Some(&'A') && backslash_and_more(&tail[1..]));
                    matched.then(|| after[..length].iter().collect())
                })
            })
    })
}

/// One port of the Ports device class as `serialport_win.cpp` and `win32.js` report it:
/// only devices whose `PortName` names a COM port, empty values left out, the USB ids cut
/// from the instance ID, and the serial number from the USB container or else from the
/// instance ID.
pub fn windows_port(
    pnp_id: &str,
    port_name: Option<String>,
    manufacturer: Option<String>,
    location: Option<String>,
    container_serial_number: Option<String>,
) -> Option<SerialPortInfo> {
    let path = port_name.filter(|name| name.contains("COM"))?;
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let pnp_id = non_empty(Some(pnp_id.to_string()));
    let serial_number = non_empty(container_serial_number)
        .or_else(|| pnp_id.as_deref().and_then(win32_serial_number));
    Some(SerialPortInfo {
        path,
        manufacturer: non_empty(manufacturer),
        serial_number,
        vendor_id: non_empty(
            pnp_id
                .as_deref()
                .and_then(|id| instance_id_field(id, "VID_")),
        ),
        product_id: non_empty(
            pnp_id
                .as_deref()
                .and_then(|id| instance_id_field(id, "PID_")),
        ),
        pnp_id,
        location_id: non_empty(location),
    })
}

/// Enumerate the OS serial devices, naturally sorted by path.
pub async fn list_ports() -> io::Result<SerialPortsResponse> {
    let ports = tokio::task::spawn_blocking(list_ports_blocking)
        .await
        .map_err(io::Error::other)??;
    Ok(serial_ports_response(ports))
}
