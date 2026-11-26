//! The settings window: a page menu on the left, that page's settings on the right.

use std::path::{Path, PathBuf};

use egui::{Button, Frame, Margin, RichText, ScrollArea};

use crate::settings::{RendererChoice, Settings};
use crate::theme::{self, FG_DIM, SPACE_1, SPACE_2};

/// Frame rate ceilings offered in the settings, in frames per second; 0 follows the display.
const MAX_FPS_CHOICES: [f64; 4] = [0.0, 30.0, 60.0, 120.0];

/// The pages of the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Renderer,
    Performance,
    Plugins,
}

impl Page {
    const ALL: [Self; 3] = [Self::Renderer, Self::Performance, Self::Plugins];

    fn title(self) -> &'static str {
        match self {
            Self::Renderer => "渲染后端",
            Self::Performance => "帧率",
            Self::Plugins => "解码插件",
        }
    }
}

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
#[allow(clippy::too_many_arguments)]
pub fn ui(
    ctx: &egui::Context,
    open: &mut bool,
    pending: &mut Settings,
    page: &mut Page,
    running_backend: &str,
    plugin_dirs: &[PathBuf],
    plugin_notice: &mut Option<(String, bool)>,
    confirm_uninstall: &mut Option<String>,
) -> Action {
    let mut action = Action::None;
    let screen = ctx
        .input(|input| input.raw.screen_rect)
        .map_or(egui::vec2(1200.0, 800.0), |rect| rect.size());
    let window_height = (screen.y - 140.0).clamp(400.0, 720.0);

    egui::Window::new("设置")
        .open(open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(680.0, window_height))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_min_size(egui::vec2(620.0, window_height));

            // The actions stay put while the page on the right scrolls.
            egui::Panel::bottom("settings-actions")
                .exact_size(46.0)
                .frame(Frame::new().inner_margin(Margin::symmetric(8, 6)))
                .show(ui, |ui| {
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

            egui::Panel::left("settings-pages")
                .exact_size(140.0)
                .resizable(false)
                .frame(Frame::new().inner_margin(Margin::symmetric(6, 8)))
                .show(ui, |ui| {
                    for candidate in Page::ALL {
                        let button = Button::new(RichText::new(candidate.title()))
                            .min_size(egui::vec2(ui.available_width(), 30.0))
                            .selected(*page == candidate);
                        if ui.add(button).clicked() {
                            *page = candidate;
                        }
                        ui.add_space(SPACE_1);
                    }
                });

            egui::CentralPanel::default()
                .frame(Frame::new().inner_margin(Margin::symmetric(12, 8)))
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match *page {
                            Page::Renderer => renderer_section(ui, pending, running_backend),
                            Page::Performance => fps_section(ui, pending),
                            Page::Plugins => {
                                plugins_section(ui, plugin_dirs, plugin_notice, confirm_uninstall);
                            }
                        });
                });
        });

    action
}

fn renderer_section(ui: &mut egui::Ui, pending: &mut Settings, running_backend: &str) {
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
}

fn fps_section(ui: &mut egui::Ui, pending: &mut Settings) {
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
}

