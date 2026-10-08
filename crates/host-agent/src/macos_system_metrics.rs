//! Bounded machine and process CPU telemetry for the macOS host.

use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CpuTicks {
    user: u64,
    system: u64,
    nice: u64,
    idle: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CpuUsageTracker {
    previous: Option<CpuTicks>,
}

impl CpuUsageTracker {
    fn sample_tenths(&mut self, current: Option<CpuTicks>) -> Option<u16> {
        let current = current?;
        let previous = self.previous.replace(current)?;
        utilization_tenths(previous, current)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessCpuUsageTracker {
    previous: Option<(u128, Instant)>,
}

impl ProcessCpuUsageTracker {
    fn sample_tenths(
        &mut self,
        current_cpu_ns: Option<u128>,
        sampled_at: Instant,
        logical_processors: Option<usize>,
    ) -> Option<u16> {
        let current_cpu_ns = current_cpu_ns?;
        let previous = self.previous.replace((current_cpu_ns, sampled_at))?;
        let elapsed_ns = sampled_at.checked_duration_since(previous.1)?.as_nanos();
        process_utilization_tenths(previous.0, current_cpu_ns, elapsed_ns, logical_processors?)
    }
}

fn utilization_tenths(previous: CpuTicks, current: CpuTicks) -> Option<u16> {
    let user = current.user.checked_sub(previous.user)?;
    let system = current.system.checked_sub(previous.system)?;
    let nice = current.nice.checked_sub(previous.nice)?;
    let idle = current.idle.checked_sub(previous.idle)?;
    let total = u128::from(user) + u128::from(system) + u128::from(nice) + u128::from(idle);
    if total == 0 {
        return None;
    }
    let busy = total.checked_sub(u128::from(idle))?;
    u16::try_from(busy.saturating_mul(1_000).saturating_div(total).min(1_000)).ok()
}

fn process_utilization_tenths(
    previous_cpu_ns: u128,
    current_cpu_ns: u128,
    elapsed_ns: u128,
    logical_processors: usize,
) -> Option<u16> {
    let process_cpu_ns = current_cpu_ns.checked_sub(previous_cpu_ns)?;
    if elapsed_ns == 0 || logical_processors == 0 {
        return None;
    }
    let total_machine_capacity_ns = elapsed_ns.checked_mul(logical_processors as u128)?;
    let tenths = process_cpu_ns
        .saturating_mul(1_000)
        .checked_div(total_machine_capacity_ns)?
        .min(1_000);
    u16::try_from(tenths).ok()
}

/// Samples aggregate machine-wide CPU usage in tenths of a percent.
///
/// The sampler retains one Mach host-port right and reads aggregate CPU ticks and this process's
/// CPU time once per StatsReport interval.
#[cfg(target_os = "macos")]
pub struct SystemCpuSampler {
    host: libc::host_t,
    tracker: CpuUsageTracker,
    process_tracker: ProcessCpuUsageTracker,
    logical_processors: Option<usize>,
}

#[cfg(target_os = "macos")]
impl Default for SystemCpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl SystemCpuSampler {
    /// Creates a sampler and primes its baseline when the kernel query succeeds.
    pub fn new() -> Self {
        // SAFETY: `mach_host_self` has no pointer arguments or preconditions and returns the
        // current task's send right to the host port. It is retained once for this sampler's
        // lifetime; the kernel releases task rights when the foreground host process exits.
        #[allow(deprecated)]
        let host = unsafe { libc::mach_host_self() };
        let mut tracker = CpuUsageTracker::default();
        let _ = tracker.sample_tenths(read_cpu_ticks(host));
        let logical_processors = std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZeroUsize::get);
        let mut process_tracker = ProcessCpuUsageTracker::default();
        let _ = process_tracker.sample_tenths(
            read_process_cpu_ns(),
            Instant::now(),
            logical_processors,
        );
        Self {
            host,
            tracker,
            process_tracker,
            logical_processors,
        }
    }

    /// Returns machine-wide CPU usage, or `None` when the kernel sample or tick delta is invalid.
    pub fn sample_tenths(&mut self) -> Option<u16> {
        self.tracker.sample_tenths(read_cpu_ticks(self.host))
    }

    /// Returns this host-agent process's share of total machine CPU capacity in tenths of a
    /// percent, or `None` when the process or timing sample is unavailable.
    pub fn process_sample_tenths(&mut self) -> Option<u16> {
        self.process_tracker.sample_tenths(
            read_process_cpu_ns(),
            Instant::now(),
            self.logical_processors,
        )
    }
}

#[cfg(target_os = "macos")]
fn read_process_cpu_ns() -> Option<u128> {
    // SAFETY: `rusage` is a plain C output structure. Zero initialization gives the kernel a
    // correctly aligned writable buffer, and `getrusage` writes the complete structure on success.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid writable pointer to the correctly sized `rusage` output buffer;
    // RUSAGE_SELF is a valid selector and has no additional preconditions.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if status != 0 {
        return None;
    }
    let user_ns = timeval_to_ns(usage.ru_utime)?;
    let system_ns = timeval_to_ns(usage.ru_stime)?;
    user_ns.checked_add(system_ns)
}

#[cfg(target_os = "macos")]
fn timeval_to_ns(value: libc::timeval) -> Option<u128> {
    let seconds = u128::try_from(value.tv_sec).ok()?;
    let microseconds = u128::try_from(value.tv_usec).ok()?;
    if microseconds >= 1_000_000 {
        return None;
    }
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(microseconds.checked_mul(1_000)?)
}

#[cfg(target_os = "macos")]
fn read_cpu_ticks(host: libc::host_t) -> Option<CpuTicks> {
    let mut ticks = [0_u32; libc::CPU_STATE_MAX as usize];
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `host` is the retained host port from `mach_host_self`; `ticks` is a correctly sized
    // and aligned four-word output buffer matching host_cpu_load_info_data_t::cpu_ticks; `count`
    // is a writable count value. The synchronous call writes no more than the supplied capacity.
    let status = unsafe {
        libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            ticks.as_mut_ptr().cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if status != libc::KERN_SUCCESS || count != libc::HOST_CPU_LOAD_INFO_COUNT {
        return None;
    }
    Some(CpuTicks {
        user: u64::from(ticks[libc::CPU_STATE_USER as usize]),
        system: u64::from(ticks[libc::CPU_STATE_SYSTEM as usize]),
        idle: u64::from(ticks[libc::CPU_STATE_IDLE as usize]),
        nice: u64::from(ticks[libc::CPU_STATE_NICE as usize]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_machine_tick_samples_report_aggregate_cpu_tenths() {
        let mut tracker = CpuUsageTracker::default();
        let before = CpuTicks {
            user: 100,
            system: 200,
            nice: 0,
            idle: 700,
        };
        let after = CpuTicks {
            user: 150,
            system: 260,
            nice: 10,
            idle: 720,
        };
        assert_eq!(tracker.sample_tenths(Some(before)), None);
        assert_eq!(tracker.sample_tenths(Some(after)), Some(857));
    }

    #[test]
    fn fake_tick_samples_are_bounded_and_recover_after_counter_reset() {
        let mut tracker = CpuUsageTracker::default();
        assert_eq!(
            tracker.sample_tenths(Some(CpuTicks {
                user: 10,
                system: 10,
                nice: 10,
                idle: 10,
            })),
            None
        );
        assert_eq!(
            tracker.sample_tenths(Some(CpuTicks {
                user: 20,
                system: 20,
                nice: 20,
                idle: 10,
            })),
            Some(1_000)
        );
        assert_eq!(
            tracker.sample_tenths(Some(CpuTicks {
                user: 1,
                system: 1,
                nice: 1,
                idle: 1,
            })),
            None
        );
        assert_eq!(
            tracker.sample_tenths(Some(CpuTicks {
                user: 1,
                system: 1,
                nice: 1,
                idle: 2,
            })),
            Some(0)
        );
    }

    #[test]
    fn missing_fake_sample_preserves_baseline_and_zero_delta_is_unknown() {
        let mut tracker = CpuUsageTracker::default();
        let baseline = CpuTicks {
            user: 3,
            system: 4,
            nice: 5,
            idle: 6,
        };
        assert_eq!(tracker.sample_tenths(Some(baseline)), None);
        assert_eq!(tracker.sample_tenths(None), None);
        assert_eq!(tracker.sample_tenths(Some(baseline)), None);
    }

    #[test]
    fn process_cpu_is_normalized_to_total_machine_capacity() {
        // One process core at 100% on a four-core machine is 25.0% of total capacity.
        assert_eq!(process_utilization_tenths(0, 1_000, 1_000, 4), Some(250));
        assert_eq!(process_utilization_tenths(0, 4_000, 1_000, 4), Some(1_000));
    }

    #[test]
    fn process_cpu_tracker_handles_first_sample_and_rejects_invalid_deltas() {
        let start = Instant::now();
        let mut tracker = ProcessCpuUsageTracker::default();
        assert_eq!(tracker.sample_tenths(Some(10_000), start, Some(2)), None);
        assert_eq!(
            tracker.sample_tenths(
                Some(50_010_000),
                start + std::time::Duration::from_millis(100),
                Some(2)
            ),
            Some(250)
        );
        assert_eq!(
            process_utilization_tenths(10, 9, 100, 1),
            None,
            "process CPU counters must not go backwards"
        );
        assert_eq!(process_utilization_tenths(0, 1, 0, 1), None);
        assert_eq!(process_utilization_tenths(0, 1, 1, 0), None);
    }
}
