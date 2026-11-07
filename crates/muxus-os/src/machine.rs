//! Readings of this computer for the status bar that the TypeScript took from Node's `os`
//! module (`readLocalHostStats` in `server/src/stats/host-stats.ts`): the CPU-time fallback
//! `nodeCpuTime` reads from `os.cpus()`, and on Windows, which has no `sh` for the stats
//! script, the whole sample (`readWindowsHostStats`).
//!
//! The values follow libuv, which Node's `os` functions call: the same system calls, the
//! same units and the same rounding.

use muxus_core::api::{CpuTime, DiskStats, HostStatsSample, MemoryStats};

/// One core's cumulative times in milliseconds, as `os.cpus()[i].times` reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreTimes {
    pub user: u64,
    pub nice: u64,
    pub sys: u64,
    pub idle: u64,
    pub irq: u64,
}

/// CPU time summed over every core, and the number of cores (`nodeCpuTime`). `None` when
/// there are no cores, as Node's empty `os.cpus()` adds nothing.
pub fn cpu_time(cores: &[CoreTimes]) -> Option<(CpuTime, u32)> {
    if cores.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut idle = 0.0;
    for core in cores {
        total += (core.user + core.nice + core.sys + core.idle + core.irq) as f64;
        idle += core.idle as f64;
    }
    let count = u32::try_from(cores.len()).unwrap_or(u32::MAX);
    Some((CpuTime { total, idle }, count))
}

/// This computer's per-core times (`os.cpus()`): macOS through `host_processor_info`,
/// Windows through `NtQuerySystemInformation`. Empty on other systems, which read them from
/// the stats script, and when the call fails.
pub fn core_times() -> Vec<CoreTimes> {
    #[cfg(target_os = "macos")]
    {
        macos::core_times()
    }
    #[cfg(windows)]
    {
        windows::core_times()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// The CPU fallback of `readLocalHostStats`: a sample without CPU time (macOS has no
/// cumulative counters in the stats script) takes it and the core count from [`core_times`].
pub fn fill_cpu_time(sample: &mut HostStatsSample) {
    if sample.cpu_time.is_some() {
        return;
    }
    if let Some((time, cores)) = cpu_time(&core_times()) {
        sample.cpu_time = Some(time);
        sample.cores = Some(cores);
    }
}

// ---- macOS ----

/// `processor_cpu_load_info.cpu_ticks` indexes (`CPU_STATE_*` in mach/machine.h).
const CPU_STATE_USER: usize = 0;
const CPU_STATE_SYSTEM: usize = 1;
const CPU_STATE_IDLE: usize = 2;
const CPU_STATE_NICE: usize = 3;

/// One core from its Mach tick counters, each tick worth `ms_per_tick` milliseconds
/// (libuv's `uv_cpu_info` on Darwin: no interrupt time).
pub fn mach_core_times(ticks: [u32; 4], ms_per_tick: u64) -> CoreTimes {
    let ms = |state: usize| u64::from(ticks[state]) * ms_per_tick;
    CoreTimes {
        user: ms(CPU_STATE_USER),
        nice: ms(CPU_STATE_NICE),
        sys: ms(CPU_STATE_SYSTEM),
        idle: ms(CPU_STATE_IDLE),
        irq: 0,
    }
}

#[cfg(target_os = "macos")]
mod macos;

// ---- Windows ----

/// One processor's `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` times (100 ns units) in
/// milliseconds, as libuv's `uv_cpu_info` on Windows converts them: kernel time includes the
/// idle time, so system time is the difference; there is no nice time.
pub fn nt_core_times(idle: i64, kernel: i64, user: i64, interrupt: i64) -> CoreTimes {
    let ms = |hundred_ns: i64| u64::try_from(hundred_ns / 10_000).unwrap_or(0);
    CoreTimes {
        user: ms(user),
        nice: 0,
        sys: ms(kernel - idle),
        idle: ms(idle),
        irq: ms(interrupt),
    }
}

/// The running Windows version (`RtlGetVersion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    /// The service pack (`szCSDVersion`), empty on current releases.
    pub service_pack: String,
}

/// `os.version()`: the registry's `ProductName`, then the service pack after a space. Windows
/// 11 still calls itself Windows 10 there, so from build 22000 on the name says 11 (libuv's
/// fix, and `readWindowsHostStats` applies the same replacement).
pub fn windows_version_name(product_name: Option<&str>, version: &WindowsVersion) -> String {
    let mut name = product_name.unwrap_or_default().to_string();
    if version.major == 10
        && version.build >= 22000
        && let Some(rest) = name.strip_prefix("Windows 10")
    {
        name = format!("Windows 11{rest}");
    }
    if !version.service_pack.is_empty() {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(&version.service_pack);
    }
    name
}

/// `${os.type()} ${os.release()}` on Windows.
pub fn windows_kernel(version: &WindowsVersion) -> String {
    format!(
        "Windows_NT {}.{}.{}",
        version.major, version.minor, version.build
    )
}

/// A drive's space from `GetDiskFreeSpaceW`, as Node's `statfs` reports it there (libuv:
/// block size = bytes per sector × sectors per cluster, available = free clusters).
pub fn windows_disk(
    drive: &str,
    sectors_per_cluster: u32,
    bytes_per_sector: u32,
    free_clusters: u32,
    total_clusters: u32,
) -> DiskStats {
    let block = u64::from(bytes_per_sector) * u64::from(sectors_per_cluster);
    let total = u64::from(total_clusters);
    let free = u64::from(free_clusters);
    DiskStats {
        mount: drive.to_string(),
        total_bytes: total * block,
        used_bytes: total.saturating_sub(free) * block,
        available_bytes: free * block,
    }
}

/// What `readWindowsHostStats` reads, before it becomes a sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowsReadings {
    pub hostname: Option<String>,
    pub product_name: Option<String>,
    pub version: Option<WindowsVersion>,
    /// `GetTickCount64`.
    pub uptime_ms: u64,
    /// Physical memory: total and available bytes (`GlobalMemoryStatusEx`).
    pub memory: Option<(u64, u64)>,
    pub cores: Vec<CoreTimes>,
    pub disk: Option<DiskStats>,
}