fn plugins_section(
    ui: &mut egui::Ui,
    plugin_dirs: &[PathBuf],
    plugin_notice: &mut Option<(String, bool)>,
    confirm_uninstall: &mut Option<String>,
) {
    ui.add_space(SPACE_2);
    ui.separator();
    ui.label(RichText::new("解码插件").strong());
    ui.label(
        RichText::new("一个插件是一个文件夹，内含 plugin.toml 与 .wasm；说明见 docs/plugins.md。")
            .small()
            .color(FG_DIM),
    );

    let user_root = crate::logging::app_data_dir().map(|dir| dir.join("plugins"));
    ui.horizontal(|ui| {
        if ui
            .button("安装插件…")
            .on_hover_text("选择包含 plugin.toml 的插件文件夹")
            .clicked()
            && let Some(source) = rfd::FileDialog::new()
                .set_title("选择插件文件夹（含 plugin.toml）")
                .pick_folder()
        {
            match &user_root {
                Some(root) => {
                    match embdbg_core::plugins::install_from(&source, root, plugin_dirs) {
                        Ok(info) => {
                            *plugin_notice =
                                Some((format!("已安装「{}」v{}", info.name, info.version), true));
                        }
                        Err(error) => *plugin_notice = Some((format!("安装失败：{error}"), false)),
                    }
                }
                None => {
                    *plugin_notice = Some(("找不到用户配置目录，无法安装插件".to_owned(), false));
                }
            }
        }
        if ui.button("重新扫描").clicked() {
            let count = embdbg_core::plugins::load_from_dirs(plugin_dirs);
            *plugin_notice = Some((format!("已重新扫描：{count} 个插件"), true));
            *confirm_uninstall = None;
        }
        if ui
            .add_enabled(user_root.is_some(), Button::new("打开插件目录"))
            .clicked()
            && let Some(root) = &user_root
        {
            open_folder(root);
        }
    });

    if let Some((text, ok)) = plugin_notice.as_ref() {
        ui.label(RichText::new(text).small().color(if *ok {
            theme::SUCCESS
        } else {
            theme::DANGER
        }));
    }

    let plugins = embdbg_core::plugins::list();
    if plugins.is_empty() {
        ui.label(RichText::new("还没有安装插件。").small().color(FG_DIM));
        return;
    }
    ScrollArea::vertical()
        .max_height(200.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for plugin in &plugins {
                plugin_row(
                    ui,
                    plugin,
                    plugin_dirs,
                    user_root.as_deref(),
                    plugin_notice,
                    confirm_uninstall,
                );
                ui.add_space(SPACE_1);
            }
        });
}

fn plugin_row(
    ui: &mut egui::Ui,
    plugin: &embdbg_core::plugins::PluginInfo,
    plugin_dirs: &[PathBuf],
    user_root: Option<&Path>,
    plugin_notice: &mut Option<(String, bool)>,
    confirm_uninstall: &mut Option<String>,
) {
    let removable = user_root.is_some_and(|root| plugin.dir.starts_with(root));
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} v{}", plugin.name, plugin.version)).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if *confirm_uninstall == Some(plugin.name.clone()) {
                if ui.small_button("确认卸载").clicked() {
                    if let Some(root) = user_root {
                        match embdbg_core::plugins::uninstall(plugin.slot, root, plugin_dirs) {
                            Ok(()) => {
                                *plugin_notice = Some((format!("已卸载「{}」", plugin.name), true));
                            }
                            Err(error) => {
                                *plugin_notice = Some((format!("卸载失败：{error}"), false));
                            }
                        }
                    }
                    *confirm_uninstall = None;
                }
                if ui.small_button("取消").clicked() {
                    *confirm_uninstall = None;
                }
            } else {
                let button = ui
                    .add_enabled(removable, Button::new("卸载").small())
                    .on_hover_text(if removable {
                        "删除用户插件目录里的这个插件"
                    } else {
                        "只能卸载用户插件目录里的插件"
                    });
                if button.clicked() {
                    *confirm_uninstall = Some(plugin.name.clone());
                }
            }
        });
    });
    if !plugin.description.is_empty() {
        ui.label(RichText::new(&plugin.description).small().color(FG_DIM));
    }
    ui.label(
        RichText::new(plugin.dir.display().to_string())
            .small()
            .color(theme::FG_DIM),
    );
    if !plugin.fields.is_empty() {
        let labels: Vec<&str> = plugin
            .fields
            .iter()
            .map(|field| field.label.as_str())
            .collect();
        ui.label(
            RichText::new(format!("参数：{}", labels.join("、")))
                .small()
                .color(FG_DIM),
        );
    }
}

/// Opens a folder in the system file manager; failures are not worth a dialog.
fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = "xdg-open";
    let _ = std::process::Command::new(command).arg(path).spawn();
}
