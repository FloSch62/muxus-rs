//! Port of `tests/unit/server/serial-routes.test.ts`, plus the udev listing
//! node-serialport reads on Linux.

use muxus_serial::ports::{empty_port, parse_udevadm_ports};
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
