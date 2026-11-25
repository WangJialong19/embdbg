//! The AI activity panel: what MCP clients asked the engine to do.

use std::collections::VecDeque;

use egui::{RichText, ScrollArea, Ui};
use embdbg_core::mcp::Activity;

use crate::theme;

/// How many calls the panel keeps.
const KEEP: usize = 200;

#[derive(Debug, Default)]
pub struct AiState {
    entries: VecDeque<Activity>,
}

impl AiState {
    pub fn push(&mut self, activity: Activity) {
        self.entries.push_back(activity);
        while self.entries.len() > KEEP {
            self.entries.pop_front();
        }
    }
}

pub fn ui(ui: &mut Ui, state: &mut AiState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("AI 活动").strong());
        ui.label(
            RichText::new("MCP 端点 embdbg-mcp")
                .small()
                .color(theme::FG_DIM),
        );
    });
    if state.entries.is_empty() {
        ui.label(
            RichText::new(
                "还没有 AI 调用。在 AI 客户端里配置 `embdbg mcp` 后，它调用的每个工具都会显示在这里。",
            )
            .small()
            .color(theme::FG_DIM),
        );
        return;
    }

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for entry in &state.entries {
                ui.horizontal(|ui| {
                    let time = chrono::DateTime::<chrono::Local>::from(entry.at)
                        .format("%H:%M:%S")
                        .to_string();
                    ui.label(RichText::new(time).monospace().small().color(theme::FG_DIM));
                    ui.label(
                        RichText::new(if entry.ok { "✓" } else { "✗" })
                            .monospace()
                            .color(if entry.ok {
                                theme::SUCCESS
                            } else {
                                theme::DANGER
                            }),
                    );
                    ui.label(RichText::new(&entry.tool).monospace().color(theme::FG));
                    ui.add(
                        egui::Label::new(
                            RichText::new(entry.summary.replace(['\r', '\n'], " "))
                                .small()
                                .color(theme::FG_DIM),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&entry.summary);
                });
            }
        });
}
