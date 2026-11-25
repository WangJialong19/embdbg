//! The spectrum of a channel: one window of samples in, magnitudes and peaks out.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// The taper applied before the transform. A plain rectangle leaks badly; the others trade a
/// wider peak for lower leakage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    Rectangle,
    #[default]
    Hann,
    Hamming,
    Blackman,
    FlatTop,
}

impl Window {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "矩形",
            Self::Hann => "汉宁",
            Self::Hamming => "汉明",
            Self::Blackman => "布莱克曼",
            Self::FlatTop => "平顶",
        }
    }

    /// The window's weights, one per sample.
    fn weights(self, len: usize) -> Vec<f32> {
        let n = len.max(1);
        (0..n)
            .map(|i| {
                let x = std::f64::consts::TAU * i as f64 / n as f64;
                match self {
                    Self::Rectangle => 1.0,
                    Self::Hann => 0.5 - 0.5 * x.cos(),
                    Self::Hamming => 0.54 - 0.46 * x.cos(),
                    Self::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
                    // Flat top: a wide main lobe, very flat, which reads amplitudes well.
                    Self::FlatTop => {
                        0.215_578_95 - 0.416_631_58 * x.cos() + 0.277_263_16 * (2.0 * x).cos()
                            - 0.083_578_95 * (3.0 * x).cos()
                            + 0.006_947_37 * (4.0 * x).cos()
                    }
                }
            })
            .map(|weight| weight as f32)
            .collect()
    }

    /// The sum of the weights, which normalizes the magnitudes so they read in signal units.
    fn gain(weights: &[f32]) -> f32 {
        weights.iter().sum::<f32>().max(f32::EPSILON)
    }
}

/// A maximum of the spectrum, with the frequency refined between bins by fitting a parabola.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub freq: f32,
    /// Amplitude in the units of the signal.
    pub magnitude: f32,
}

/// One window's worth of spectrum.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectrum {
    /// Frequency of each bin, in hertz.
    pub freqs: Vec<f32>,
    /// Amplitude of each bin, in signal units (a full-scale sine reads about 1.0).
    pub magnitudes: Vec<f32>,
    /// The strongest local maxima, strongest first.
    pub peaks: Vec<Peak>,
    /// Sample rate this was computed with.
    pub sample_rate: f32,
}

