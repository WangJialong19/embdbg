//! The settings a person can change, and where they are kept.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::logging;

/// Which renderer to start the window with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RendererChoice {
    /// Windows/Linux 上优先 OpenGL，不可用时改用 wgpu；macOS 优先 wgpu（Metal）
    #[default]
    Auto,
    /// OpenGL（eframe 的 glow 后端）：三台机器上实测最省内存、每帧 CPU 最低
    Glow,
    /// 让 wgpu 自己挑图形接口
    Wgpu,
    /// wgpu 的 DirectX 12 后端（仅 Windows）
    Dx12,
    /// wgpu 的 Vulkan 后端（Windows、Linux）
    Vulkan,
}

impl RendererChoice {
    /// How the settings window labels this choice.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动（推荐）",
            Self::Glow => "OpenGL",
            Self::Wgpu => "wgpu 自动选择",
            Self::Dx12 => "DirectX 12",
            Self::Vulkan => "Vulkan",
        }
    }

    /// One line on what the choice means for this platform.
    pub fn detail(self) -> &'static str {
        match self {
            Self::Auto => "Windows/Linux 用 OpenGL，不可用时改用 wgpu；macOS 用 Metal",
            Self::Glow => "最省资源；需要 OpenGL 3.3（远程桌面可能不支持）",
            Self::Wgpu => "由 wgpu 按显卡挑选图形接口",
            Self::Dx12 => "Windows 的现代图形接口，占用多于 OpenGL",
            Self::Vulkan => "部分显卡驱动（如 Intel 核显）上会空转耗 CPU",
        }
    }
}

/// Everything that survives a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub renderer: RendererChoice,
    /// Repaint ceiling per second while data streams; 0 follows the display's refresh rate.
    pub max_fps: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            renderer: RendererChoice::Auto,
            max_fps: 0.0,
        }
    }
}

impl Settings {
    /// `%LOCALAPPDATA%\embdbg\config.toml` on Windows, the equivalent elsewhere.
    pub fn path() -> Option<PathBuf> {
        logging::app_data_dir().map(|dir| dir.join("config.toml"))
    }

    /// Reads the settings file. A missing file, or one that no longer parses, gives the defaults:
    /// a broken config must never keep the program from starting.
    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(
                    target: "embdbg::settings",
                    "忽略无法解析的设置（{}）：{error}",
                    path.display()
                );
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(
                    target: "embdbg::settings",
                    "读取设置失败（{}）：{error}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建设置目录失败：{}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("序列化设置失败")?;
        std::fs::write(path, text).with_context(|| format!("写入设置失败：{}", path.display()))
    }

    /// Applies the command line on top, so a one-off `--renderer vulkan` is not written back.
    pub fn with_cli(mut self, renderer: Option<RendererChoice>, max_fps: Option<f64>) -> Self {
        if let Some(renderer) = renderer {
            self.renderer = renderer;
        }
        if let Some(max_fps) = max_fps {
            self.max_fps = max_fps;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        // A fresh directory per call: the tests run in parallel and must not delete each other's files.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("embdbg-settings-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_round_trip_through_a_file() {
        let dir = temp_dir();
        let path = dir.join("config.toml");
        let settings = Settings {
            renderer: RendererChoice::Vulkan,
            max_fps: 60.0,
        };
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_or_broken_file_gives_the_defaults() {
        let dir = temp_dir();
        assert_eq!(
            Settings::load_from(&dir.join("absent.toml")),
            Settings::default()
        );

        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "renderer = 42\n").unwrap();
        assert_eq!(Settings::load_from(&broken), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fields_missing_from_an_old_file_take_their_defaults() {
        let dir = temp_dir();
        let path = dir.join("old.toml");
        std::fs::write(&path, "renderer = \"dx12\"\n").unwrap();
        let settings = Settings::load_from(&path);
        assert_eq!(settings.renderer, RendererChoice::Dx12);
        assert_eq!(settings.max_fps, 0.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_command_line_overrides_the_file() {
        let saved = Settings {
            renderer: RendererChoice::Glow,
            max_fps: 30.0,
        };
        assert_eq!(
            saved.clone().with_cli(Some(RendererChoice::Vulkan), None),
            Settings {
                renderer: RendererChoice::Vulkan,
                max_fps: 30.0
            }
        );
        assert_eq!(
            saved.with_cli(None, Some(0.0)),
            Settings {
                renderer: RendererChoice::Glow,
                max_fps: 0.0
            }
        );
    }
}