/// The status bar sample of a Windows computer (`readWindowsHostStats`): no `who`, load
/// average or byte counters.
pub fn windows_sample(readings: WindowsReadings) -> HostStatsSample {
    let (cpu_time, cores) = match cpu_time(&readings.cores) {
        Some((time, cores)) => (Some(time), Some(cores)),
        None => (None, None),
    };
    HostStatsSample {
        hostname: readings.hostname,
        os: readings
            .version
            .as_ref()
            .map(|version| windows_version_name(readings.product_name.as_deref(), version))
            .filter(|name| !name.is_empty()),
        kernel: readings.version.as_ref().map(windows_kernel),
        uptime_seconds: Some(readings.uptime_ms as f64 / 1000.0),
        memory: readings
            .memory
            .map(|(total_bytes, available_bytes)| MemoryStats {
                total_bytes,
                available_bytes,
                swap_total_bytes: None,
                swap_free_bytes: None,
            }),
        cpu_time,
        cores,
        disk: readings.disk,
        ..HostStatsSample::default()
    }
}

/// The drive the status bar reports on Windows: `%SystemDrive%`, else `C:`.
pub fn system_drive(value: Option<&str>) -> String {
    value
        .filter(|drive| !drive.is_empty())
        .unwrap_or("C:")
        .to_string()
}