/// Computes spectra of a fixed size, reusing its FFT plan and buffers.
pub struct FftAnalyzer {
    size: usize,
    window: Window,
    weights: Vec<f32>,
    plan: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl std::fmt::Debug for FftAnalyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FftAnalyzer")
            .field("size", &self.size)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl FftAnalyzer {
    /// A power of two between 64 and 65536 reads well on screen; anything in that range is fine.
    pub fn new(size: usize, window: Window) -> Self {
        let size = size.clamp(64, 65_536).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let plan = planner.plan_fft_forward(size);
        Self {
            size,
            window,
            weights: window.weights(size),
            input: plan.make_input_vec(),
            output: plan.make_output_vec(),
            scratch: plan.make_scratch_vec(),
            plan,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn window(&self) -> Window {
        self.window
    }

    /// Changes the window; the next transform uses it.
    pub fn set_window(&mut self, window: Window) {
        if window != self.window {
            self.window = window;
            self.weights = window.weights(self.size);
        }
    }

    /// Analyzes the newest `size` samples. `samples` must hold at least that many, all finite.
    pub fn analyze(&mut self, samples: &[f32], sample_rate: f32) -> Option<Spectrum> {
        if samples.len() < self.size {
            return None;
        }
        let newest = &samples[samples.len() - self.size..];
        if newest.iter().any(|sample| !sample.is_finite()) {
            return None;
        }

        let gain = Window::gain(&self.weights);
        // A full-scale sine puts its amplitude into one bin: 2·|X| / Σw reads back as the sine's
        // amplitude, so the y axis is in signal units.
        let scale = 2.0 / gain;
        for (input, (sample, weight)) in self
            .input
            .iter_mut()
            .zip(newest.iter().zip(self.weights.iter()))
        {
            *input = sample * weight;
        }
        self.plan
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .ok()?;

        let bin_count = self.output.len();
        let bin_width = sample_rate / self.size as f32;
        let freqs: Vec<f32> = (0..bin_count).map(|bin| bin as f32 * bin_width).collect();
        // DC and Nyquist bins are not halved like the others: a constant signal reads back as
        // itself, not as twice itself.
        let last_bin = bin_count.saturating_sub(1);
        let magnitudes: Vec<f32> = self
            .output
            .iter()
            .enumerate()
            .map(|(bin, value)| {
                let gain_here = if bin == 0 || bin == last_bin {
                    1.0 / gain
                } else {
                    scale
                };
                value.norm() * gain_here
            })
            .collect();

        Some(Spectrum {
            freqs,
            peaks: find_peaks(&magnitudes, bin_width),
            magnitudes,
            sample_rate,
        })
    }
}

/// The strongest local maxima, refined between bins.
fn find_peaks(magnitudes: &[f32], bin_width: f32) -> Vec<Peak> {
    const MAX_PEAKS: usize = 8;
    let Some(&strongest) = magnitudes.iter().max_by(|a, b| a.total_cmp(b)) else {
        return Vec::new();
    };
    if strongest <= 0.0 {
        return Vec::new();
    }
    // Anything below a thousandth of the strongest peak is noise for this purpose.
    let floor = strongest * 1e-3;

    let mut peaks: Vec<Peak> = (1..magnitudes.len().saturating_sub(1))
        .filter(|&bin| {
            let value = magnitudes[bin];
            value >= floor && value > magnitudes[bin - 1] && value >= magnitudes[bin + 1]
        })
        .map(|bin| {
            // Fit a parabola through the three bins to place the peak between them.
            let (left, centre, right) = (
                magnitudes[bin - 1].max(1e-15),
                magnitudes[bin].max(1e-15),
                magnitudes[bin + 1].max(1e-15),
            );
            let offset = 0.5 * (left - right) / (left - 2.0 * centre + right);
            let offset = if offset.is_finite() {
                offset.clamp(-0.5, 0.5)
            } else {
                0.0
            };
            Peak {
                freq: (bin as f32 + offset) * bin_width,
                magnitude: centre,
            }
        })
        .collect();
    peaks.sort_by(|a, b| b.magnitude.total_cmp(&a.magnitude));
    peaks.truncate(MAX_PEAKS);
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sample_rate: f32, len: usize, amplitude: f32) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (std::f32::consts::TAU * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    #[test]
    fn a_sine_shows_up_at_its_frequency_with_its_amplitude() {
        let sample_rate = 10_000.0;
        // 1000 Hz is exactly bin 1024 of 10 240? Keep it simple: 8192-point window, 1 250 Hz = bin 1024.
        let mut analyzer = FftAnalyzer::new(8192, Window::Hann);
        let samples = sine(1_250.0, sample_rate, 8192, 2.0);
        let spectrum = analyzer.analyze(&samples, sample_rate).unwrap();

        let peak = spectrum.peaks.first().copied().unwrap();
        assert!((peak.freq - 1_250.0).abs() < 2.0, "peak at {peak:?}");
        assert!(
            (peak.magnitude - 2.0).abs() < 0.05,
            "amplitude {}",
            peak.magnitude
        );
    }

    #[test]
    fn the_window_does_not_change_the_reading_of_a_centred_bin() {
        let sample_rate = 8192.0; // one hertz per bin
        let samples = sine(512.0, sample_rate, 4096, 1.0);
        for window in [
            Window::Rectangle,
            Window::Hann,
            Window::Hamming,
            Window::Blackman,
            Window::FlatTop,
        ] {
            let mut analyzer = FftAnalyzer::new(4096, window);
            let spectrum = analyzer.analyze(&samples, sample_rate).unwrap();
            let peak = spectrum.peaks.first().copied().unwrap();
            assert!(
                (peak.magnitude - 1.0).abs() < 0.02,
                "{window:?}: amplitude {}",
                peak.magnitude
            );
        }
    }

    #[test]
    fn a_dc_signal_lands_in_the_first_bin() {
        let mut analyzer = FftAnalyzer::new(1024, Window::Hann);
        let samples = vec![3.0f32; 1024];
        let spectrum = analyzer.analyze(&samples, 1024.0).unwrap();
        assert!((spectrum.magnitudes[0] - 3.0).abs() < 0.05);
        assert!((spectrum.freqs[0]).abs() < f32::EPSILON);
    }

    #[test]
    fn too_few_samples_or_gaps_give_nothing() {
        let mut analyzer = FftAnalyzer::new(1024, Window::Hann);
        assert!(analyzer.analyze(&[1.0; 100], 1000.0).is_none());
        let mut with_gap = vec![1.0f32; 1024];
        with_gap[500] = f32::NAN;
        assert!(analyzer.analyze(&with_gap, 1000.0).is_none());
    }

    #[test]
    fn sizes_are_clamped_and_rounded_to_powers_of_two() {
        assert_eq!(FftAnalyzer::new(1000, Window::Hann).size(), 1024);
        assert_eq!(FftAnalyzer::new(10, Window::Hann).size(), 64);
        assert_eq!(FftAnalyzer::new(200_000, Window::Hann).size(), 65_536);
    }

    #[test]
    fn peaks_are_refined_between_bins() {
        // A frequency halfway between two bins still reads close to the truth.
        let sample_rate = 1024.0;
        let samples = sine(100.5, sample_rate, 1024, 1.0);
        let mut analyzer = FftAnalyzer::new(1024, Window::Hann);
        let spectrum = analyzer.analyze(&samples, sample_rate).unwrap();
        let peak = spectrum.peaks.first().copied().unwrap();
        assert!((peak.freq - 100.5).abs() < 1.0, "peak at {peak:?}");
    }
}
