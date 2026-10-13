//! Ports of `tests/unit/server/host-stats.test.ts` and `tests/unit/client/host-stats.test.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use muxus_core::api::{
    CpuTime, DiskStats, HostStatsResponse, HostStatsSample, MemoryStats, NetworkStats,
};
use muxus_session::stats::view::{
    MeterLevel, StatusBarItem, describe_users, format_bytes, format_percent, format_rate,
    format_uptime, host_stats_view, meter_level, status_bar_item_list, timed_sample,
    with_status_bar_item,
};
use muxus_session::stats::{
    HOST_STATS_SCRIPT, HostStatsReading, RemoteCommand, SharedReadings, host_stats_response,
    parse_host_stats, read_local_host_stats, read_remote_host_stats,
};
use serde_json::json;
use tokio::io::AsyncRead;

fn section(name: &str, lines: &[&str]) -> String {
    std::iter::once(format!("@@muxus:{name}"))
        .chain(lines.iter().map(|line| line.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn linux_output() -> String {
    [
        "Welcome to web-01! Unauthorized access is prohibited.".to_string(),
        String::new(),
        section("begin", &[]),
        section("hostname", &["web-01"]),
        section("kernel", &["Linux 6.8.0-45-generic"]),
        section("os-release", &["NAME=\"Ubuntu\"", "VERSION_ID=\"24.04\"", "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\""]),
        section(
            "who",
            &[
                "alice    pts/0        2026-10-08 10:00 (10.0.0.5)",
                "bob      pts/1        2026-10-08 10:02 (10.0.0.6)",
                "alice    pts/2        2026-10-08 11:30 (10.0.0.5)",
            ],
        ),
        section(
            "df",
            &[
                "Filesystem     1024-blocks     Used Available Capacity Mounted on",
                "/dev/sda1         41152736 18000000  21036000      47% /",
            ],
        ),
        section("uptime", &["1054312.47 4011231.20"]),
        section("stat", &["cpu  10132153 290696 3084719 46828483 16683 0 25195 0 175628 0", "4"]),
        section("loadavg", &["0.12 0.30 0.25 1/234 5678"]),
        section(
            "meminfo",
            &[
                "MemTotal:        4028228 kB",
                "MemFree:          312040 kB",
                "MemAvailable:    2654120 kB",
                "Buffers:          120004 kB",
                "Cached:          2014200 kB",
                "SwapTotal:       2097148 kB",
                "SwapFree:        2000000 kB",
            ],
        ),
        section(
            "route",
            &[
                "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT",
                "docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0",
                "eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0",
                "eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0",
            ],
        ),
        section(
            "netdev",
            &[
                "Inter-|   Receive                                                |  Transmit",
                " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed",
                "    lo:    1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0",
                "docker0: 999999999 10 0 0 0 0 0 0 5 1 0 0 0 0 0 0",
                "  eth0: 123456789  1000    0    0    0     0          0         0 98765432     900    0    0    0     0       0          0",
            ],
        ),
        section("end", &[]),
    ]
    .join("\n")
}

fn mac_output() -> String {
    [
        section("begin", &[]),
        section("hostname", &["studio.local"]),
        section("kernel", &["Darwin 23.5.0"]),
        section("os-release", &[]),
        section("who", &["alice    console  Oct  8 09:00 ", "alice    ttys000  Oct  8 10:00 "]),
        section(
            "df",
            &[
                "Filesystem     1024-blocks      Used Available Capacity  Mounted on",
                "/dev/disk3s1s1   971350180  10340000 600000000     2%    /",
            ],
        ),
        section("sw-vers", &["ProductName:\t\tmacOS", "ProductVersion:\t\t14.5", "BuildVersion:\t\t23F79"]),
        section("now", &["1728400000"]),
        section("boottime", &["{ sec = 1728370000, usec = 123456 } Tue Oct  8 08:46:40 2024"]),
        section("bsd-loadavg", &["{ 1.52 1.71 1.80 }"]),
        section("ncpu", &["10"]),
        section("memsize", &["17179869184"]),
        section(
            "vm-stat",
            &[
                "Mach Virtual Memory Statistics: (page size of 16384 bytes)",
                "Pages free:                               12345.",
                "Pages active:                            300000.",
                "Pages inactive:                          290000.",
                "Pages speculative:                         5000.",
                "Pages wired down:                        150000.",
            ],
        ),
        section("bsd-route", &["   route to: default", "destination: default", "    gateway: 192.168.1.1", "  interface: en0"]),
        section(
            "netstat",
            &[
                "Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll",
                "lo0        16384 <Link#1>                         123456     0   98765432   123456     0   98765432     0",
                "lo0        16384 127           127.0.0.1          123456     -   98765432   123456     -   98765432     -",
                "en0        1500  <Link#6>    a4:83:e7:12:34:56  5000000     0 6000000000  3000000     0  400000000     0",
                "en0        1500  192.168.1     192.168.1.20       5000000     -  6000000000  3000000     -   400000000     -",
            ],
        ),
        section("end", &[]),
    ]
    .join("\n")
}

#[test]
fn feeds_the_same_script_as_the_typescript() {
    let golden = include_str!("fixtures/host-stats-script.sh");
    assert_eq!(HOST_STATS_SCRIPT, golden);
}

#[test]
fn reads_every_linux_source_ignoring_a_banner_before_the_first_marker() {
    assert_eq!(
        parse_host_stats(&linux_output()),
        Some(HostStatsSample {
            hostname: Some("web-01".into()),
            kernel: Some("Linux 6.8.0-45-generic".into()),
            os: Some("Ubuntu 24.04.1 LTS".into()),
            uptime_seconds: Some(1054312.47),
            cpu_time: Some(CpuTime {
                total: (10132153 + 290696 + 3084719 + 46828483 + 16683 + 25195) as f64,
                idle: (46828483 + 16683) as f64,
            }),
            cores: Some(4),
            load_average: Some([0.12, 0.3, 0.25]),
            memory: Some(MemoryStats {
                total_bytes: 4028228 * 1024,
                available_bytes: 2654120 * 1024,
                swap_total_bytes: Some(2097148 * 1024),
                swap_free_bytes: Some(2000000 * 1024),
            }),
            network: Some(NetworkStats {
                interface: "eth0".into(),
                received_bytes: 123456789,
                sent_bytes: 98765432
            }),
            users: Some(vec!["alice".into(), "bob".into(), "alice".into()]),
            disk: Some(DiskStats {
                mount: "/".into(),
                total_bytes: 41152736 * 1024,
                used_bytes: 18000000 * 1024,
                available_bytes: 21036000 * 1024,
            }),
        })
    );
}

#[test]
fn estimates_available_memory_on_old_kernels_and_picks_the_busiest_link_without_a_default_route() {
    let output = [
        section("begin", &[]),
        section(
            "meminfo",
            &[
                "MemTotal: 1000 kB",
                "MemFree: 100 kB",
                "Buffers: 50 kB",
                "Cached: 250 kB",
            ],
        ),
        section("route", &["Iface\tDestination\tGateway"]),
        section(
            "netdev",
            &[
                "Inter-|   Receive",
                "    lo: 5000000 1 0 0 0 0 0 0 5000000 1 0 0 0 0 0 0",
                "  ens3: 300 1 0 0 0 0 0 0 30 1 0 0 0 0 0 0",
                "  ens4: 900 1 0 0 0 0 0 0 90 1 0 0 0 0 0 0",
            ],
        ),
    ]
    .join("\n");
    let sample = parse_host_stats(&output).unwrap();
    let memory = sample.memory.unwrap();
    assert_eq!(
        (memory.total_bytes, memory.available_bytes),
        (1000 * 1024, 400 * 1024)
    );
    assert_eq!(
        sample.network,
        Some(NetworkStats {
            interface: "ens4".into(),
            received_bytes: 900,
            sent_bytes: 90
        })
    );
}

#[test]
fn reads_macos_through_sw_vers_sysctl_vm_stat_route_and_netstat() {
    let sample = parse_host_stats(&mac_output()).unwrap();
    assert_eq!(
        sample,
        HostStatsSample {
            hostname: Some("studio.local".into()),
            kernel: Some("Darwin 23.5.0".into()),
            os: Some("macOS 14.5".into()),
            uptime_seconds: Some(30000.0),
            cpu_time: None,
            cores: Some(10),
            load_average: Some([1.52, 1.71, 1.8]),
            memory: Some(MemoryStats {
                total_bytes: 17179869184,
                available_bytes: (12345 + 290000 + 5000) * 16384,
                swap_total_bytes: None,
                swap_free_bytes: None,
            }),
            network: Some(NetworkStats {
                interface: "en0".into(),
                received_bytes: 6000000000,
                sent_bytes: 400000000
            }),
            users: Some(vec!["alice".into(), "alice".into()]),
            disk: Some(DiskStats {
                mount: "/".into(),
                total_bytes: 971350180 * 1024,
                used_bytes: 10340000 * 1024,
                available_bytes: 600000000 * 1024,
            }),
        }
    );
}

#[test]
fn falls_back_to_the_kernel_for_the_system_name_and_reports_nobody_logged_in() {
    let output = [
        section("begin", &[]),
        section("kernel", &["FreeBSD 14.0-RELEASE"]),
        section("who", &[]),
    ]
    .join("\n");
    assert_eq!(
        parse_host_stats(&output),
        Some(HostStatsSample {
            kernel: Some("FreeBSD 14.0-RELEASE".into()),
            os: Some("FreeBSD 14.0-RELEASE".into()),
            users: Some(vec![]),
            ..HostStatsSample::default()
        })
    );
}

#[test]
fn recognizes_a_host_that_never_ran_the_script() {
    assert_eq!(parse_host_stats(""), None);
    assert_eq!(
        parse_host_stats("% Invalid input detected at '^' marker.\r\n"),
        None
    );
    assert_eq!(
        parse_host_stats("'sh' is not recognized as an internal or external command"),
        None
    );
}

/// A remote host that answers the exec with fixed output.
struct StubHost {
    reply: Result<String, String>,
    execs: AtomicUsize,
    last: std::sync::Mutex<Option<(String, Vec<u8>)>>,
    delay: Duration,
}

impl StubHost {
    fn new(reply: Result<&str, &str>) -> Self {
        Self {
            reply: reply.map(str::to_string).map_err(str::to_string),
            execs: AtomicUsize::new(0),
            last: std::sync::Mutex::new(None),
            delay: Duration::ZERO,
        }
    }
}

#[async_trait]
impl RemoteCommand for StubHost {
    async fn exec(
        &self,
        command: &str,
        stdin: Vec<u8>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, String> {
        self.execs.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = Some((command.to_string(), stdin));
        tokio::time::sleep(self.delay).await;
        let reply = self.reply.clone()?;
        Ok(Box::new(std::io::Cursor::new(reply.into_bytes())))
    }
}

#[tokio::test]
async fn runs_the_script_through_sh_on_stdin_so_the_login_shell_never_parses_it() {
    let host = StubHost::new(Ok(&linux_output()));
    let reading = read_remote_host_stats(&host).await.unwrap();
    let (command, stdin) = host.last.lock().unwrap().clone().unwrap();
    assert_eq!(command, "sh -s");
    assert_eq!(stdin, HOST_STATS_SCRIPT.as_bytes());
    let HostStatsReading::Supported(sample) = reading else {
        panic!("unsupported")
    };
    assert_eq!(sample.hostname.as_deref(), Some("web-01"));
}

#[tokio::test]
async fn reports_a_host_that_answers_without_the_markers_as_unsupported() {
    let host = StubHost::new(Ok("Command not allowed\r\n"));
    assert_eq!(
        read_remote_host_stats(&host).await,
        Ok(HostStatsReading::Unsupported)
    );
}

#[tokio::test]
async fn rejects_when_the_server_refuses_the_channel() {
    let host = StubHost::new(Err("(SSH) Channel open failure: open failed"));
    assert!(
        read_remote_host_stats(&host)
            .await
            .unwrap_err()
            .contains("Channel open failure")
    );
}

#[tokio::test(start_paused = true)]
async fn gives_up_on_a_host_that_never_finishes() {
    /// Prints the beginning of the output, then hangs.
    struct Hanging;
    #[async_trait]
    impl RemoteCommand for Hanging {
        async fn exec(
            &self,
            _command: &str,
            _stdin: Vec<u8>,
        ) -> Result<Box<dyn AsyncRead + Send + Unpin>, String> {
            let (mut tx, rx) = tokio::io::duplex(1024);
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                tx.write_all(b"@@muxus:begin\n@@muxus:hostname\nslow-01\n")
                    .await
                    .unwrap();
                std::future::pending::<()>().await;
            });
            Ok(Box::new(rx))
        }
    }
    let reading = read_remote_host_stats(&Hanging).await.unwrap();
    let HostStatsReading::Supported(sample) = reading else {
        panic!("unsupported")
    };
    assert_eq!(sample.hostname.as_deref(), Some("slow-01"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn runs_the_real_script_on_this_linux_machine_and_gets_every_source() {
    let sample = read_local_host_stats().await;
    assert!(sample.hostname.is_some_and(|name| !name.is_empty()));
    assert!(
        sample
            .kernel
            .as_deref()
            .is_some_and(|kernel| kernel.starts_with("Linux "))
    );
    assert!(sample.os.is_some());
    assert!(sample.uptime_seconds.is_some_and(|uptime| uptime > 0.0));
    assert!(sample.cpu_time.is_some_and(|cpu| cpu.total > 0.0));
    assert!(sample.cores.is_some_and(|cores| cores > 0));
    assert!(sample.load_average.is_some());
    assert!(sample.memory.is_some_and(|memory| memory.total_bytes > 0));
    let disk = sample.disk.unwrap();
    assert_eq!(disk.mount, "/");
    assert!(disk.total_bytes > 0);
}

#[tokio::test]
async fn shares_one_reading_between_requests_for_the_same_connection() {
    let host = Arc::new(StubHost {
        delay: Duration::from_millis(50),
        ..StubHost::new(Ok(&linux_output()))
    });
    let readings = Arc::new(SharedReadings::new());
    let read = |readings: Arc<SharedReadings>, host: Arc<StubHost>| async move {
        readings
            .read("ssh:conn-1", || async {
                read_remote_host_stats(host.as_ref()).await
            })
            .await
    };
    let (first, second) = tokio::join!(
        read(readings.clone(), host.clone()),
        read(readings.clone(), host.clone())
    );
    assert_eq!(host.execs.load(Ordering::SeqCst), 1);
    assert_eq!(first, second);
    // Once it is done, the next poll reads again.
    read(readings, host.clone()).await.unwrap();
    assert_eq!(host.execs.load(Ordering::SeqCst), 2);
}

#[test]
fn answers_ok_with_the_sample_or_unsupported() {
    let sample = HostStatsSample {
        hostname: Some("web-01".into()),
        ..HostStatsSample::default()
    };
    let ok = host_stats_response(
        &HostStatsReading::Supported(Box::new(sample)),
        1_700_000_000_000,
    );
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({ "status": "ok", "sampledAt": 1_700_000_000_000_i64, "sample": { "hostname": "web-01" } })
    );
    assert!(matches!(
        host_stats_response(&HostStatsReading::Unsupported, 0),
        HostStatsResponse::Unsupported { .. }
    ));
}

// --- client view -------------------------------------------------------------

fn reading(at: f64, patch: HostStatsSample) -> muxus_session::stats::view::TimedSample {
    timed_sample(
        HostStatsSample {
            uptime_seconds: Some(at),
            ..patch
        },
        0,
    )
}

fn cpu(total: f64, idle: f64) -> HostStatsSample {
    HostStatsSample {
        cpu_time: Some(CpuTime { total, idle }),
        ..HostStatsSample::default()
    }
}

fn net(interface: &str, received: u64, sent: u64) -> HostStatsSample {
    HostStatsSample {
        network: Some(NetworkStats {
            interface: interface.into(),
            received_bytes: received,
            sent_bytes: sent,
        }),
        ..HostStatsSample::default()
    }
}

#[test]
fn has_no_rates_from_a_single_reading() {
    let view = host_stats_view(&reading(100.0, cpu(1000.0, 800.0)), None);
    assert_eq!((view.cpu, view.receive_rate), (None, None));
}

#[test]
fn measures_cpu_as_the_busy_share_of_the_time_between_two_readings() {
    let view = host_stats_view(
        &reading(103.0, cpu(1400.0, 1100.0)),
        Some(&reading(100.0, cpu(1000.0, 800.0))),
    );
    assert!((view.cpu.unwrap() - 0.25).abs() < 1e-9);
}

#[test]
fn divides_traffic_by_the_seconds_the_host_itself_counted() {
    let view = host_stats_view(
        &reading(104.0, net("eth0", 9000, 2000)),
        Some(&reading(100.0, net("eth0", 1000, 0))),
    );
    assert_eq!(
        (view.receive_rate, view.send_rate),
        (Some(2000.0), Some(500.0))
    );
}

#[test]
fn falls_back_to_the_backend_clock_when_the_host_reports_no_uptime() {
    let view = host_stats_view(
        &timed_sample(net("en0", 300, 0), 5_000),
        Some(&timed_sample(net("en0", 100, 0), 3_000)),
    );
    assert_eq!(view.receive_rate, Some(100.0));
}

#[test]
fn gives_no_rate_for_counters_that_went_backwards_or_moved_to_another_interface() {
    let both = |total, idle, interface: &str, bytes| HostStatsSample {
        cpu_time: cpu(total, idle).cpu_time,
        ..net(interface, bytes, bytes)
    };
    let reset = host_stats_view(
        &reading(5.0, both(10.0, 5.0, "eth0", 10)),
        Some(&reading(100_000.0, both(9_000.0, 8_000.0, "eth0", 900))),
    );
    assert_eq!((reset.cpu, reset.receive_rate), (None, None));
    let moved = host_stats_view(
        &reading(10.0, net("wlan0", 900, 900)),
        Some(&reading(5.0, net("eth0", 100, 100))),
    );
    assert_eq!(moved.receive_rate, None);
}

#[test]
fn writes_sizes_in_binary_units_with_one_decimal_below_100() {
    assert_eq!(format_bytes(0.0), "0 B");
    assert_eq!(format_bytes(512.0), "512 B");
    assert_eq!(format_bytes(1536.0), "1.5 KB");
    assert_eq!(format_bytes(1024.0 * 1024.0), "1 MB");
    assert_eq!(format_bytes(3.84 * 1024f64.powi(3)), "3.8 GB");
    assert_eq!(format_bytes(250.0 * 1024f64.powi(3)), "250 GB");
    assert_eq!(format_rate(2048.0), "2 KB/s");
    // toFixed rounds an exact half up, unlike Rust's formatter.
    assert_eq!(format_bytes(1.25 * 1024.0), "1.3 KB");
    assert_eq!(format_bytes(99.96 * 1024.0), "100 KB");
}

#[test]
fn clamps_percentages() {
    assert_eq!(format_percent(0.254), "25%");
    assert_eq!(format_percent(1.2), "100%");
    assert_eq!(format_percent(-0.1), "0%");
}

#[test]
fn writes_uptime_with_its_two_largest_units() {
    assert_eq!(format_uptime(59.0), "0m");
    assert_eq!(format_uptime(5.0 * 60.0), "5m");
    assert_eq!(format_uptime(3.0 * 3600.0 + 20.0 * 60.0 + 5.0), "3h 20m");
    assert_eq!(format_uptime(2.0 * 3600.0), "2h");
    assert_eq!(
        format_uptime(12.0 * 86400.0 + 4.0 * 3600.0 + 59.0 * 60.0),
        "12d 4h"
    );
    assert_eq!(format_uptime(3.0 * 86400.0 + 30.0 * 60.0), "3d");
}

#[test]
fn lists_users_by_how_many_sessions_they_have() {
    let users: Vec<String> = ["bob", "alice", "alice", "carol"]
        .iter()
        .map(|user| user.to_string())
        .collect();
    assert_eq!(describe_users(&users), "alice (2), bob, carol");
    assert_eq!(describe_users(&[]), "");
}

#[test]
fn warns_at_80_percent_and_turns_critical_at_95() {
    assert_eq!(meter_level(0.5), MeterLevel::Normal);
    assert_eq!(meter_level(0.8), MeterLevel::Warning);
    assert_eq!(meter_level(0.95), MeterLevel::Critical);
}

#[test]
fn keeps_the_bar_order_when_an_item_is_switched_on() {
    use StatusBarItem::*;
    assert_eq!(
        with_status_bar_item(&[Users, Cpu], Hostname, true),
        [Hostname, Cpu, Users]
    );
    assert_eq!(
        with_status_bar_item(&[Hostname, Cpu], Cpu, false),
        [Hostname]
    );
    assert_eq!(with_status_bar_item(&[Cpu], Cpu, true), [Cpu]);
}

#[test]
fn accepts_only_known_unrepeated_items() {
    assert_eq!(
        status_bar_item_list(&json!(["cpu", "disk"])),
        Some(vec![StatusBarItem::Cpu, StatusBarItem::Disk])
    );
    assert_eq!(status_bar_item_list(&json!([])), Some(vec![]));
    assert_eq!(status_bar_item_list(&json!(["cpu", "cpu"])), None);
    assert_eq!(status_bar_item_list(&json!(["gpu"])), None);
    assert_eq!(status_bar_item_list(&json!("cpu")), None);
}