/// Read this Windows computer (`readWindowsHostStats`). Blocking: it asks the file system for
/// the system drive's space. Off Windows, an empty sample.
pub fn windows_host_stats() -> HostStatsSample {
    #[cfg(windows)]
    {
        windows_sample(windows::readings())
    }
    #[cfg(not(windows))]
    {
        HostStatsSample::default()
    }
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_every_core_like_node_cpu_time() {
        let cores = [
            CoreTimes {
                user: 100,
                nice: 5,
                sys: 50,
                idle: 800,
                irq: 2,
            },
            CoreTimes {
                user: 300,
                nice: 0,
                sys: 100,
                idle: 600,
                irq: 0,
            },
        ];
        let (time, count) = cpu_time(&cores).unwrap();
        assert_eq!(time.total, 957.0 + 1000.0);
        assert_eq!(time.idle, 1400.0);
        assert_eq!(count, 2);
        assert_eq!(cpu_time(&[]), None);
    }

    #[test]
    fn converts_mach_ticks_to_milliseconds() {
        // user, system, idle, nice; 100 ticks per second.
        assert_eq!(
            mach_core_times([7, 3, 90, 1], 10),
            CoreTimes {
                user: 70,
                nice: 10,
                sys: 30,
                idle: 900,
                irq: 0,
            }
        );
    }

    #[test]
    fn converts_nt_processor_times_like_libuv() {
        // 100 ns units; kernel time includes idle time.
        assert_eq!(
            nt_core_times(80_000_000, 95_000_000, 40_000_000, 1_234_567),
            CoreTimes {
                user: 4_000,
                nice: 0,
                sys: 1_500,
                idle: 8_000,
                irq: 123,
            }
        );
    }

    fn version(major: u32, build: u32, service_pack: &str) -> WindowsVersion {
        WindowsVersion {
            major,
            minor: 0,
            build,
            service_pack: service_pack.into(),
        }
    }

    #[test]
    fn names_windows_11_by_its_build() {
        assert_eq!(
            windows_version_name(Some("Windows 10 Pro"), &version(10, 22631, "")),
            "Windows 11 Pro"
        );
        assert_eq!(
            windows_version_name(Some("Windows 10 Pro"), &version(10, 19045, "")),
            "Windows 10 Pro"
        );
        assert_eq!(
            windows_version_name(
                Some("Windows Server 2022 Datacenter"),
                &version(10, 26100, "")
            ),
            "Windows Server 2022 Datacenter"
        );
    }

    #[test]
    fn appends_the_service_pack() {
        assert_eq!(
            windows_version_name(
                Some("Windows 7 Professional"),
                &version(6, 7601, "Service Pack 1")
            ),
            "Windows 7 Professional Service Pack 1"
        );
        assert_eq!(
            windows_version_name(None, &version(6, 7601, "Service Pack 1")),
            "Service Pack 1"
        );
        assert_eq!(windows_version_name(None, &version(10, 22631, "")), "");
    }

    #[test]
    fn names_the_kernel_like_os_type_and_release() {
        let version = WindowsVersion {
            major: 10,
            minor: 0,
            build: 22631,
            service_pack: String::new(),
        };
        assert_eq!(windows_kernel(&version), "Windows_NT 10.0.22631");
    }

    #[test]
    fn reports_drive_space_from_clusters() {
        // 4 KiB clusters: 8 sectors of 512 bytes.
        assert_eq!(
            windows_disk("C:", 8, 512, 25_000_000, 100_000_000),
            DiskStats {
                mount: "C:".into(),
                total_bytes: 409_600_000_000,
                used_bytes: 307_200_000_000,
                available_bytes: 102_400_000_000,
            }
        );
    }

    #[test]
    fn falls_back_to_drive_c() {
        assert_eq!(system_drive(Some("D:")), "D:");
        assert_eq!(system_drive(Some("")), "C:");
        assert_eq!(system_drive(None), "C:");
    }

    #[test]
    fn builds_the_windows_sample_from_its_readings() {
        let sample = windows_sample(WindowsReadings {
            hostname: Some("desk-17".into()),
            product_name: Some("Windows 10 Enterprise".into()),
            version: Some(WindowsVersion {
                major: 10,
                minor: 0,
                build: 22631,
                service_pack: String::new(),
            }),
            uptime_ms: 93_784_500,
            memory: Some((34_359_738_368, 12_884_901_888)),
            cores: vec![
                CoreTimes {
                    user: 10,
                    nice: 0,
                    sys: 5,
                    idle: 85,
                    irq: 0,
                };
                4
            ],
            disk: Some(windows_disk("C:", 8, 512, 1, 2)),
        });
        assert_eq!(sample.hostname.as_deref(), Some("desk-17"));
        assert_eq!(sample.os.as_deref(), Some("Windows 11 Enterprise"));
        assert_eq!(sample.kernel.as_deref(), Some("Windows_NT 10.0.22631"));
        assert_eq!(sample.uptime_seconds, Some(93_784.5));
        assert_eq!(
            sample.memory,
            Some(MemoryStats {
                total_bytes: 34_359_738_368,
                available_bytes: 12_884_901_888,
                swap_total_bytes: None,
                swap_free_bytes: None,
            })
        );
        assert_eq!(
            sample.cpu_time,
            Some(CpuTime {
                total: 400.0,
                idle: 340.0
            })
        );
        assert_eq!(sample.cores, Some(4));
        assert_eq!(
            sample.disk.as_ref().map(|disk| disk.total_bytes),
            Some(8192)
        );
        assert_eq!(sample.users, None);
        assert_eq!(sample.network, None);
        assert_eq!(sample.load_average, None);
    }

    #[test]
    fn leaves_out_what_could_not_be_read() {
        let sample = windows_sample(WindowsReadings::default());
        assert_eq!(sample.os, None);
        assert_eq!(sample.kernel, None);
        assert_eq!(sample.cpu_time, None);
        assert_eq!(sample.cores, None);
        assert_eq!(sample.disk, None);
        assert_eq!(sample.uptime_seconds, Some(0.0));
    }

    #[test]
    fn keeps_the_scripts_cpu_time() {
        let mut sample = HostStatsSample {
            cpu_time: Some(CpuTime {
                total: 10.0,
                idle: 5.0,
            }),
            cores: Some(3),
            ..HostStatsSample::default()
        };
        fill_cpu_time(&mut sample);
        assert_eq!(sample.cores, Some(3));
        assert_eq!(
            sample.cpu_time,
            Some(CpuTime {
                total: 10.0,
                idle: 5.0
            })
        );
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn other_systems_read_cpu_time_from_the_script() {
        assert!(core_times().is_empty());
        let mut sample = HostStatsSample::default();
        fill_cpu_time(&mut sample);
        assert_eq!(sample.cpu_time, None);
    }
}
