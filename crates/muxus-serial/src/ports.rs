//! Serial port enumeration for the saved-host editor (`server/src/routes/serial.ts`).
//!
//! On Linux the list comes from `udevadm info -e`, read the way
//! node-serialport's `linux-list.js` reads it, so the metadata is the same
//! (MIT, Copyright 2010 Christopher Williams). Elsewhere the `serialport`
//! crate's enumeration supplies it.

use std::cmp::Ordering;
use std::io;

use muxus_core::api::{SerialPortInfo, SerialPortsResponse};

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
    Ok(ports.into_iter().map(port_info).collect())
}

fn port_info(port: serialport::SerialPortInfo) -> SerialPortInfo {
    let mut info = empty_port(port.port_name);
    if let serialport::SerialPortType::UsbPort(usb) = port.port_type {
        info.manufacturer = usb.manufacturer;
        info.serial_number = usb.serial_number;
        info.vendor_id = Some(format!("{:04x}", usb.vid));
        info.product_id = Some(format!("{:04x}", usb.pid));
    }
    info
}

/// Enumerate the OS serial devices, naturally sorted by path.
pub async fn list_ports() -> io::Result<SerialPortsResponse> {
    let ports = tokio::task::spawn_blocking(list_ports_blocking)
        .await
        .map_err(io::Error::other)??;
    Ok(serial_ports_response(ports))
}
