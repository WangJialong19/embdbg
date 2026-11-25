//! Built-in signal generator, so every view can be tried and tested without hardware.

/// Number of channels the simulator produces.
pub const SIM_CHANNELS: usize = 8;

/// Names of the simulated channels, in order.
pub const SIM_CHANNEL_NAMES: [&str; SIM_CHANNELS] = [
    "1 kHz 正弦",
    "50 Hz 正弦 + 噪声",
    "200 Hz 方波",
    "123.4 Hz 三角波",
    "777 Hz 锯齿波",
    "白噪声",
    "3.3 V 电源",
    "调幅波",
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulatorConfig {
    /// Samples per second, per channel.
    pub sample_rate: f64,
    /// Batches per second. Each batch carries `sample_rate / batch_hz` frames.
    pub batch_hz: f64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        // Eight channels at 1 MS/s: the load the UI has to stay smooth under.
        Self {
            sample_rate: 1_000_000.0,
            batch_hz: 50.0,
        }
    }
}

/// Generates frames of `SIM_CHANNELS` samples. Deterministic: two simulators fed the same
/// number of frames produce the same signal.
#[derive(Debug, Clone)]
pub struct Simulator {
    sample_rate: f64,
    /// Frames generated so far.
    n: u64,
    /// State of the noise generator, an xorshift64* sequence.
    noise: u64,
}

impl Simulator {
    pub fn new(config: SimulatorConfig) -> Self {
        Self {
            sample_rate: config.sample_rate,
            n: 0,
            noise: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Frames generated so far.
    pub fn frames(&self) -> u64 {
        self.n
    }

    /// Appends `frames` frames to `out`, channel-major within each frame.
    pub fn generate(&mut self, frames: usize, out: &mut Vec<f32>) {
        let fs = self.sample_rate;
        let (mut n, mut noise) = (self.n, self.noise);
        out.reserve(frames * SIM_CHANNELS);
        for _ in 0..frames {
            // Phase as a fraction of a period, wrapped so the precision never degrades.
            let phase = |freq: f64| ((n as f64 * freq) / fs).fract();
            let sine = |freq: f64| (TAU * phase(freq)).sin() as f32;
            let (r1, r2) = (next_noise(&mut noise), next_noise(&mut noise));

            out.extend([
                sine(1_000.0),
                2.0 * sine(50.0) + 0.05 * r1,
                if phase(200.0) < 0.5 { 1.5 } else { -1.5 },
                4.0 * (phase(123.4) - 0.5).abs() as f32 - 1.0,
                0.8 * (2.0 * phase(777.0) as f32 - 1.0),
                0.5 * r2,
                3.3 + 0.002 * next_noise(&mut noise),
                (1.0 + 0.5 * sine(5.0)) * sine(3_000.0),
            ]);
            n += 1;
        }
        self.n = n;
        self.noise = noise;
    }
}

/// Uniform noise in `[-1, 1)` from an xorshift64* sequence.
#[inline]
fn next_noise(state: &mut u64) -> f32 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    let bits = state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
    (bits as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

const TAU: f64 = std::f64::consts::TAU;

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(config: SimulatorConfig, frames: usize) -> Vec<Vec<f32>> {
        let mut sim = Simulator::new(config);
        let mut flat = Vec::new();
        sim.generate(frames, &mut flat);
        (0..SIM_CHANNELS)
            .map(|ch| (0..frames).map(|i| flat[i * SIM_CHANNELS + ch]).collect())
            .collect()
    }

    #[test]
    fn channels_match_their_names() {
        // 1 kHz sampled at 1 MS/s: exactly 1000 samples per period.
        let ch = collect(SimulatorConfig::default(), 100_000);
        assert_eq!(ch.len(), SIM_CHANNELS);

        // Sine: 1 kHz, peak amplitude 1.
        let (min, max) = extremes(&ch[0]);
        assert!(
            (min + 1.0).abs() < 1e-3 && (max - 1.0).abs() < 1e-3,
            "{min} {max}"
        );
        let peaks = ch[0]
            .windows(3)
            .filter(|w| w[0] < w[1] && w[1] >= w[2] && w[1] > 0.99)
            .count();
        assert_eq!(peaks, 100);

        // Sine plus a little noise.
        assert!(extremes(&ch[1]).0 < -1.9 && extremes(&ch[1]).1 > 1.9);
        // Square wave.
        assert!(ch[2].iter().all(|&v| v == 1.5 || v == -1.5));
        // Triangle and sawtooth stay inside their amplitude.
        assert!(extremes(&ch[3]).0 >= -1.0 && extremes(&ch[3]).1 <= 1.0);
        assert!(extremes(&ch[4]).0 >= -0.8 && extremes(&ch[4]).1 <= 0.8);
        // Noise.
        assert!(extremes(&ch[5]).0 < -0.45 && extremes(&ch[5]).1 > 0.45);
        // Supply rail with a little ripple.
        assert!(ch[6].iter().all(|&v| (v - 3.3).abs() < 0.01));
        // Amplitude modulated: the envelope reaches ±1.5.
        assert!((extremes(&ch[7]).1 - 1.5).abs() < 0.01);
    }

    #[test]
    fn output_is_deterministic_and_appendable() {
        let cfg = SimulatorConfig {
            sample_rate: 1000.0,
            batch_hz: 10.0,
        };
        let whole = collect(cfg, 500);
        let mut sim = Simulator::new(cfg);
        let mut halves = Vec::new();
        sim.generate(200, &mut halves);
        sim.generate(300, &mut halves);
        for ch in 0..SIM_CHANNELS {
            let got: Vec<f32> = (0..500).map(|i| halves[i * SIM_CHANNELS + ch]).collect();
            assert_eq!(got, whole[ch]);
        }
    }

    fn extremes(values: &[f32]) -> (f32, f32) {
        (
            values.iter().copied().fold(f32::INFINITY, f32::min),
            values.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        )
    }
}
