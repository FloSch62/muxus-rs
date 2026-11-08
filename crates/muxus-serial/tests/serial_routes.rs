//! Port of `tests/unit/server/serial-routes.test.ts`, plus the udev listing
//! node-serialport reads on Linux, and the Windows and macOS metadata it reports.

use muxus_serial::ports::{
    empty_port, instance_id_field, is_callout_device, mac_location_id, parse_udevadm_ports,
    win32_serial_number, windows_port,
};
use muxus_serial::{SerialPortInfo, SerialPortsResponse, serial_ports_response};

#[test]
fn returns_naturally_sorted_os_port_metadata() {
    let response = serial_ports_response(vec![
        SerialPortInfo {
            path: "COM10".into(),
            manufacturer: Some("Acme".into()),
            serial_number: Some("ten".into()),
            ..empty_port(String::new())
        },
        SerialPortInfo {
            path: "COM2".into(),
            vendor_id: Some("1234".into()),
            product_id: Some("5678".into()),
            ..empty_port(String::new())
        },
    ]);
    assert_eq!(
        response,
        SerialPortsResponse {
            ports: vec![
                SerialPortInfo {
                    path: "COM2".into(),
                    product_id: Some("5678".into()),
                    vendor_id: Some("1234".into()),
                    ..empty_port(String::new())
                },
                SerialPortInfo {
                    path: "COM10".into(),
                    manufacturer: Some("Acme".into()),
                    serial_number: Some("ten".into()),
                    ..empty_port(String::new())
                },
            ]
        }
    );
    assert_eq!(
        serde_json::to_value(&response.ports[0]).unwrap(),
        serde_json::json!({ "path": "COM2", "productId": "5678", "vendorId": "1234" })
    );
}

const UDEVADM: &str = "P: /devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/ttyUSB0/tty/ttyUSB0
N: ttyUSB0
L: 0
S: serial/by-id/usb-FTDI_FT232R_USB_UART_A50285BI-if00-port0
S: serial/by-path/pci-0000:00:14.0-usb-0:2:1.0-port0
E: DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/ttyUSB0/tty/ttyUSB0
E: DEVNAME=/dev/ttyUSB0
E: ID_VENDOR_ENC=FTDI\\x20Ltd
E: ID_SERIAL_SHORT=A50285BI
E: ID_VENDOR_ID=0403
E: ID_MODEL_ID=0x6001
E: DEVLINKS=/dev/serial/by-path/pci-0000:00:14.0-usb-0:2:1.0-port0 /dev/serial/by-id/usb-FTDI_FT232R_USB_UART_A50285BI-if00-port0

P: /devices/virtual/net/lo
N: lo
E: DEVNAME=/dev/lo
E: ID_VENDOR_ID=ffff

P: /devices/pnp0/00:05/tty/ttyS0
N: ttyS0
E: DEVNAME=/dev/ttyS0

P: /devices/pci0000:00/0000:00:14.0/usb1/1-3/1-3:1.0/tty/ttyACM0
N: ttyACM0
E: DEVNAME=/dev/ttyACM0
E: ID_USB_VENDOR_ENC=Arduino\\x20\\x28www.arduino.cc\\x29
E: ID_USB_SERIAL_SHORT=7503330303135171A1E1
E: ID_USB_VENDOR_ID=2341
E: ID_USB_MODEL_ID=0043
";

#[test]
fn reads_udev_metadata_like_node_serialport() {
    assert_eq!(
        parse_udevadm_ports(UDEVADM),
        vec![
            SerialPortInfo {
                path: "/dev/ttyUSB0".into(),
                manufacturer: Some("FTDI Ltd".into()),
                serial_number: Some("A50285BI".into()),
                pnp_id: Some("usb-FTDI_FT232R_USB_UART_A50285BI-if00-port0".into()),
                location_id: None,
                product_id: Some("6001".into()),
                vendor_id: Some("0403".into()),
            },
            SerialPortInfo {
                path: "/dev/ttyS0".into(),
                ..empty_port(String::new())
            },
            SerialPortInfo {
                path: "/dev/ttyACM0".into(),
                manufacturer: Some("Arduino (www.arduino.cc)".into()),
                serial_number: Some("7503330303135171A1E1".into()),
                pnp_id: None,
                location_id: None,
                product_id: Some("0043".into()),
                vendor_id: Some("2341".into()),
            },
        ]
    );
}

// ---- Windows (serialport_win.cpp, win32.js, win32-sn-parser.js) ----

