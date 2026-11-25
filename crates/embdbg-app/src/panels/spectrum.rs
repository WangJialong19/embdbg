//! The spectrum view: the FFT of one channel's newest samples.

use std::time::{Duration, Instant};

use egui::{ComboBox, RichText, Ui};
use embdbg_core::analysis::{FftAnalyzer, Spectrum as ComputedSpectrum, Window};
use embdbg_core::pipeline::SessionShared;

use crate::plots::area::{AxisStyle, PlotArea, draw_axes, value_label};
use crate::theme;
use crate::util::format_sig;

/// Transform sizes offered; all powers of two.
const SIZES: [usize; 6] = [256, 512, 1024, 2048, 4096, 8192];

/// The view recomputes at most this often, whatever the frame rate.
const REFRESH: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct SpectrumState {
    channel: usize,
    size: usize,
    window: Window,
    /// Show decibels instead of signal units.
    db: bool,
    /// Scale the frequency axis to the strongest peak instead of the whole band.
    auto_x: bool,
    analyzer: FftAnalyzer,
    spectrum: Option<ComputedSpectrum>,
    /// When it was last computed, and from how many frames.
    computed_at: Option<Instant>,
    computed_frames: u64,
    computed_params: (usize, usize, Window),
}

impl Default for SpectrumState {
    fn default() -> Self {
        let (size, window) = (2048, Window::Hann);
        Self {
            channel: 0,
            size,
            window,
            db: false,
            auto_x: true,
            analyzer: FftAnalyzer::new(size, window),
            spectrum: None,
            computed_at: None,
            computed_frames: 0,
            computed_params: (0, size, window),
        }
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, state: &mut SpectrumState) {
    let (channel_count, frames, sample_rate) = {
        let data = shared.channels.read();
        (
            data.channels().len(),
            data.frames(),
            data.axis().frame_rate().unwrap_or(0.0),
        )
    };
    if channel_count == 0 {
        ui.label(
            RichText::new("打开一个输出 JustFloat 或 JustInt 数据的连接后，这里显示它的频谱。")
                .color(theme::FG_DIM),
        );
        return;
    }
    state.channel = state.channel.min(channel_count - 1);

    ui.horizontal(|ui| {
        ComboBox::from_id_salt("fft-channel")
            .selected_text(channel_name(shared, state.channel))
            .show_ui(ui, |ui| {
                for index in 0..channel_count {
                    ui.selectable_value(&mut state.channel, index, channel_name(shared, index));
                }
            });
        ComboBox::from_id_salt("fft-size")
            .selected_text(format!("{} 点", state.size))
            .show_ui(ui, |ui| {
                for size in SIZES {
                    ui.selectable_value(&mut state.size, size, format!("{size} 点"));
                }
            });
        ComboBox::from_id_salt("fft-window")
            .selected_text(state.window.label())
            .show_ui(ui, |ui| {
                for window in [
                    Window::Rectangle,
                    Window::Hann,
                    Window::Hamming,
                    Window::Blackman,
                    Window::FlatTop,
                ] {
                    ui.selectable_value(&mut state.window, window, window.label());
                }
            });
        ui.checkbox(&mut state.db, "dB");
        ui.checkbox(&mut state.auto_x, "自动横轴");
        ui.label(
            RichText::new(format!("采样率 {}", format_sig(sample_rate, 4)))
                .small()
                .color(theme::FG_DIM),
        );
    });

    let params = (state.channel, state.size, state.window);
    let due = state.computed_at.is_none_or(|at| at.elapsed() >= REFRESH);
    if state.spectrum.is_none()
        || state.computed_params != params
        || (due && state.computed_frames != frames)
    {
        if state.analyzer.size() != state.size {
            state.analyzer = FftAnalyzer::new(state.size, state.window);
        }
        state.analyzer.set_window(state.window);
        state.spectrum = {
            let data = shared.channels.read();
            let mut samples = Vec::with_capacity(state.size);
            if let Some(channel) = data.channel(state.channel) {
                channel.latest(state.size, &mut samples);
            }
            if sample_rate > 0.0 && samples.len() >= state.size {
                state.analyzer.analyze(&samples, sample_rate as f32)
            } else {
                None
            }
        };
        state.computed_at = Some(Instant::now());
        state.computed_frames = frames;
        state.computed_params = params;
    }

    let Some(spectrum) = &state.spectrum else {
        ui.label(
            RichText::new("样本还不够做一个窗口：把时间窗拉长，或让数据再流一会儿。")
                .color(theme::FG_DIM),
        );
        return;
    };

    draw(ui, spectrum, state.db, state.auto_x);
    ui.horizontal(|ui| {
        ui.label(RichText::new("峰值").small().color(theme::FG_DIM));
        for peak in spectrum.peaks.iter().take(5) {
            let text = if state.db {
                format!(
                    "{} Hz @ {} dB",
                    format_sig(f64::from(peak.freq), 4),
                    format_sig(f64::from(db(peak.magnitude)), 4)
                )
            } else {
                format!(
                    "{} Hz @ {}",
                    format_sig(f64::from(peak.freq), 4),
                    format_sig(f64::from(peak.magnitude), 4)
                )
            };
            ui.label(RichText::new(text).monospace().small());
        }
    });
}

fn db(magnitude: f32) -> f32 {
    (20.0 * magnitude.max(1e-9).log10()).max(-140.0)
}

fn channel_name(shared: &SessionShared, index: usize) -> String {
    shared.channels.read().channel(index).map_or_else(
        || format!("通道 {}", index + 1),
        |channel| channel.name().to_owned(),
    )
}

fn draw(ui: &mut Ui, spectrum: &ComputedSpectrum, use_db: bool, auto_x: bool) {
    // Leave a line under the plot for the list of peaks.
    let height = (ui.available_height() - 22.0).max(80.0);
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let plot = rect
        .shrink2(egui::vec2(64.0, 12.0))
        .shrink2(egui::vec2(0.0, 14.0));
    if plot.width() < 32.0 || plot.height() < 32.0 {
        return;
    }

    let nyquist = spectrum.sample_rate as f64 * 0.5;
    // A full-band view hides everything but the left edge; follow the strongest peak instead.
    let x_max = if auto_x {
        spectrum.peaks.first().map_or(nyquist, |peak| {
            (f64::from(peak.freq) * 5.0).max(1_000.0).min(nyquist)
        })
    } else {
        nyquist
    };
    let y_of = |magnitude: f32| -> f64 {
        if use_db {
            f64::from(db(magnitude))
        } else {
            f64::from(magnitude)
        }
    };
    let y_max = spectrum
        .magnitudes
        .iter()
        .map(|&magnitude| y_of(magnitude))
        .fold(if use_db { -140.0 } else { 0.0 }, f64::max);
    let y_max = if use_db {
        (y_max / 10.0).ceil() * 10.0
    } else {
        (y_max * 1.1).max(1e-6)
    };

    let columns = plot.width().ceil() as usize;
    let area = PlotArea::new(
        plot,
        (0.0, x_max),
        (if use_db { -140.0 } else { 0.0 }, y_max),
    );
    draw_axes(
        ui.painter(),
        &area,
        &AxisStyle::default(),
        value_label,
        value_label,
    );

    // One point per pixel column: the strongest bin in that column.
    let bins_per_column = (spectrum.magnitudes.len() as f64 / columns as f64).max(1.0);
    let mut points = Vec::with_capacity(columns);
    for column in 0..columns {
        let start = (column as f64 * bins_per_column) as usize;
        let end = (((column + 1) as f64) * bins_per_column) as usize;
        let end = end.min(spectrum.magnitudes.len()).max(start + 1);
        let mut strongest = 0.0f32;
        for bin in start..end.min(spectrum.magnitudes.len()) {
            strongest = strongest.max(spectrum.magnitudes[bin]);
        }
        let freq = column as f64 / columns as f64 * x_max;
        points.push(area.screen_pos(freq, y_of(strongest)));
    }
    let painter = ui.painter().with_clip_rect(area.rect);
    painter.add(egui::Shape::line(
        points,
        egui::epaint::PathStroke::new(1.2, theme::series_color(0)),
    ));
    painter.text(
        egui::pos2(plot.right() - 4.0, plot.top() + 2.0),
        egui::Align2::RIGHT_TOP,
        if use_db { "dB" } else { "幅度" },
        egui::FontId::proportional(11.5),
        theme::FG_DIM,
    );
    painter.text(
        egui::pos2(plot.right() - 4.0, plot.bottom() + 2.0),
        egui::Align2::RIGHT_TOP,
        "Hz",
        egui::FontId::proportional(11.5),
        theme::FG_DIM,
    );
}
