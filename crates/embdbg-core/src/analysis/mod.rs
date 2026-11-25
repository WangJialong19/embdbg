//! Turning samples into what the analysis views show: spectra and histograms.

pub mod fft;
pub mod histogram;

pub use fft::{FftAnalyzer, Peak, Spectrum, Window};
pub use histogram::{Histogram, SampleStats, histogram};
