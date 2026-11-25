//! The settings window: renderer, frame rate ceiling, and the restart that applies them.

use egui::RichText;

use crate::settings::{RendererChoice, Settings};
use crate::theme::{self, FG_DIM, SPACE_1, SPACE_2};

/// Frame rate ceilings offered in the settings, in frames per second; 0 follows the display.
const MAX_FPS_CHOICES: [f64; 4] = [0.0, 30.0, 60.0, 120.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep editing.
    None,
    /// Write the settings; the frame rate takes effect at once.
    Save,
    /// Write the settings and start the program over, which is what a new renderer needs.
    SaveAndRestart,
}

/// Draws the settings window. Returns what the user asked for.
pub fn ui(
    ctx: &egui::Context,
    open: &mut bool,
    pending: &mut Settings,
    running_backend: &str,
) -> Action {
    let mut action = Action::None;

    egui::Window::new("设置")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.add_space(SPACE_1);

            ui.label(RichText::new("渲染后端").strong());
            for choice in [
                RendererChoice::Auto,
                RendererChoice::Glow,
                RendererChoice::Wgpu,
                RendererChoice::Dx12,
                RendererChoice::Vulkan,
            ] {
                // Offer only what the platform can do; a choice from another platform still shows.
                let capable = match choice {
                    RendererChoice::Dx12 => cfg!(target_os = "windows"),
                    RendererChoice::Vulkan => !cfg!(target_os = "macos"),
                    _ => true,
                };
                if !capable && pending.renderer != choice {
                    continue;
                }
                ui.add_enabled_ui(capable, |ui| {
                    ui.radio_value(&mut pending.renderer, choice, choice.label())
                        .on_hover_text(choice.detail());
                });
                ui.label(RichText::new(choice.detail()).small().color(FG_DIM));
            }
            ui.add_space(SPACE_1);
            ui.label(
                RichText::new(format!("当前运行的渲染后端：{running_backend}"))
                    .small()
                    .color(FG_DIM),
            );
            ui.label(
                RichText::new("换渲染后端需要重启程序才生效。")
                    .small()
                    .color(theme::WARNING),
            );

            ui.add_space(SPACE_2);
            ui.label(RichText::new("数据流入时的最高帧率").strong());
            ui.horizontal_wrapped(|ui| {
                for fps in MAX_FPS_CHOICES {
                    let label = if fps == 0.0 {
                        "跟随显示器".to_owned()
                    } else {
                        format!("{fps:.0} 帧")
                    };
                    ui.radio_value(&mut pending.max_fps, fps, label);
                }
            });
            ui.label(
                RichText::new("跟随显示器最平稳；设上限更省电，但在高刷新率屏幕上节奏可能不匀。")
                    .small()
                    .color(FG_DIM),
            );

            ui.add_space(SPACE_2);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    action = Action::Save;
                }
                if ui.button("保存并重启").clicked() {
                    action = Action::SaveAndRestart;
                }
                ui.label(
                    RichText::new("设置保存在 config.toml")
                        .small()
                        .color(FG_DIM),
                );
            });
        });

    action
}
