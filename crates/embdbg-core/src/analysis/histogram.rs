//! How the values of a channel are spread: a histogram and the numbers behind it.

/// A summary of one window of samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleStats {
    pub count: usize,
    pub min: f32,
    pub max: f32,
    pub mean: f64,
    /// Standard deviation.
    pub std_dev: f64,
}

/// Bin counts over a value range.
#[derive(Debug, Clone, PartialEq)]
pub struct Histogram {
    /// Left edge of the first bin.
    pub min: f32,
    /// Right edge of the last bin.
    pub max: f32,
    /// Counts, left to right.
    pub bins: Vec<u32>,
    pub stats: SampleStats,
}

impl Histogram {
    /// Width of one bin.
    pub fn bin_width(&self) -> f32 {
        (self.max - self.min) / self.bins.len().max(1) as f32
    }

    /// The highest count, for scaling the bars.
    pub fn tallest(&self) -> u32 {
        self.bins.iter().copied().max().unwrap_or(0)
    }
}

/// Counts `samples` into `bins` equal bins. A `range` of `None` takes the samples' own range,
/// widened a little so the extremes do not sit on the edges.
pub fn histogram(samples: &[f32], bins: usize, range: Option<(f32, f32)>) -> Option<Histogram> {
    let bins = bins.clamp(2, 4096);
    let values: Vec<f32> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return None;
    }

    let (min, max) = match range {
        Some((min, max)) if max > min => (min, max),
        _ => {
            let mut min = values.iter().copied().fold(f32::INFINITY, f32::min);
            let mut max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            if max <= min {
                // A constant signal: give it a range so it lands in the middle.
                min -= 0.5;
                max += 0.5;
            } else {
                let pad = (max - min) * 0.02;
                min -= pad;
                max += pad;
            }
            (min, max)
        }
    };

    let width = (max - min) / bins as f32;
    let mut counts = vec![0u32; bins];
    for value in &values {
        let index = (((value - min) / width) as usize).min(bins - 1);
        counts[index] += 1;
    }

    let count = values.len();
    let mean = values.iter().map(|&v| f64::from(v)).sum::<f64>() / count as f64;
    let variance = values
        .iter()
        .map(|&v| {
            let difference = f64::from(v) - mean;
            difference * difference
        })
        .sum::<f64>()
        / count as f64;

    Some(Histogram {
        min,
        max,
        bins: counts,
        stats: SampleStats {
            count,
            min: values.iter().copied().fold(f32::INFINITY, f32::min),
            max: values.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            mean,
            std_dev: variance.max(0.0).sqrt(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sample_lands_in_exactly_one_bin() {
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32) * 0.001).collect();
        let histogram = histogram(&samples, 10, None).unwrap();
        assert_eq!(histogram.bins.iter().sum::<u32>(), 1000);
        assert_eq!(histogram.stats.count, 1000);
        assert!((histogram.stats.min - 0.0).abs() < 1e-6);
        assert!((histogram.stats.max - 0.999).abs() < 1e-6);
    }

    #[test]
    fn the_bin_counts_are_about_equal_for_a_uniform_signal() {
        let samples: Vec<f32> = (0..10_000).map(|i| (i as f32) / 10_000.0).collect();
        let histogram = histogram(&samples, 10, Some((0.0, 1.0))).unwrap();
        // 1 000 per bin, give or take the boundary sample.
        assert!(histogram.bins.iter().all(|&count| count == 1000));
        assert!((histogram.stats.mean - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_constant_signal_still_gets_a_range() {
        let histogram = histogram(&[2.5; 100], 8, None).unwrap();
        assert!(histogram.max > histogram.min);
        assert_eq!(histogram.bins.iter().sum::<u32>(), 100);
        // The middle bin holds everything.
        assert_eq!(histogram.bins[3] + histogram.bins[4], 100);
        assert!((histogram.stats.std_dev).abs() < 1e-9);
    }

    #[test]
    fn gaps_and_nothing_are_handled() {
        assert!(histogram(&[], 16, None).is_none());
        assert!(histogram(&[f32::NAN, f32::INFINITY], 16, None).is_none());
        let histogram = histogram(&[1.0, f32::NAN, 2.0], 4, Some((1.0, 2.0))).unwrap();
        assert_eq!(histogram.stats.count, 2);
    }

    #[test]
    fn a_sine_is_the_expected_dome() {
        // Values of a sine crowd near the extremes; the ends hold more samples than the middle.
        let samples: Vec<f32> = (0..8192)
            .map(|i| (std::f32::consts::TAU * i as f32 / 8192.0 * 13.0).sin())
            .collect();
        let histogram = histogram(&samples, 16, Some((-1.0, 1.0))).unwrap();
        let middle = histogram.bins[7] + histogram.bins[8];
        let end = histogram.bins[0] + histogram.bins[15];
        assert!(end > middle * 2, "ends {end}, middle {middle}");
        assert!((histogram.stats.mean).abs() < 0.01);
    }
}
