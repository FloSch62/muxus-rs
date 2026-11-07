//! The Ports device class through SetupAPI (port of `ListBaton::Execute` and
//! `getSerialNumber` in @serialport/bindings-cpp's `serialport_win.cpp`, MIT, Copyright 2010
//! Christopher Williams).

use std::io;
use std::mem::size_of;
use std::ptr;

use muxus_core::api::SerialPortInfo;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIGCF_PROFILE, DIREG_DEV, GUID_DEVCLASS_PORTS, HDEVINFO,
    SP_DEVINFO_DATA, SPDRP_LOCATION_INFORMATION, SPDRP_MFG, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPROPTYPE};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Com::StringFromGUID2;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW,
};
use windows_sys::core::GUID;

use super::{instance_id_field, windows_port};

/// bindings-cpp's string buffers, in UTF-16 units.
const MAX_BUFFER_SIZE: usize = 1000;
const MAX_REGISTRY_KEY_SIZE: usize = 255;

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The UTF-16 text before the first NUL.
fn until_nul(units: &[u16]) -> String {
    let length = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..length])
}

/// The present ports of this hardware profile.
pub(super) fn list_ports() -> io::Result<Vec<SerialPortInfo>> {
    // SAFETY: a valid class GUID, no enumerator and no window.
    let devices: HDEVINFO = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_PORTS,
            ptr::null(),
            0,
            DIGCF_PRESENT | DIGCF_PROFILE,
        )
    };
    if devices == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut ports = Vec::new();
    for index in 0.. {
        let mut device = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ClassGuid: GUID::from_u128(0),
            DevInst: 0,
            Reserved: 0,
        };
        // SAFETY: `devices` is a valid set and `device` has its size set.
        if unsafe { SetupDiEnumDeviceInfo(devices, index, &mut device) } == 0 {
            // ERROR_NO_MORE_ITEMS ends the list; bindings-cpp would go on after another
            // failure with an empty device, which yields nothing either.
            break;
        }
        ports.extend(port(devices, &device));
    }
    // SAFETY: the set created above, not used after this.
    unsafe { SetupDiDestroyDeviceInfoList(devices) };
    Ok(ports)
}

fn port(devices: HDEVINFO, device: &SP_DEVINFO_DATA) -> Option<SerialPortInfo> {
    let pnp_id = instance_id(devices, device).unwrap_or_default();
    let container_serial_number = match (
        instance_id_field(&pnp_id, "VID_"),
        instance_id_field(&pnp_id, "PID_"),
    ) {
        (Some(vendor), Some(product)) => {
            container_serial_number(devices, device, &vendor, &product)
        }
        _ => None,
    };
    windows_port(
        &pnp_id,
        port_name(devices, device),
        registry_property(devices, device, SPDRP_MFG),
        registry_property(devices, device, SPDRP_LOCATION_INFORMATION),
        container_serial_number,
    )
}

/// The device instance ID (`USB\VID_2341&PID_0043\752303138333518011C1`): node-serialport's
/// `pnpId`.
fn instance_id(devices: HDEVINFO, device: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buffer = [0u16; MAX_BUFFER_SIZE];
    let mut required = 0u32;
    // SAFETY: the buffer holds MAX_BUFFER_SIZE UTF-16 units.
    let ok = unsafe {
        SetupDiGetDeviceInstanceIdW(
            devices,
            device,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
        )
    };
    (ok != 0).then(|| until_nul(&buffer))
}

/// A string property of the device (`SPDRP_MFG`, `SPDRP_LOCATION_INFORMATION`).
fn registry_property(devices: HDEVINFO, device: &SP_DEVINFO_DATA, property: u32) -> Option<String> {
    let mut buffer = [0u16; MAX_BUFFER_SIZE];
    let mut kind = 0u32;
    let mut required = 0u32;
    // SAFETY: the buffer holds the byte count passed.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            devices,
            device,
            property,
            &mut kind,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * size_of::<u16>()) as u32,
            &mut required,
        )
    };
    (ok != 0).then(|| until_nul(&buffer))
}