#[test]
fn reads_serial_numbers_from_instance_ids_like_win32_sn_parser() {
    // Expected values printed by @serialport/bindings-cpp 13's `serialNumParser` under Node.
    let cases = [
        (
            r"USB\VID_2341&PID_0043\752303138333518011C1",
            Some("752303138333518011C1"),
        ),
        (
            r"FTDIBUS\VID_0403+PID_6015+DO004ZB7A\0000",
            Some("DO004ZB7"),
        ),
        (
            r"FTDIBUS\VID_0403+PID_6001+A50285BIA\0000",
            Some("A50285BI"),
        ),
        (
            r"FTDIBUS\VID_0403+PID_6001+AH05HBFIA\0000",
            Some("AH05HBFI"),
        ),
        (r"FTDIBUS\VID_0403+PID_6001+12345678\0000", Some("12345678")),
        (r"FTDIBUS\A+B\C+D", Some("B")),
        (r"FTDIBUS\VID_0403+PID_6001+AA\0", Some("A")),
        (r"FTDIBUS\VID_0403+PID_6001+A\", None),
        (
            r"USB\VID_2341&PID_8036&MI_00\6&3A4B5C6D&0&0000",
            Some("6&3A4B5C6D&0&0000"),
        ),
        (
            r"USB\VID_1A86&PID_7523\5&2E9A8C3&0&2",
            Some("5&2E9A8C3&0&2"),
        ),
        (r"USB\VID_1A86&PID_7523\", None),
        (r"USB\X\Y\", Some(r"Y\")),
        (r"ACPI\PNP0501\1", None),
        (r"USB\", None),
        (
            r"BTHENUM\{00001101}_LOCALMFG&0002\7&1B2E&0&000000000000_00000000",
            None,
        ),
        (r"ROOT\USB\A\B", Some("B")),
        ("", None),
    ];
    for (pnp_id, expected) in cases {
        assert_eq!(win32_serial_number(pnp_id).as_deref(), expected, "{pnp_id}");
    }
}

#[test]
fn cuts_usb_ids_out_of_the_instance_id() {
    let pnp_id = r"USB\VID_10C4&PID_EA60\0001";
    assert_eq!(instance_id_field(pnp_id, "VID_").as_deref(), Some("10C4"));
    assert_eq!(instance_id_field(pnp_id, "PID_").as_deref(), Some("EA60"));
    assert_eq!(
        instance_id_field(r"USB\VID_12", "VID_").as_deref(),
        Some("12")
    );
    assert_eq!(instance_id_field(r"ACPI\PNP0501\1", "VID_"), None);
}

#[test]
fn lists_a_usb_com_port_like_the_windows_binding() {
    let port = windows_port(
        r"USB\VID_2341&PID_0043\752303138333518011C1",
        Some("COM3".into()),
        Some("Arduino LLC (www.arduino.cc)".into()),
        Some("Port_#0001.Hub_#0004".into()),
        None,
    );
    assert_eq!(
        port,
        Some(SerialPortInfo {
            path: "COM3".into(),
            manufacturer: Some("Arduino LLC (www.arduino.cc)".into()),
            serial_number: Some("752303138333518011C1".into()),
            pnp_id: Some(r"USB\VID_2341&PID_0043\752303138333518011C1".into()),
            location_id: Some("Port_#0001.Hub_#0004".into()),
            product_id: Some("0043".into()),
            vendor_id: Some("2341".into()),
        })
    );
}

#[test]
fn prefers_the_usb_containers_serial_number() {
    let port = windows_port(
        r"USB\VID_2341&PID_8036&MI_00\6&3A4B5C6D&0&0000",
        Some("COM7".into()),
        None,
        None,
        Some("HIDPC0123".into()),
    )
    .unwrap();
    assert_eq!(port.serial_number.as_deref(), Some("HIDPC0123"));
    // An empty container answer is no answer: the instance ID is read instead.
    let port = windows_port(
        r"USB\VID_2341&PID_8036&MI_00\6&3A4B5C6D&0&0000",
        Some("COM7".into()),
        None,
        None,
        Some(String::new()),
    )
    .unwrap();
    assert_eq!(port.serial_number.as_deref(), Some("6&3A4B5C6D&0&0000"));
}

#[test]
fn lists_a_built_in_port_without_usb_metadata() {
    let port = windows_port(
        r"ACPI\PNP0501\1",
        Some("COM1".into()),
        Some("(Standard port types)".into()),
        Some(String::new()),
        None,
    );
    assert_eq!(
        port,
        Some(SerialPortInfo {
            path: "COM1".into(),
            manufacturer: Some("(Standard port types)".into()),
            pnp_id: Some(r"ACPI\PNP0501\1".into()),
            ..empty_port(String::new())
        })
    );
}

#[test]
fn skips_ports_class_devices_that_are_no_com_port() {
    assert_eq!(
        windows_port(r"ACPI\PNP0401\0", Some("LPT1".into()), None, None, None),
        None
    );
    assert_eq!(
        windows_port(r"ACPI\PNP0501\1", None, None, None, None),
        None
    );
}

// ---- macOS (darwin_list.cpp) ----

#[test]
fn keeps_the_dial_in_device_of_each_port() {
    assert!(is_callout_device("/dev/cu.usbmodem1421"));
    assert!(!is_callout_device("/dev/tty.usbmodem1421"));
    assert!(!is_callout_device("/dev/tty.Bluetooth-Incoming-Port"));
}

#[test]
fn prints_the_usb_location_id_as_eight_hex_digits() {
    // `serialport` splits 0x14500000 into bus 20 and the port chain [5].
    assert_eq!(mac_location_id("20", &[5]).as_deref(), Some("14500000"));
    assert_eq!(
        mac_location_id("1", &[1, 4, 2]).as_deref(),
        Some("01142000")
    );
    assert_eq!(
        mac_location_id("255", &[15, 15, 15, 15, 15, 15]).as_deref(),
        Some("ffffffff")
    );
    assert_eq!(mac_location_id("0", &[]).as_deref(), Some("00000000"));
    assert_eq!(mac_location_id("bus", &[1]), None);
    assert_eq!(mac_location_id("1", &[16]), None);
    assert_eq!(mac_location_id("1", &[1; 7]), None);
}
