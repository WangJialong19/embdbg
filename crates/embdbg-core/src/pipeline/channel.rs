//! One channel of samples: a ring buffer plus a min/max pyramid for cheap decimation.

use std::ops::Range;

use super::batch::TimeAxis;

/// Bucket sizes of the min/max pyramid, in samples. Each level aggregates 16 buckets of the level below.
const PYRAMID_BUCKETS: [u64; 3] = [16, 256, 4096];
const LEVELS: usize = PYRAMID_BUCKETS.len();

/// Samples retained per channel by default: 2^20, i.e. 4 MiB of f32 per channel.
pub const DEFAULT_CAPACITY: usize = 1 << 20;
const MIN_CAPACITY: usize = 1 << 10;

/// A point of a decimated waveform: a time and the sample value at that time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecPoint {
    pub t: f64,
    pub v: f32,
}

/// Session-wide statistics of a channel. NaN samples are not counted.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChannelStats {
    pub count: u64,
    pub last: Option<f32>,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub mean: f64,
}

/// One level of the min/max pyramid: per-bucket extremes in a ring that covers the same span as the values.
#[derive(Debug)]
struct Level {
    bucket: u64,
    mask: usize,
    mins: Box<[f32]>,
    maxs: Box<[f32]>,
}

/// How much of every column a decimation reports. The extremes are always there; the first and
/// last sample of a column are what it costs to also keep the line continuous between columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDetail {
    /// The extremes only: two points per column, the cheapest to draw.
    Extremes,
    /// The extremes plus the first and last sample: four points per column.
    ExtremesAndEdges,
}

/// Samples of one channel.
///
/// Samples are addressed by *frame*, an index that counts across the whole session, so a caller
/// never has to care where the ring's physical start currently is.
#[derive(Debug)]
pub struct ChannelRing {
    name: String,
    /// Set once a person chose the name, which stops data sources from replacing it.
    name_is_custom: bool,
    unit: String,
    /// Frame index of the very first sample pushed into this ring.
    base: u64,
    mask: usize,
    values: Box<[f32]>,
    /// Samples pushed so far.
    total: u64,
    levels: Vec<Level>,
    /// Extremes of the bucket currently being filled, per level.
    acc_min: [f32; LEVELS],
    acc_max: [f32; LEVELS],
    stats: ChannelStats,
}

