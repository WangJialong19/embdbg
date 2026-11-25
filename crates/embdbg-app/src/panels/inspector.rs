//! The channel table: what each channel is called, whether it is drawn, and how its samples look.

use egui::{Button, Color32, Layout, RichText, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use embdbg_core::pipeline::{ChannelStats, SessionShared};

use crate::fonts::icon_label;
use crate::icons::outline;
use crate::theme::{self, FG_DIM, SPACE_1, SPACE_2};
use crate::util::format_sig;

/// Per-channel choices the user makes in this panel, indexed by channel.
#[derive(Debug, Default)]
pub struct ChannelUi {
    /// Whether the plots draw the channel. Shorter than the channel list until it is resized.
    visible: Vec<bool>,
}

impl ChannelUi {
    /// One visibility flag per channel, defaulting to visible.
    pub fn resize(&mut self, channels: usize) -> &mut [bool] {
        self.visible.resize(channels, true);
        &mut self.visible
    }

    /// The visibility flags, padded to `channels` entries for channels not seen before.
    pub fn visible(&mut self, channels: usize) -> &[bool] {
        self.resize(channels)
    }
}

/// Width of the name column. Longer names scroll inside their field instead of widening the table.
const NAME_WIDTH: f32 = 92.0;
/// Width of a value column: enough for the longest number shown, on one line. Content that still
/// does not fit is clipped, never allowed to spill into the neighbouring column.
const VALUE_WIDTH: f32 = 72.0;
/// Height of one table row.
const ROW_HEIGHT: f32 = 20.0;

/// What the table shows about one channel, copied out of the engine so the lock is not held while drawing.
struct Row {
    name: String,
    color: Color32,
    stats: ChannelStats,
    /// Largest minus smallest value seen, which sets how fine the numbers are read.
    span: f64,
}

/// A statistic of a channel, in three significant digits so it fits one column. Anything
/// within a millionth of the channel's span of zero reads as zero: the mean of a symmetric
/// signal is not usefully `-3e-18`.
fn format_stat(value: f64, span: f64) -> String {
    if value.abs() <= span * 1e-6 {
        "0".to_owned()
    } else {
        format_sig(value, 3)
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, channel_ui: &mut ChannelUi) {
    ui.add_space(SPACE_2);
    ui.label(RichText::new("通道").strong());

    let rows: Vec<Row> = {
        let data = shared.channels.read();
        data.channels()
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let stats = channel.stats();
                Row {
                    name: channel.name().to_owned(),
                    color: theme::series_color(index),
                    span: match (stats.min, stats.max) {
                        (Some(min), Some(max)) => f64::from(max - min),
                        _ => 0.0,
                    },
                    stats,
                }
            })
            .collect()
    };
    let visible = channel_ui.resize(rows.len());

    if rows.is_empty() {
        ui.label(
            RichText::new("还没有通道。数据到达后，每个通道占一行。")
                .small()
                .color(FG_DIM),
        );
        return;
    }

    ui.add_space(SPACE_1);
    let mut renamed: Option<(usize, String)> = None;
    TableBuilder::new(ui)
        .striped(true)
        .vscroll(true)
        .id_salt("channel-table")
        .cell_layout(Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(22.0))
        .column(Column::exact(NAME_WIDTH + 8.0))
        .columns(Column::exact(VALUE_WIDTH).clip(true), 4)
        .header(ROW_HEIGHT, |mut header| {
            header.col(|_| {});
            header.col(|ui| {
                ui.label(RichText::new("名称").small().color(FG_DIM));
            });
            for title in ["当前值", "最小", "最大", "均值"] {
                header.col(|ui| {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(title).small().color(FG_DIM));
                    });
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                let index = row.index();
                let data = &rows[index];
                let shown = visible[index];

                row.col(|ui| {
                    ui.checkbox(&mut visible[index], "")
                        .on_hover_text("是否在图上绘制这个通道");
                });
                row.col(|ui| {
                    let mut name = data.name.clone();
                    let response = ui
                        .add(
                            TextEdit::singleline(&mut name)
                                .desired_width(NAME_WIDTH)
                                .frame(egui::Frame::NONE)
                                .text_color(if shown { data.color } else { FG_DIM }),
                        )
                        .on_hover_text("点击后可以重命名通道");
                    if response.lost_focus() && name != data.name {
                        renamed = Some((index, name));
                    }
                });
                for value in [
                    data.stats.last.map(f64::from),
                    data.stats.min.map(f64::from),
                    data.stats.max.map(f64::from),
                    Some(data.stats.mean),
                ] {
                    row.col(|ui| {
                        let text = value
                            .map_or_else(|| "—".to_owned(), |value| format_stat(value, data.span));
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(text).monospace().color(if shown {
                                theme::FG
                            } else {
                                FG_DIM
                            }));
                        });
                    });
                }
            });
        });

    if let Some((index, name)) = renamed
        && let Some(channel) = shared.channels.write().channel_mut(index)
    {
        channel.set_name(name);
    }

    ui.add_space(SPACE_2);
    ui.horizontal(|ui| {
        if ui
            .add(Button::new(icon_label(outline::EYE, "全部显示", 12.5)))
            .clicked()
        {
            visible.fill(true);
        }
        if ui
            .add(Button::new(icon_label(outline::EYE_OFF, "全部隐藏", 12.5)))
            .clicked()
        {
            visible.fill(false);
        }
    });
}
