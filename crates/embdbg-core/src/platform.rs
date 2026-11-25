//! Small platform adjustments that make timings on this OS trustworthy.

/// Requests a finer system timer resolution for as long as it lives.
///
/// Windows wakes waiting threads only on system timer ticks, 15.6 ms apart by default, and every
/// timed wait in the process inherits that granularity: `sleep(1ms)` really takes about 15.6 ms.
/// That makes on-demand repaint scheduling jitter and turns precise batch rates into coarse ones.
///
/// While this guard lives, the period is 1 ms instead (measured on Windows 11: `sleep(1ms)` then
/// takes 1–3 ms). Dropping it restores the previous resolution.
///
/// On other systems timed waits are already precise, so this is a no-op there.
#[derive(Debug)]
pub struct TimerResolution {
    #[cfg(windows)]
    active: bool,
}

#[cfg(windows)]
#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

/// The period requested on Windows, in milliseconds. 1 ms is the finest value the API accepts.
#[cfg(windows)]
const PERIOD_MS: u32 = 1;

impl TimerResolution {
    /// Requests the finest timer resolution the system offers.
    pub fn finest() -> Self {
        #[cfg(windows)]
        {
            // SAFETY: both functions only adjust a process-wide timer setting.
            let active = unsafe { timeBeginPeriod(PERIOD_MS) } == 0;
            Self { active }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.active {
            // SAFETY: see [`TimerResolution::finest`]. Pairs with its `timeBeginPeriod`.
            unsafe { timeEndPeriod(PERIOD_MS) };
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn fine_resolution_makes_short_sleeps_short() {
        fn median_sleep() -> f64 {
            let mut gaps: Vec<f64> = (0..15)
                .map(|_| {
                    let start = Instant::now();
                    std::thread::park_timeout(Duration::from_millis(1));
                    start.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            gaps[gaps.len() / 2]
        }

        let _fine = TimerResolution::finest();
        // About 2 ms in practice; the default would be about 15.6 ms.
        assert!(median_sleep() < 8.0, "park_timeout(1ms) is still coarse");
    }
}
