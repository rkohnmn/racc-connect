//! Bounded machine-wide CPU telemetry for the macOS host.

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

/// Samples aggregate machine-wide CPU usage in tenths of a percent.
///
/// The sampler retains one Mach host-port right and reads four aggregate CPU tick counters once
/// per StatsReport interval. It does not inspect per-process CPU usage.
#[cfg(target_os = "macos")]
pub struct SystemCpuSampler {
    host: libc::host_t,
    tracker: CpuUsageTracker,
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
        Self { host, tracker }
    }

    /// Returns machine-wide CPU usage, or `None` when the kernel sample or tick delta is invalid.
    pub fn sample_tenths(&mut self) -> Option<u16> {
        self.tracker.sample_tenths(read_cpu_ticks(self.host))
    }
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
}
