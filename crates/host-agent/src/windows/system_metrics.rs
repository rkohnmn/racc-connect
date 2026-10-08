//! Bounded Windows system metrics used by one-second host telemetry reports.

use ::windows::Win32::Foundation::FILETIME;
use ::windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcess, GetProcessTimes, GetSystemTimes,
    ALL_PROCESSOR_GROUPS,
};
use std::time::Instant;

/// Samples machine-wide and host-process CPU utilization.
#[derive(Default)]
pub struct SystemCpuSampler {
    previous: Option<CpuTimes>,
    previous_process: Option<ProcessCpuSample>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CpuTimes {
    idle: u64,
    kernel: u64,
    user: u64,
}

#[derive(Clone, Copy, Debug)]
struct ProcessCpuSample {
    cpu_100ns: u64,
    wall_time: Instant,
}

impl SystemCpuSampler {
    /// Returns machine-wide CPU usage in tenths of a percent, or `None` until a
    /// valid pair of monotonic samples is available.
    pub fn sample_tenths(&mut self) -> Option<u16> {
        let current = read_times()?;
        let previous = self.previous.replace(current)?;
        let idle = current.idle.checked_sub(previous.idle)?;
        let kernel = current.kernel.checked_sub(previous.kernel)?;
        let user = current.user.checked_sub(previous.user)?;
        let total = u128::from(kernel).saturating_add(u128::from(user));
        if total == 0 || u128::from(idle) > total {
            return None;
        }
        let busy = total.saturating_sub(u128::from(idle));
        u16::try_from(busy.saturating_mul(1000).saturating_div(total).min(1000)).ok()
    }

    /// Returns this host process's share of total machine CPU capacity in
    /// tenths of a percent, or `None` until two valid samples are available.
    pub fn sample_process_tenths(&mut self) -> Option<u16> {
        let current = read_process_times()?;
        let previous = self.previous_process.replace(current)?;
        let cpu_delta = current.cpu_100ns.checked_sub(previous.cpu_100ns)?;
        let wall_delta = current
            .wall_time
            .duration_since(previous.wall_time)
            .as_nanos()
            / 100;
        let wall_delta = u64::try_from(wall_delta).ok()?;
        let processor_count = processor_count()?;
        process_cpu_pct_x10(cpu_delta, wall_delta, processor_count)
    }
}

/// Converts process CPU time to tenths of a percent of total machine capacity.
///
/// Windows process times and the wall interval use 100 ns units. Values above
/// 100% of the whole machine are clamped because they can only result from
/// counter skew or an invalid processor count.
fn process_cpu_pct_x10(
    cpu_delta_100ns: u64,
    wall_delta_100ns: u64,
    processors: u32,
) -> Option<u16> {
    if wall_delta_100ns == 0 || processors == 0 {
        return None;
    }
    let capacity = u128::from(wall_delta_100ns).saturating_mul(u128::from(processors));
    let pct_x10 = u128::from(cpu_delta_100ns)
        .saturating_mul(1000)
        .saturating_div(capacity)
        .min(1000);
    u16::try_from(pct_x10).ok()
}

fn read_times() -> Option<CpuTimes> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: All three pointers are writable FILETIME values and remain live for
    // the synchronous GetSystemTimes call.
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
    }
    Some(CpuTimes {
        idle: filetime_value(idle),
        kernel: filetime_value(kernel),
        user: filetime_value(user),
    })
}

fn read_process_times() -> Option<ProcessCpuSample> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle for this process. The
    // FILETIME pointers refer to writable values that remain live for the call.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
        .ok()?;
    }
    Some(ProcessCpuSample {
        cpu_100ns: filetime_value(kernel).saturating_add(filetime_value(user)),
        wall_time: Instant::now(),
    })
}

fn processor_count() -> Option<u32> {
    // SAFETY: This query has no pointer arguments or process-lifetime
    // requirements; ALL_PROCESSOR_GROUPS requests the machine-wide count.
    let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    (count > 0).then_some(count)
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_cpu_delta_is_bounded_to_zero_through_one_hundred_percent() {
        let before = CpuTimes {
            idle: 100,
            kernel: 200,
            user: 100,
        };
        let after = CpuTimes {
            idle: 150,
            kernel: 350,
            user: 200,
        };
        let idle_delta = after.idle - before.idle;
        let total_delta = (after.kernel - before.kernel) + (after.user - before.user);
        let busy = total_delta - idle_delta;
        let pct_x10 = u16::try_from((u128::from(busy) * 1000) / u128::from(total_delta))
            .expect("bounded percentage");
        assert_eq!(pct_x10, 800);
    }

    #[test]
    fn filetime_is_combined_as_little_endian_halves() {
        assert_eq!(
            filetime_value(FILETIME {
                dwLowDateTime: 5,
                dwHighDateTime: 2
            }),
            (2_u64 << 32) | 5
        );
    }

    #[test]
    fn process_cpu_is_normalized_to_machine_capacity() {
        // One core fully busy on a four-core machine is 25% of total capacity.
        assert_eq!(process_cpu_pct_x10(1_000, 1_000, 4), Some(250));
        // Two cores fully busy on a four-core machine is 50% of total capacity.
        assert_eq!(process_cpu_pct_x10(2_000, 1_000, 4), Some(500));
    }

    #[test]
    fn process_cpu_rejects_empty_intervals_and_clamps_to_machine_capacity() {
        assert_eq!(process_cpu_pct_x10(1, 0, 4), None);
        assert_eq!(process_cpu_pct_x10(1, 1, 0), None);
        assert_eq!(process_cpu_pct_x10(8_000, 1_000, 4), Some(1000));
    }
}
