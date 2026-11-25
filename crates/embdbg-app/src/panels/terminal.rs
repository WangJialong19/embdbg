//! The terminal: the text a connection carried, both directions.

use egui::{Label, RichText, ScrollArea, Ui};
use embdbg_core::pipeline::{Direction, SessionShared};

use crate::theme;
use crate::util::format_clock;

/// Height of one terminal row, shared with the virtualized list.
const ROW_HEIGHT: f32 = 18.0;

#[derive(Debug)]
pub struct TerminalState {
    /// Keep the newest line in view.
    pub follow: bool,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self { follow: true }
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, state: &mut TerminalState) {
    let lines = shared.lines.read();

    ui.horizontal(|ui| {
        ui.checkbox(&mut state.follow, "跟随最新");
        ui.label(
            RichText::new(format!("{} 行", lines.len()))
                .small()
                .color(theme::FG_DIM),
        );
        if lines.is_empty() {
            ui.label(
                RichText::new(
                    "（这个连接还没有文本；协议选「文本行」时，收到的每一行都会出现在这里）",
                )
                .small()
                .color(theme::FG_DIM),
            );
        }
    });

    // Only the rows in view are laid out: a long session must not slow the frame down.
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(state.follow)
        .show_rows(ui, ROW_HEIGHT, lines.len(), |ui, range| {
            for index in range {
                let Some(line) = lines.get(index) else {
                    continue;
                };
                let color = match line.direction {
                    Direction::RemoteToHere => theme::FG,
                    Direction::HereToRemote => theme::ACCENT,
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format_clock(line.t))
                            .monospace()
                            .small()
                            .color(theme::FG_DIM),
                    );
                    ui.label(RichText::new(line.direction.label()).small().color(color));
                    ui.add(
                        Label::new(RichText::new(&line.text).monospace().color(color)).truncate(),
                    );
                });
            }
        });
}
