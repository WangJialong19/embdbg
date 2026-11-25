//! Arrival timestamp of every batch of frames, used to map sample indices to time and back.
//!
//! Batch `i` covers frames `[start_i, start_{i+1})`. Its samples are spread evenly over the
//! interval `(t_{i-1}, t_i]`, i.e. up to the moment the batch arrived. Looking backwards keeps
//! timestamps stable: a sample never moves once its batch has arrived, however bursty the source.
//! The one exception is the oldest batch in the log, which has no predecessor: its samples share
//! one instant until a second batch arrives, and are then spaced like that second batch.

/// Timestamps of the most recent batches. Holds at least `capacity` of them, rounded up to a power of two.
#[derive(Debug, Clone)]
pub struct BatchLog {
    mask: usize,
    starts: Box<[u64]>,
    times: Box<[f64]>,
    /// Slot of the oldest retained batch.
    head: usize,
    /// Number of retained batches.
    count: usize,
}

impl BatchLog {
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(16).next_power_of_two();
        Self {
            mask: capacity - 1,
            starts: vec![0; capacity].into_boxed_slice(),
            times: vec![0.0; capacity].into_boxed_slice(),
            head: 0,
            count: 0,
        }
    }

    /// Records that a batch starting at frame `start` arrived at time `t`.
    ///
    /// The batch ends where the next one starts, so it is complete as soon as anything is pushed after it.
    pub fn push(&mut self, t: f64, start: u64) {
        let slot = (self.head + self.count) & self.mask;
        self.starts[slot] = start;
        self.times[slot] = t;
        if self.count == self.starts.len() {
            self.head = (self.head + 1) & self.mask;
        } else {
            self.count += 1;
        }
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.count = 0;
    }

    /// Batches currently retained.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Frame the oldest retained batch starts at.
    pub fn first_frame(&self) -> Option<u64> {
        (!self.is_empty()).then(|| self.batch(0).0)
    }

    /// Time at which the most recent batch arrived.
    pub fn last_time(&self) -> Option<f64> {
        (!self.is_empty()).then(|| self.batch(self.count - 1).1)
    }

    /// Frames per second over the retained history.
    ///
    /// The newest batch is left out: it has an arrival time, but no end yet.
    pub fn frame_rate(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        let (first_start, first_time) = self.batch(0);
        let (last_start, last_time) = self.batch(self.count - 1);
        let frames = last_start.checked_sub(first_start)?;
        let seconds = last_time - first_time;
        (frames > 0 && seconds > 0.0).then(|| frames as f64 / seconds)
    }

    /// `(start frame, arrival time)` of the `i`-th retained batch.
    fn batch(&self, i: usize) -> (u64, f64) {
        let slot = (self.head + i) & self.mask;
        (self.starts[slot], self.times[slot])
    }

    /// Frame just past the batch `i`.
    fn batch_end(&self, i: usize, end_frame: u64) -> u64 {
        if i + 1 < self.count {
            self.batch(i + 1).0
        } else {
            end_frame
        }
    }

    /// Position, among the retained batches, of the one containing `frame`.
    fn batch_of(&self, frame: u64, end_frame: u64) -> Option<usize> {
        if self.is_empty() || frame >= end_frame || frame < self.batch(0).0 {
            return None;
        }
        let mut lo = 0;
        let mut hi = self.count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.batch(mid).0 <= frame {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Some(lo - 1)
    }

    /// Time of the sample at `frame`. `end_frame` is the total number of frames pushed so far,
    /// which delimits the newest batch.
    pub fn time_at(&self, frame: u64, end_frame: u64) -> Option<f64> {
        let i = self.batch_of(frame, end_frame)?;
        let (start, t) = self.batch(i);
        let n = (self.batch_end(i, end_frame) - start).max(1) as f64;
        // Where the frame sits in its batch, counted back from the newest one, which arrived at `t`.
        let behind = n - (frame - start + 1) as f64;
        if i > 0 {
            let step = (t - self.batch(i - 1).1) / n;
            return Some(t - behind * step);
        }
        // The oldest retained batch has no predecessor: it never had one, or the log dropped it.
        // Space it like the batch after it, rather than stacking all its samples on one instant.
        let step = (self.count > 1).then(|| {
            let (next_start, next_t) = self.batch(1);
            (next_t - t) / (self.batch_end(1, end_frame) - next_start).max(1) as f64
        });
        step.map_or(Some(t), |step| Some(t - behind * step))
    }

    /// How many samples had arrived by time `t`. Samples `0..result` are the ones at or before
    /// `t`, so the result is the exclusive end of a frame range, like `end_frame` once `t` is
    /// newer than every sample.
    ///
    /// It is the inverse of [`Self::time_at`]: `time_at(frame) <= t` holds exactly for
    /// `frame < index_at_time(t)`.
    pub fn index_at_time(&self, t: f64, end_frame: u64) -> Option<u64> {
        if self.is_empty() {
            return None;
        }
        let (_, last_time) = self.batch(self.count - 1);
        if t >= last_time {
            return Some(end_frame);
        }

        // First batch that arrived at or after `t`. It exists, because `t < last_time`.
        let mut lo = 0;
        let mut hi = self.count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.batch(mid).1 < t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let (start, t_i) = self.batch(lo);
        let n = self.batch_end(lo, end_frame) - start;

        if lo == 0 {
            // Alone, its samples share one instant; the `t >= last_time` case is already done.
            if self.count == 1 {
                return Some(start);
            }
            // Spaced like the batch after it (see `time_at`).
            let (next_start, next_t) = self.batch(1);
            let step = (next_t - t_i) / (self.batch_end(1, end_frame) - next_start).max(1) as f64;
            // Samples of the batch sit at `t_i - k·step`, so `n - ahead` of them have arrived,
            // where `ahead` is how many steps before `t_i` the time `t` lies.
            let ahead = (t_i - t) / step;
            let ahead = if (ahead - ahead.round()).abs() < 1e-6 {
                ahead.round()
            } else {
                ahead
            };
            return Some(start + (n as f64 - ahead.ceil()).clamp(0.0, n as f64) as u64);
        }

        let (_, t_prev) = self.batch(lo - 1);
        // Samples of the batch sit at `t_prev + k·step`, so `k·n` of them have arrived by `t`.
        let position = (t - t_prev) / (t_i - t_prev) * n as f64;
        // One within rounding error of a whole number counts as that number: a time that is
        // exactly a multiple of the step includes the sample sitting on it.
        let arrived = if (position - position.round()).abs() < 1e-6 {
            position.round()
        } else {
            position.floor()
        };
        Some(start + (arrived as u64).min(n))
    }
}

