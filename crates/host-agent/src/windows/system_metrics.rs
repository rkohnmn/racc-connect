//! Bounded Windows system metrics used by one-second host telemetry reports.

use ::windows::Win32::Foundation::FILETIME;
use ::windows::Win32::System::Threading::GetSystemTimes;

/// Samples machine-wide CPU utilization without collecting per-process data.
#[derive(Default)]
pub struct SystemCpuSampler {
    previous: Option<CpuTimes>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CpuTimes {
    idle: u64,
    kernel: u64,
    user: u64,
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
}