/// A string value of an open registry key.
fn registry_string(key: HKEY, name: &str) -> Option<String> {
    let name = wide(name);
    let mut buffer = [0u16; MAX_BUFFER_SIZE];
    let mut bytes = (buffer.len() * size_of::<u16>()) as u32;
    // SAFETY: the name is NUL-terminated and the buffer holds `bytes` bytes.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (bytes as usize / size_of::<u16>()).min(buffer.len());
    Some(until_nul(&buffer[..units]))
}

/// `PortName` from the device's hardware key (`COM3`; printers have `LPT1`).
fn port_name(devices: HDEVINFO, device: &SP_DEVINFO_DATA) -> Option<String> {
    // SAFETY: a valid set and device; the key is closed below.
    let key =
        unsafe { SetupDiOpenDevRegKey(devices, device, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ) };
    if key == INVALID_HANDLE_VALUE {
        return None;
    }
    let name = registry_string(key, "PortName");
    // SAFETY: the key opened above.
    unsafe { RegCloseKey(key) };
    name
}

/// The serial number of the USB device this port belongs to: the subkey of
/// `HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_x&PID_y` whose `ContainerID` is the port's
/// container (a composite device's interfaces share their device's container).
fn container_serial_number(
    devices: HDEVINFO,
    device: &SP_DEVINFO_DATA,
    vendor: &str,
    product: &str,
) -> Option<String> {
    let mut container = GUID::from_u128(0);
    let mut kind: DEVPROPTYPE = 0;
    let mut required = 0u32;
    // SAFETY: the buffer is one GUID, the property's type.
    let ok = unsafe {
        SetupDiGetDevicePropertyW(
            devices,
            device,
            &DEVPKEY_Device_ContainerId,
            &mut kind,
            ptr::addr_of_mut!(container).cast(),
            size_of::<GUID>() as u32,
            &mut required,
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let mut text = [0u16; 64];
    // SAFETY: the buffer holds the 64 units passed, more than a braced GUID needs.
    if unsafe { StringFromGUID2(&container, text.as_mut_ptr(), text.len() as i32) } == 0 {
        return None;
    }
    // `{XXXXXXXX-XXXX-...}`, compared exactly as bindings-cpp's wcscmp compares it.
    let wanted = until_nul(&text);

    let models = format!(r"SYSTEM\CurrentControlSet\Enum\USB\VID_{vendor}&PID_{product}");
    let key = open_key(&models)?;
    let mut count = 0u32;
    // SAFETY: only the subkey count is asked for; every other out pointer is null.
    let status = unsafe {
        RegQueryInfoKeyW(
            key,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            &mut count,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    let found = (status == ERROR_SUCCESS)
        .then(|| {
            (0..count).find_map(|index| {
                let serial = subkey_name(key, index)?;
                let device_key = open_key(&format!(r"{models}\{serial}"))?;
                let container = registry_string(device_key, "ContainerID");
                // SAFETY: the key opened above.
                unsafe { RegCloseKey(device_key) };
                (container.as_deref() == Some(wanted.as_str())).then_some(serial)
            })
        })
        .flatten();
    // SAFETY: the key opened above.
    unsafe { RegCloseKey(key) };
    found
}

fn open_key(path: &str) -> Option<HKEY> {
    let path = wide(path);
    let mut key: HKEY = 0;
    // SAFETY: the path is NUL-terminated and `key` is a valid out pointer.
    let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &mut key) };
    (status == ERROR_SUCCESS).then_some(key)
}

fn subkey_name(key: HKEY, index: u32) -> Option<String> {
    let mut name = [0u16; MAX_REGISTRY_KEY_SIZE];
    let mut length = name.len() as u32;
    // SAFETY: the buffer holds `length` UTF-16 units; the class and time are not asked for.
    let status = unsafe {
        RegEnumKeyExW(
            key,
            index,
            name.as_mut_ptr(),
            &mut length,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    (status == ERROR_SUCCESS)
        .then(|| String::from_utf16_lossy(&name[..(length as usize).min(name.len())]))
}