/// A [`BatchLog`] together with the total frame count, which delimits the newest batch.
#[derive(Debug, Clone, Copy)]
pub struct TimeAxis<'a> {
    log: &'a BatchLog,
    end_frame: u64,
}

impl<'a> TimeAxis<'a> {
    pub fn new(log: &'a BatchLog, end_frame: u64) -> Self {
        Self { log, end_frame }
    }

    /// Total number of frames pushed so far.
    pub fn end_frame(&self) -> u64 {
        self.end_frame
    }

    pub fn first_frame(&self) -> Option<u64> {
        self.log.first_frame()
    }

    pub fn last_time(&self) -> Option<f64> {
        self.log.last_time()
    }

    pub fn frame_rate(&self) -> Option<f64> {
        self.log.frame_rate()
    }

    pub fn time_at(&self, frame: u64) -> Option<f64> {
        self.log.time_at(frame, self.end_frame)
    }

    pub fn index_at_time(&self, t: f64) -> Option<u64> {
        self.log.index_at_time(t, self.end_frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Log built from `(arrival time, batch length)` pairs, plus the resulting total frame count.
    fn log(batches: &[(f64, u64)]) -> (BatchLog, u64) {
        let mut log = BatchLog::with_capacity(8);
        let mut start = 0;
        for &(t, n) in batches {
            log.push(t, start);
            start += n;
        }
        (log, start)
    }

    #[test]
    fn samples_lead_up_to_their_arrival_time() {
        // One batch so far: nothing to space it by, so its samples share its arrival time.
        let (single, end) = log(&[(1.0, 4)]);
        for f in 0..4 {
            assert_eq!(single.time_at(f, end), Some(1.0));
        }

        // Two batches: the second is spread over (1.0, 2.0], the first back from 1.0 at that pace.
        let (log, end) = log(&[(1.0, 4), (2.0, 4)]);
        let expected = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
        for (f, want) in expected.iter().enumerate() {
            assert!((log.time_at(f as u64, end).unwrap() - want).abs() < 1e-12);
        }
        assert_eq!(log.time_at(end, end), None);
    }

    #[test]
    fn time_and_index_round_trip() {
        let (log, end) = log(&[(0.0, 10), (0.1, 10), (0.2, 10), (0.3, 10)]);
        // Before anything arrived.
        assert_eq!(log.index_at_time(-5.0, end), Some(0));
        // All ten samples of the first batch arrived by t = 0, the last exactly then.
        assert_eq!(log.index_at_time(0.0, end), Some(10));
        // The batch covering (0.2, 0.3] holds frames 30..40; 0.25 s is its fifth sample, frame 34.
        assert_eq!(log.index_at_time(0.25, end), Some(35));
        // At or past the last arrival, everything has arrived.
        assert_eq!(log.index_at_time(0.3, end), Some(end));
        assert_eq!(log.index_at_time(9.9, end), Some(end));
        assert!((log.frame_rate().unwrap() - 100.0).abs() < 1e-9);

        // The inverse of `time_at`: every frame maps back to just past itself.
        for frame in 0..end {
            let t = log.time_at(frame, end).unwrap();
            assert_eq!(
                log.index_at_time(t, end),
                Some(frame + 1),
                "frame {frame} at t = {t}"
            );
        }
    }

    #[test]
    fn timestamps_stay_monotonic() {
        let (log, end) = log(&[(0.0, 3), (0.5, 1), (0.6, 7), (2.0, 2)]);
        let times: Vec<f64> = (0..end).map(|f| log.time_at(f, end).unwrap()).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    }

    #[test]
    fn old_batches_are_dropped_but_lookup_still_works() {
        let mut log = BatchLog::with_capacity(4);
        for i in 0..20u64 {
            log.push(i as f64, i * 5);
        }
        // The log never holds fewer than 16 batches, so the first four were dropped.
        let end = 100;
        assert_eq!(log.len(), 16);
        assert_eq!(log.first_frame(), Some(20));
        assert_eq!(log.time_at(19, end), None);
        // Frame 85 is the first sample of the batch spread over (16, 17].
        assert!((log.time_at(85, end).unwrap() - 16.2).abs() < 1e-12);
        assert_eq!(log.index_at_time(18.5, end), Some(97));
    }

    #[test]
    fn frame_rate_needs_two_timed_batches() {
        assert_eq!(log(&[(0.0, 4)]).0.frame_rate(), None);
        // 30 frames between the first and the last batch, 0.3 s apart.
        let rate = log(&[(0.0, 10), (0.1, 10), (0.2, 10), (0.3, 10)])
            .0
            .frame_rate()
            .unwrap();
        assert!((rate - 100.0).abs() < 1e-9);
    }
}
