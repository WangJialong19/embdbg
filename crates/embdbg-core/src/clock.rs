//! Monotonic clock shared by the engine.

use std::time::Instant;

/// Monotonic clock. Every timestamp in the pipeline is in seconds since the
/// clock was created, so storing it as `f64` keeps full precision for years.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    origin: Instant,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    /// Seconds elapsed since the clock was created.
    #[inline]
    pub fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn now_is_monotonic() {
        let clock = Clock::new();
        let a = clock.now();
        thread::sleep(Duration::from_millis(5));
        let b = clock.now();
        assert!(a >= 0.0);
        assert!(b - a >= 0.005);
    }
}
