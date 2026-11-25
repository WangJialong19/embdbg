//! The histogram view: how the values of one channel are spread.

use std::time::{Duration, Instant};

use egui::{ComboBox, RichText, Ui};
use embdbg_core::analysis::{Histogram, histogram};
use embdbg_core::pipeline::SessionShared;

use crate::plots::area::{AxisStyle, PlotArea, draw_axes, value_label};
use crate::theme;
use crate::util::format_sig;

/// How many samples one histogram looks at.
const SAMPLE_COUNT: usize = 16_384;
/// Bin counts offered.
const BIN_CHOICES: [usize; 5] = [32, 64, 128, 256, 512];
const REFRESH: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub struct HistogramState {
    channel: usize,
    bins: usize,
    log_y: bool,
    computed: Option<Histogram>,
    computed_at: Option<Instant>,
    computed_frames: u64,
    computed_params: (usize, usize),
}

impl Default for HistogramState {
    fn default() -> Self {
        Self {
            channel: 0,
            bins: 64,
            log_y: false,
            computed: None,
            computed_at: None,
            computed_frames: 0,
            computed_params: (0, 64),
        }
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, state: &mut HistogramState) {
    let (channel_count, frames) = {
        let data = shared.channels.read();
        (data.channels().len(), data.frames())
    };
    if channel_count == 0 {
        ui.label(
            RichText::new("打开一个输出数据的连接后，这里显示它的直方图。").color(theme::FG_DIM),
        );
        return;
    }
    state.channel = state.channel.min(channel_count - 1);

    ui.horizontal(|ui| {
        ComboBox::from_id_salt("hist-channel")
            .selected_text(channel_name(shared, state.channel))
            .show_ui(ui, |ui| {
                for index in 0..channel_count {
                    ui.selectable_value(&mut state.channel, index, channel_name(shared, index));
                }
            });
        ComboBox::from_id_salt("hist-bins")
            .selected_text(format!("{} 个区间", state.bins))
            .show_ui(ui, |ui| {
                for bins in BIN_CHOICES {
                    ui.selectable_value(&mut state.bins, bins, format!("{bins} 个区间"));
                }
            });
        ui.checkbox(&mut state.log_y, "对数纵轴");
        ui.label(
            RichText::new(format!("统计最近 {SAMPLE_COUNT} 个样本"))
                .small()
                .color(theme::FG_DIM),
        );
    });

    let params = (state.channel, state.bins);
    let due = state.computed_at.is_none_or(|at| at.elapsed() >= REFRESH);
    if state.computed.is_none()
        || state.computed_params != params
        || (due && state.computed_frames != frames)
    {
        state.computed = {
            let data = shared.channels.read();
            let mut samples = Vec::with_capacity(SAMPLE_COUNT);
            if let Some(channel) = data.channel(state.channel) {
                channel.latest(SAMPLE_COUNT, &mut samples);
            }
            histogram(&samples, state.bins, None)
        };
        state.computed_at = Some(Instant::now());
        state.computed_frames = frames;
        state.computed_params = params;
    }

    let Some(computed) = &state.computed else {
        ui.label(RichText::new("还没有数据").color(theme::FG_DIM));
        return;
    };

    draw(ui, computed, state.log_y);

    let stats = computed.stats;
    ui.horizontal_wrapped(|ui| {
        let entries = [
            format!("样本 {}", stats.count),
            format!("均值 {}", format_sig(stats.mean, 4)),
            format!("标准差 {}", format_sig(stats.std_dev, 4)),
            format!("最小 {}", format_sig(f64::from(stats.min), 4)),
            format!("最大 {}", format_sig(f64::from(stats.max), 4)),
            format!("峰峰值 {}", format_sig(f64::from(stats.max - stats.min), 4)),
        ];
        for entry in entries {
            ui.label(
                RichText::new(entry)
                    .monospace()
                    .small()
                    .color(theme::FG_DIM),
            );
        }
    });
}

fn channel_name(shared: &SessionShared, index: usize) -> String {
    shared.channels.read().channel(index).map_or_else(
        || format!("通道 {}", index + 1),
        |channel| channel.name().to_owned(),
    )
}

fn draw(ui: &mut Ui, computed: &Histogram, log_y: bool) {
    let (rect, _response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
    let plot = rect
        .shrink2(egui::vec2(64.0, 12.0))
        .shrink2(egui::vec2(0.0, 14.0));
    if plot.width() < 32.0 || plot.height() < 32.0 {
        return;
    }

    let tallest = computed.tallest().max(1) as f64;
    let top = if log_y {
        tallest.log10().ceil().max(1.0)
    } else {
        tallest
    };
    let y_of = |count: u32| -> f64 {
        if log_y {
            (f64::from(count).max(1.0)).log10()
        } else {
            f64::from(count)
        }
    };

    let area = PlotArea::new(
        plot,
        (f64::from(computed.min), f64::from(computed.max)),
        (0.0, top),
    );
    draw_axes(
        ui.painter(),
        &area,
        &AxisStyle::default(),
        value_label,
        value_label,
    );

    let painter = ui.painter().with_clip_rect(area.rect);
    let width = computed.bin_width() as f64;
    for (index, &count) in computed.bins.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let left = f64::from(computed.min) + index as f64 * width;
        let x0 = area.screen_pos(left, 0.0).x;
        let x1 = area.screen_pos(left + width, 0.0).x;
        let y0 = area.screen_pos(left, 0.0).y;
        let y1 = area.screen_pos(left, y_of(count)).y;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0.min(x1) + 0.5, y1),
                egui::pos2(x0.max(x1) - 0.5, y0),
            ),
            egui::CornerRadius::ZERO,
            theme::series_color(0).gamma_multiply(0.85),
        );
    }
}