impl ChannelRing {
    /// Creates an empty ring holding at least `capacity` samples, whose first sample will be `base_frame`.
    pub fn new(name: impl Into<String>, capacity: usize, base_frame: u64) -> Self {
        let capacity = normalize_capacity(capacity);
        let mut levels = Vec::with_capacity(LEVELS);
        for &bucket in &PYRAMID_BUCKETS {
            let n = (capacity as u64 / bucket) as usize;
            if n == 0 {
                break;
            }
            levels.push(Level {
                bucket,
                mask: n - 1,
                mins: vec![0.0; n].into_boxed_slice(),
                maxs: vec![0.0; n].into_boxed_slice(),
            });
        }
        Self {
            name: name.into(),
            name_is_custom: false,
            unit: String::new(),
            base: base_frame,
            mask: capacity - 1,
            values: vec![0.0; capacity].into_boxed_slice(),
            total: 0,
            levels,
            acc_min: [f32::INFINITY; LEVELS],
            acc_max: [f32::NEG_INFINITY; LEVELS],
            stats: ChannelStats::default(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Renames the channel on the user's behalf; from now on data sources leave the name alone.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
        self.name_is_custom = true;
    }

    /// Gives the channel the name its data source suggests, unless the user already named it.
    pub fn set_default_name(&mut self, name: impl Into<String>) {
        if !self.name_is_custom {
            self.name = name.into();
        }
    }

    pub fn name_is_custom(&self) -> bool {
        self.name_is_custom
    }

    pub fn unit(&self) -> &str {
        &self.unit
    }

    pub fn set_unit(&mut self, unit: impl Into<String>) {
        self.unit = unit.into();
    }

    pub fn capacity(&self) -> usize {
        self.values.len()
    }

    /// Samples currently retained.
    pub fn len(&self) -> usize {
        (self.total.min(self.values.len() as u64)) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Frame index of the oldest retained sample.
    pub fn oldest(&self) -> u64 {
        self.newest() - self.len() as u64
    }

    /// Frame index just past the newest sample.
    pub fn newest(&self) -> u64 {
        self.base + self.total
    }

    /// Frames of the retained samples.
    pub fn retained(&self) -> Range<u64> {
        self.oldest()..self.newest()
    }

    pub fn stats(&self) -> ChannelStats {
        self.stats
    }

    /// Drops every sample. Frame indices keep counting up, so stale indices stay invalid.
    pub fn clear(&mut self) {
        self.base = self.newest();
        self.total = 0;
        self.acc_min = [f32::INFINITY; LEVELS];
        self.acc_max = [f32::NEG_INFINITY; LEVELS];
        self.stats = ChannelStats::default();
    }

    #[inline]
    pub fn push(&mut self, v: f32) {
        let idx = self.total;
        self.values[(idx as usize) & self.mask] = v;
        self.total += 1;

        if !v.is_nan() {
            let s = &mut self.stats;
            s.count += 1;
            s.last = Some(v);
            s.min = Some(s.min.map_or(v, |m| m.min(v)));
            s.max = Some(s.max.map_or(v, |m| m.max(v)));
            s.mean += (f64::from(v) - s.mean) / s.count as f64;

            for l in 0..self.levels.len() {
                if v < self.acc_min[l] {
                    self.acc_min[l] = v;
                }
                if v > self.acc_max[l] {
                    self.acc_max[l] = v;
                }
            }
        }

        // A NaN sample still completes the bucket it falls in.
        for (l, level) in self.levels.iter_mut().enumerate() {
            if !(idx + 1).is_multiple_of(level.bucket) {
                break;
            }
            let slot = ((idx / level.bucket) as usize) & level.mask;
            level.mins[slot] = self.acc_min[l];
            level.maxs[slot] = self.acc_max[l];
            self.acc_min[l] = f32::INFINITY;
            self.acc_max[l] = f32::NEG_INFINITY;
        }
    }

    #[inline]
    fn raw(&self, frame: u64) -> f32 {
        self.values[((frame - self.base) as usize) & self.mask]
    }

    /// Copies the newest `len` samples into `out`, oldest first; fewer if the ring does not hold
    /// that many. NaN samples are copied as they are, so a caller can decide what to do with gaps.
    pub fn latest(&self, len: usize, out: &mut Vec<f32>) {
        let end = self.newest();
        let start = end.saturating_sub(len as u64).max(self.oldest());
        out.extend((start..end).map(|frame| self.raw(frame)));
    }

    /// Value of the sample at `frame`, if it is still retained.
    pub fn value_at(&self, frame: u64) -> Option<f32> {
        (self.oldest()..self.newest())
            .contains(&frame)
            .then(|| self.raw(frame))
    }

    /// Extremes of the retained samples in `range`, or `None` if they are all NaN.
    pub fn range_minmax(&self, range: Range<u64>) -> Option<(f32, f32)> {
        let start = range.start.max(self.oldest());
        let end = range.end.min(self.newest());
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        if start < end {
            self.visit(start, end, self.levels.len() as isize - 1, &mut lo, &mut hi);
        }
        (lo <= hi).then_some((lo, hi))
    }

    /// Aggregates complete buckets at `level` and descends for the partial ones at both ends.
    fn visit(&self, start: u64, end: u64, level: isize, lo: &mut f32, hi: &mut f32) {
        if start >= end {
            return;
        }
        if level < 0 {
            for frame in start..end {
                let v = self.raw(frame);
                if !v.is_nan() {
                    *lo = lo.min(v);
                    *hi = hi.max(v);
                }
            }
            return;
        }
        let lvl = &self.levels[level as usize];
        let bucket = lvl.bucket;
        let first_full = start.div_ceil(bucket);
        let last_full = end / bucket;
        if first_full < last_full {
            self.visit(start, first_full * bucket, level - 1, lo, hi);
            for k in first_full..last_full {
                let slot = (k as usize) & lvl.mask;
                *lo = lo.min(lvl.mins[slot]);
                *hi = hi.max(lvl.maxs[slot]);
            }
            self.visit(last_full * bucket, end, level - 1, lo, hi);
        } else {
            self.visit(start, end, level - 1, lo, hi);
        }
    }

    /// Appends a decimated view of `range` to `out`, spending a few points per column. The
    /// extremes make spikes survive any zoom level; see [`ColumnDetail`] for the rest.
    ///
    /// Once zoomed in past one sample per column, every sample becomes its own column instead.
    pub fn decimate(
        &self,
        axis: TimeAxis<'_>,
        range: Range<u64>,
        columns: usize,
        detail: ColumnDetail,
        out: &mut Vec<DecPoint>,
    ) {
        let start = range.start.max(self.oldest());
        let end = range.end.min(self.newest());
        if start >= end || columns == 0 {
            return;
        }
        let span = end - start;
        if span <= columns as u64 {
            for frame in start..end {
                let v = self.raw(frame);
                if !v.is_nan() {
                    out.push(DecPoint {
                        t: axis.time_at(frame).unwrap_or(f64::NAN),
                        v,
                    });
                }
            }
            return;
        }

        for c in 0..columns as u64 {
            let c_start = start + c * span / columns as u64;
            let c_end = start + (c + 1) * span / columns as u64;
            let Some((lo, hi)) = self.range_minmax(c_start..c_end) else {
                continue;
            };
            // NaN at a column edge would tear the line apart; fall back to an extreme that exists.
            let first = self.raw(c_start);
            let first = if first.is_nan() { lo } else { first };
            let last = self.raw(c_end - 1);
            let last = if last.is_nan() { hi } else { last };
            // Visit the extreme nearer to `first` first, so the zigzag stays short.
            let (a, b) = if (first - lo).abs() <= (first - hi).abs() {
                (lo, hi)
            } else {
                (hi, lo)
            };
            let t = axis
                .time_at(c_start + (c_end - c_start) / 2)
                .unwrap_or(f64::NAN);

            let points = [
                DecPoint { t, v: first },
                DecPoint { t, v: a },
                DecPoint { t, v: b },
                DecPoint { t, v: last },
            ];
            let kept = match detail {
                ColumnDetail::ExtremesAndEdges => &points[..],
                ColumnDetail::Extremes => &points[1..3],
            };
            out.extend_from_slice(kept);
        }
    }
}

/// Rounds a capacity up to a power of two, with a floor of 1024 samples.
pub(crate) fn normalize_capacity(capacity: usize) -> usize {
    capacity.max(MIN_CAPACITY).next_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::batch::BatchLog;

    /// Deterministic pseudo-random numbers, so failing tests can be reproduced.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 1
        }

        fn next_f32(&mut self) -> f32 {
            ((self.next_u64() >> 39) as f32 / (1u64 << 24) as f32) * 20.0 - 10.0
        }
    }

    fn axis<'a>(log: &'a BatchLog, end_frame: u64) -> TimeAxis<'a> {
        TimeAxis::new(log, end_frame)
    }

    /// Extremes of a ring, computed the naive way.
    fn brute_force_minmax(ring: &ChannelRing) -> (f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for frame in ring.retained() {
            if let Some(v) = ring.value_at(frame).filter(|v| !v.is_nan()) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        (lo, hi)
    }

    #[test]
    fn ring_wraps_and_reports_retained_range() {
        let mut ring = ChannelRing::new("a", 1024, 7);
        assert!(ring.is_empty());
        assert_eq!(ring.retained(), 7..7);
        for i in 0..1034u64 {
            ring.push(i as f32);
        }
        assert_eq!(ring.len(), 1024);
        assert_eq!(ring.retained(), 17..1041);
        assert_eq!(ring.value_at(16), None);
        // Frame 17 held sample 10, which is the oldest one left.
        assert_eq!(ring.value_at(17), Some(10.0));
        assert_eq!(ring.value_at(1040), Some(1033.0));
        assert_eq!(ring.value_at(1041), None);

        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.retained(), 1041..1041);
        assert_eq!(ring.value_at(17), None);
    }

    #[test]
    fn pyramid_agrees_with_scanning() {
        let mut ring = ChannelRing::new("a", 4096, 0);
        let mut rng = Lcg(42);
        for i in 0..5000 {
            // Every 13th frame is NaN.
            ring.push(if i % 13 == 0 {
                f32::NAN
            } else {
                rng.next_f32()
            });
        }
        let retained = ring.retained();
        let count = retained.end - retained.start;
        for _ in 0..2000 {
            let a = retained.start + (rng.next_u64() % count);
            let b = retained.start + (rng.next_u64() % count);
            let range = a.min(b)..a.max(b) + 1;
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for frame in range.clone() {
                if let Some(v) = ring.value_at(frame).filter(|v| !v.is_nan()) {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let expected = (lo <= hi).then_some((lo, hi));
            assert_eq!(
                ring.range_minmax(range.clone()),
                expected,
                "range {range:?}"
            );
        }
        assert_eq!(ring.range_minmax(0..5_000), Some(brute_force_minmax(&ring)));
        // Frame 910 = 70 × 13 is NaN, 911 is not.
        assert_eq!(ring.range_minmax(910..911), None);
        assert!(ring.range_minmax(911..912).is_some());
    }

    #[test]
    fn decimate_keeps_extremes_and_bounds_the_point_count() {
        let log = BatchLog::with_capacity(16);
        let mut ring = ChannelRing::new("a", 4096, 0);
        let mut rng = Lcg(7);
        let mut source = Vec::new();
        for _ in 0..3000 {
            let v = rng.next_f32();
            source.push(v);
            ring.push(v);
        }
        let end = 3000u64;
        let out = &mut Vec::new();
        ring.decimate(
            axis(&log, end),
            0..end,
            200,
            ColumnDetail::ExtremesAndEdges,
            out,
        );
        assert!(out.len() <= 4 * 200 && out.len() > 100);

        let got_min = out.iter().map(|p| p.v).fold(f32::INFINITY, f32::min);
        let got_max = out.iter().map(|p| p.v).fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(
            got_min,
            source.iter().copied().fold(f32::INFINITY, f32::min)
        );
        assert_eq!(
            got_max,
            source.iter().copied().fold(f32::NEG_INFINITY, f32::max)
        );
    }

    #[test]
    fn extremes_only_halve_the_points_and_keep_the_envelope() {
        let log = BatchLog::with_capacity(16);
        let mut ring = ChannelRing::new("a", 4096, 0);
        let mut rng = Lcg(99);
        for _ in 0..3000 {
            ring.push(rng.next_f32());
        }
        let full = &mut Vec::new();
        ring.decimate(
            axis(&log, 3000),
            0..3000,
            200,
            ColumnDetail::ExtremesAndEdges,
            full,
        );
        let lean = &mut Vec::new();
        ring.decimate(axis(&log, 3000), 0..3000, 200, ColumnDetail::Extremes, lean);
        assert_eq!(full.len(), 800);
        assert_eq!(lean.len(), 400);
        // The lean form is the middle two points of every column of the full form.
        // (Times are NaN here, the log is empty, so only values are compared.)
        assert!(
            lean.iter()
                .enumerate()
                .all(|(i, p)| p.v == full[(i / 2) * 4 + 1 + i % 2].v)
        );
    }

    #[test]
    fn decimate_zoomed_in_emits_every_sample() {
        // Frames 0..5 arrived at t = 0, frames 5..10 at t = 1.
        let mut log = BatchLog::with_capacity(16);
        log.push(0.0, 0);
        log.push(1.0, 5);
        let mut ring = ChannelRing::new("a", 1024, 0);
        for i in 0..10 {
            ring.push(i as f32);
        }
        let out = &mut Vec::new();
        ring.decimate(
            axis(&log, 10),
            0..10,
            500,
            ColumnDetail::ExtremesAndEdges,
            out,
        );
        assert_eq!(out.len(), 10);
        assert_eq!(out[0].v, 0.0);
        assert_eq!(out[9].v, 9.0);
        // The first batch has no predecessor, so it is spaced like the second: 0.2 s per frame back from t = 0.
        assert!((out[0].t - (-0.8)).abs() < 1e-12);
        assert_eq!(out[9].t, 1.0);
    }

    #[test]
    fn nan_samples_are_skipped() {
        let log = BatchLog::with_capacity(16);
        let mut ring = ChannelRing::new("a", 1024, 0);
        for i in 0..64 {
            ring.push(if (4..40).contains(&i) {
                f32::NAN
            } else {
                i as f32
            });
        }
        let out = &mut Vec::new();
        ring.decimate(
            axis(&log, 64),
            0..64,
            8,
            ColumnDetail::ExtremesAndEdges,
            out,
        );
        // Eight samples per column: columns 0, 5, 6 and 7 hold numbers, the four in between are NaN only.
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|p| p.v < 4.0 || p.v >= 40.0));

        let stats = ring.stats();
        assert_eq!(stats.count, 28);
        assert_eq!(stats.min, Some(0.0));
        assert_eq!(stats.max, Some(63.0));
        assert_eq!(stats.last, Some(63.0));
        // (0 + 1 + 2 + 3) + (40 + … + 63) = 1242, over 28 samples.
        assert!((stats.mean - 1242.0 / 28.0).abs() < 1e-3);
    }

    #[test]
    fn capacity_is_normalized() {
        assert_eq!(ChannelRing::new("a", 1, 0).capacity(), 1024);
        assert_eq!(ChannelRing::new("a", 3000, 0).capacity(), 4096);
        assert_eq!(ChannelRing::new("a", 1 << 20, 0).capacity(), 1 << 20);
    }

    #[test]
    fn decimating_a_million_samples_is_cheap() {
        let log = BatchLog::with_capacity(16);
        let mut ring = ChannelRing::new("a", DEFAULT_CAPACITY, 0);
        let mut rng = Lcg(1);
        for _ in 0..DEFAULT_CAPACITY {
            ring.push(rng.next_f32());
        }
        let end = DEFAULT_CAPACITY as u64;
        let out = &mut Vec::new();
        let started = std::time::Instant::now();
        ring.decimate(
            axis(&log, end),
            0..end,
            1000,
            ColumnDetail::ExtremesAndEdges,
            out,
        );
        let elapsed = started.elapsed();
        assert_eq!(out.len(), 4000);
        // Release builds take well under a millisecond; debug builds are not held to it.
        if cfg!(not(debug_assertions)) {
            assert!(
                elapsed < std::time::Duration::from_millis(2),
                "took {elapsed:?}"
            );
        }
    }
}
