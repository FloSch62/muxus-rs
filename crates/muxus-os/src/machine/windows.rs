//! Windows readings, made with the calls libuv makes for Node's `os` functions:
//! `uv_cpu_info` (`NtQuerySystemInformation`), `uv_os_uname` (`RtlGetVersion` and the
//! registry's `ProductName`), `uv_uptime` (`GetTickCount64`), `uv_get_total_memory` and
//! `uv_get_free_memory` (`GlobalMemoryStatusEx`), `uv_fs_statfs` (`GetDiskFreeSpaceW`) and
//! `uv_os_gethostname` (the DNS host name).

use std::mem::size_of;
use std::ptr;

use muxus_core::api::DiskStats;
use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessorPerformanceInformation,
};
use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use windows_sys::Win32::System::SystemInformation::{
    ComputerNameDnsHostname, GetComputerNameExW, GetSystemInfo, GetTickCount64,
    GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW, SYSTEM_INFO,
};
use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

use super::{
    CoreTimes, WindowsReadings, WindowsVersion, nt_core_times, system_drive, windows_disk,
};

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

pub(super) fn readings() -> WindowsReadings {
    let drive = system_drive(std::env::var("SystemDrive").ok().as_deref());
    WindowsReadings {
        hostname: hostname(),
        product_name: product_name(),
        version: version(),
        // SAFETY: no preconditions.
        uptime_ms: unsafe { GetTickCount64() },
        memory: memory(),
        cores: core_times(),
        disk: disk(&drive),
    }
}

pub(super) fn core_times() -> Vec<CoreTimes> {
    let mut system = SYSTEM_INFO::default();
    // SAFETY: `system` is a valid out pointer.
    unsafe { GetSystemInfo(&mut system) };
    let count = system.dwNumberOfProcessors as usize;
    if count == 0 {
        return Vec::new();
    }
    let mut processors = vec![SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION::default(); count];
    let Ok(size) = u32::try_from(count * size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>())
    else {
        return Vec::new();
    };
    let mut returned = 0u32;
    // SAFETY: the buffer holds `size` bytes of processor records and `returned` is a valid out
    // pointer.
    let status = unsafe {
        NtQuerySystemInformation(
            SystemProcessorPerformanceInformation,
            processors.as_mut_ptr().cast(),
            size,
            &mut returned,
        )
    };
    // libuv gives up on a failure or an answer for another number of processors.
    if status < 0 || returned != size {
        return Vec::new();
    }
    processors
        .iter()
        .map(|processor| {
            // `Reserved1` is `[DpcTime, InterruptTime]` (libuv's own declaration names them).
            nt_core_times(
                processor.IdleTime,
                processor.KernelTime,
                processor.UserTime,
                processor.Reserved1[1],
            )
        })
        .collect()
}

fn version() -> Option<WindowsVersion> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: `info` is a valid OSVERSIONINFOW with its size set.
    let status = unsafe { RtlGetVersion(&mut info) };
    (status >= 0).then(|| WindowsVersion {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
        service_pack: until_nul(&info.szCSDVersion),
    })
}

/// `ProductName` under `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`, read from the
/// 64-bit view as libuv does.
fn product_name() -> Option<String> {
    let key = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let value = wide("ProductName");
    let mut buffer = [0u16; 256];
    let mut bytes = (buffer.len() * size_of::<u16>()) as u32;
    // SAFETY: the strings are NUL-terminated, the buffer holds `bytes` bytes and the type
    // pointer may be null.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
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

/// The DNS host name (what `GetHostNameW` answers for `os.hostname()`).
fn hostname() -> Option<String> {
    let mut size = 0u32;
    // SAFETY: asking for the size only: a null buffer with a zero size.
    unsafe { GetComputerNameExW(ComputerNameDnsHostname, ptr::null_mut(), &mut size) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; size as usize];
    // SAFETY: the buffer holds `size` UTF-16 units, the size just asked for.
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return None;
    }
    let name = until_nul(&buffer[..(size as usize).min(buffer.len())]);
    (!name.is_empty()).then_some(name)
}

/// Total and available physical memory.
fn memory() -> Option<(u64, u64)> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: `status` is a valid MEMORYSTATUSEX with its length set.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then_some((status.ullTotalPhys, status.ullAvailPhys))
}

/// The space of `drive` (such as `C:`), through its root folder as `statfs('C:\\')` asks.
fn disk(drive: &str) -> Option<DiskStats> {
    let root = wide(&format!("{drive}\\"));
    let (mut sectors_per_cluster, mut bytes_per_sector) = (0u32, 0u32);
    let (mut free_clusters, mut total_clusters) = (0u32, 0u32);
    // SAFETY: the root is NUL-terminated and the four out pointers are valid.
    let ok = unsafe {
        GetDiskFreeSpaceW(
            root.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            &mut free_clusters,
            &mut total_clusters,
        )
    };
    (ok != 0).then(|| {
        windows_disk(
            drive,
            sectors_per_cluster,
            bytes_per_sector,
            free_clusters,
            total_clusters,
        )
    })
}
