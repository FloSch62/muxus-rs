//! Per-core CPU ticks from Mach (libuv's `uv_cpu_info` on Darwin).

use super::{CPU_STATE_IDLE, CPU_STATE_NICE, CPU_STATE_SYSTEM, CPU_STATE_USER, CoreTimes};

// The tick indexes `mach_core_times` uses are the system's.
const _: () = assert!(
    libc::CPU_STATE_USER as usize == CPU_STATE_USER
        && libc::CPU_STATE_SYSTEM as usize == CPU_STATE_SYSTEM
        && libc::CPU_STATE_IDLE as usize == CPU_STATE_IDLE
        && libc::CPU_STATE_NICE as usize == CPU_STATE_NICE
        && libc::CPU_STATE_MAX == 4
);

/// The host port, asked for once: every `mach_host_self` call adds a reference to the send
/// right, and the status bar reads every few seconds.
fn host_port() -> libc::host_t {
    static HOST: std::sync::OnceLock<libc::host_t> = std::sync::OnceLock::new();
    *HOST.get_or_init(|| {
        // libc marks the Mach port calls deprecated in favour of the mach2 crate, which lacks
        // host_processor_info; the calls themselves are current.
        #[allow(deprecated)]
        // SAFETY: mach_host_self has no preconditions.
        unsafe {
            libc::mach_host_self()
        }
    })
}

pub(super) fn core_times() -> Vec<CoreTimes> {
    // SAFETY: sysconf has no preconditions.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let Ok(ticks_per_second) = u64::try_from(ticks_per_second) else {
        return Vec::new();
    };
    if ticks_per_second == 0 {
        return Vec::new();
    }
    let ms_per_tick = 1000 / ticks_per_second;

    let mut processors: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut info_count: libc::mach_msg_type_number_t = 0;
    // SAFETY: the out pointers are valid; on success the kernel allocates `info` in this task.
    let status = unsafe {
        libc::host_processor_info(
            host_port(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut processors,
            &mut info,
            &mut info_count,
        )
    };
    if status != libc::KERN_SUCCESS || info.is_null() {
        return Vec::new();
    }
    let loads = {
        // SAFETY: on success `info` holds `processors` processor_cpu_load_info records
        // (CPU_STATE_MAX integers each), valid until the vm_deallocate below.
        let records = unsafe {
            std::slice::from_raw_parts(
                info.cast::<libc::processor_cpu_load_info>(),
                processors as usize,
            )
        };
        records
            .iter()
            .map(|record| super::mach_core_times(record.cpu_ticks, ms_per_tick))
            .collect()
    };
    let bytes = info_count as usize * std::mem::size_of::<libc::integer_t>();
    #[allow(deprecated)]
    // SAFETY: `info` was allocated by host_processor_info in this task with `info_count`
    // integers and is not used after this.
    unsafe {
        libc::vm_deallocate(
            libc::mach_task_self(),
            info as libc::vm_address_t,
            bytes as libc::vm_size_t,
        );
    }
    loads
}
