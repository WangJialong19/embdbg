//! The right-hand tool stack: terminals for every connection, and closable tools below them.
//!
//! Tools are opened from the top menu and can be closed from their own header. The stack itself
//! is a dock tab, so the divider between the main views and the tools stays draggable.

use egui::{RichText, ScrollArea, Ui, vec2};
use embdbg_core::engine::Engine;

use crate::panels::ai::AiState;
use crate::panels::hex::HexState;
use crate::panels::inspector::ChannelUi;
use crate::panels::log::LogBuffer;
use crate::panels::net_tools::NetToolsState;
use crate::panels::send::SendState;
use crate::panels::terminal::TerminalState;
use crate::theme;

/// Tools that can be opened below the terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StackTool {
    Channels,
    Hex,
    Send,
    Log,
    Ai,
    Network,
}

impl StackTool {
    pub const ALL: [Self; 6] = [
        Self::Channels,
        Self::Hex,
        Self::Send,
        Self::Log,
        Self::Ai,
        Self::Network,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Channels => "channels",
            Self::Hex => "hex",
            Self::Send => "send",
            Self::Log => "log",
            Self::Ai => "ai",
            Self::Network => "network",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Channels => "通道",
            Self::Hex => "十六进制",
            Self::Send => "发送",
            Self::Log => "日志",
            Self::Ai => "AI 活动",
            Self::Network => "网络工具",
        }
    }
}

/// Which tools are open, and how much of the stack the terminals take.
#[derive(Debug)]
pub struct StackState {
    open: Vec<StackTool>,
    /// Share of the stack height the terminals keep; the tools scroll in the rest.
    terminal_share: f32,
    /// Set when the open set changed, so the app can save it.
    changed: bool,
}

impl Default for StackState {
    fn default() -> Self {
        Self {
            open: vec![StackTool::Channels],
            terminal_share: 0.55,
            changed: false,
        }
    }
}

impl StackState {
    /// Restores the tools that were open last time.
    pub fn from_settings(ids: &[String]) -> Self {
        let mut open: Vec<StackTool> = ids.iter().filter_map(|id| StackTool::parse(id)).collect();
        open.sort();
        open.dedup();
        Self {
            open,
            ..Self::default()
        }
    }

    /// The open tools as ids, for the settings file.
    pub fn ids(&self) -> Vec<String> {
        self.open.iter().map(|tool| tool.id().to_owned()).collect()
    }

    /// Whether a tool was opened or closed since the last save; the flag is cleared.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn is_open(&self, tool: StackTool) -> bool {
        self.open.contains(&tool)
    }

    pub fn set_open(&mut self, tool: StackTool, open: bool) {
        if open {
            if !self.is_open(tool) {
                self.open.push(tool);
                self.open.sort();
            }
        } else {
            self.open.retain(|candidate| *candidate != tool);
        }
        self.changed = true;
    }

    fn any_tool_open(&self) -> bool {
        !self.open.is_empty()
    }
}

/// The panel states the stack draws.
pub struct StackPanels<'a> {
    pub channels: &'a mut ChannelUi,
    pub terminal: &'a mut TerminalState,
    pub hex: &'a mut HexState,
    pub send: &'a mut SendState,
    pub log: &'a mut LogBuffer,
    pub ai: &'a mut AiState,
    pub net: &'a mut NetToolsState,
}

pub fn ui(ui: &mut Ui, engine: &Engine, stack: &mut StackState, panels: StackPanels<'_>) {
    let total = ui.available_height().max(120.0);
    let tools_height = if stack.any_tool_open() {
        (total * (1.0 - stack.terminal_share)).max(120.0)
    } else {
        0.0
    };
    let terminal_height = (total - tools_height - ui.spacing().item_spacing.y).max(160.0);

    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), terminal_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            if !crate::panels::terminal::columns(ui, engine, panels.terminal) {
                ui.label(
                    RichText::new("打开一个连接后，这里会出现它的终端。").color(theme::FG_DIM),
                );
            }
        },
    );

    if !stack.any_tool_open() {
        return;
    }
    ui.separator();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for tool in StackTool::ALL {
                if !stack.is_open(tool) {
                    continue;
                }
                let mut close = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tool.title()).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("关闭这个工具").clicked() {
                            close = true;
                        }
                    });
                });
                let height = match tool {
                    StackTool::Network => 280.0,
                    _ => 200.0,
                };
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let active = engine.active();
                        let shared = engine.active_shared();
                        match tool {
                            StackTool::Channels => {
                                crate::panels::inspector::ui(ui, &shared, panels.channels);
                            }
                            StackTool::Hex => {
                                crate::panels::hex::ui(ui, &shared, panels.hex);
                            }
                            StackTool::Send => {
                                if let Some(error) = crate::panels::send::ui(
                                    ui,
                                    engine,
                                    active.as_deref(),
                                    panels.send,
                                ) {
                                    panels.log.error(error);
                                }
                            }
                            StackTool::Log => crate::panels::log::ui(ui, panels.log),
                            StackTool::Ai => crate::panels::ai::ui(ui, panels.ai),
                            StackTool::Network => {
                                crate::panels::net_tools::ui(ui, panels.net, engine);
                            }
                        }
                    },
                );
                ui.add_space(theme::SPACE_1);
                if close {
                    stack.set_open(tool, false);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tools_round_trip_through_ids() {
        let mut stack = StackState::default();
        assert!(stack.is_open(StackTool::Channels));
        stack.set_open(StackTool::Hex, true);
        stack.set_open(StackTool::Channels, false);
        assert!(stack.take_changed());
        assert!(!stack.take_changed());

        let restored = StackState::from_settings(&stack.ids());
        assert!(restored.is_open(StackTool::Hex));
        assert!(!restored.is_open(StackTool::Channels));
    }
}
