//! Program entry. `embdbg` opens the window; engine-only subcommands come later.

// A release build is a window program: double-clicking it must not leave a console behind. Debug
// builds keep the console so `cargo run` and the subcommands print where they are expected to.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod fonts;
mod headless;
mod icons;
mod layout;
mod logging;
mod mcp_host;
mod panels;
mod plots;
mod screenshot;
mod settings;
mod theme;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use embdbg_core::engine::Engine;
use embdbg_core::pipeline::{DEFAULT_CAPACITY, DecoderSpec};
use embdbg_core::platform::TimerResolution;
use embdbg_core::transport::{SerialConfig, SimulatorConfig};

use crate::settings::{RendererChoice, Settings};

#[derive(Parser)]
#[command(name = "embdbg", version, about = "嵌入式调试助手")]
struct Cli {
    /// 渲染后端；不填则用设置里保存的（默认自动）
    #[arg(long, value_enum)]
    renderer: Option<RendererChoice>,

    /// 启动时打开内置模拟信号源，没有硬件也能试用各个视图
    #[arg(long)]
    sim: bool,

    /// 不申请 1 ms 的系统定时精度。更省电，但定时唤醒和重绘节奏会粗到约 15 ms
    #[arg(long)]
    coarse_timers: bool,

    /// 数据流动时窗口每秒最多重绘多少次；0 表示跟随显示器刷新率。
    ///
    /// 跟随刷新率最平稳：定时唤醒有 1–2 ms 抖动，设了上限反而会错过垂直同步点。
    #[arg(long, allow_hyphen_values = false)]
    max_fps: Option<f64>,

    /// 渲染性能自检：带着模拟信号源运行这么多秒，把帧率写进 --perf-report 后退出
    #[arg(long, requires = "perf_report")]
    perf_test: Option<f64>,

    /// --perf-test 结果的输出文件
    #[arg(long, requires = "perf_test")]
    perf_report: Option<PathBuf>,

    /// 启动两秒后把窗口存成 PNG 并退出，用于自检和问题反馈
    #[arg(long, conflicts_with = "perf_test")]
    screenshot: Option<PathBuf>,

    /// 启动时打开设置窗口
    #[arg(long)]
    open_settings: bool,

    /// 启动时选中哪个页签（waveform/xy/spectrum/histogram/terminal/hex/send）
    #[arg(long)]
    tab: Option<String>,

    /// 额外扫描的插件目录，可重复；优先于用户目录和程序目录
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dirs: Vec<PathBuf>,

    /// 启动时直接打开一个串口（界面模式）
    #[arg(long, value_name = "PORT")]
    serial: Option<String>,

    /// --serial 的波特率
    #[arg(long, default_value_t = 115_200)]
    serial_baud: u32,

    /// --serial 的协议：line、justfloat 或 justint
    #[arg(long, default_value = "line")]
    serial_decoder: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 无界面自检一个串口：收发并打印结果
    Serial {
        /// 端口名，如 COM5
        port: String,
        #[arg(long, default_value_t = 115_200)]
        baud: u32,
        /// 协议：line、justfloat 或 justint
        #[arg(long, default_value = "line")]
        decoder: String,
        /// 监听多少秒后退出
        #[arg(long, default_value_t = 3.0)]
        listen: f64,
        /// 在这段时间后发送一次文本（会按 --ending 追加换行）
        #[arg(long)]
        send: Option<String>,
        /// 换行方式：none、cr、lf 或 crlf
        #[arg(long, default_value = "crlf")]
        ending: String,
        /// 改为发送这些十六进制字节，如 "01 02 0A"
        #[arg(long)]
        hex_send: Option<String>,
        /// 打开后多久发送
        #[arg(long, default_value_t = 0.5)]
        send_after: f64,
    },
    /// 列出本机串口
    Ports,
    /// 以 MCP 服务运行：有窗口时接管当前会话，否则无界面运行
    Mcp,
    /// 插件管理
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// 列出已安装的解码插件
    List,
    /// 打印插件目录
    Dirs,
}

fn main() -> anyhow::Result<()> {
    attach_parent_console();
    let cli = Cli::parse();
    // A window program has no console to print a startup failure to. A subcommand is a console
    // program run by a person or an AI client; a dialog would block it.
    let headless = cli.command.is_some();
    let result = run(cli);
    if let Err(error) = &result
        && !headless
    {
        show_failure(&format!("{error:#}"));
    }
    result
}

fn run(cli: Cli) -> anyhow::Result<()> {
    logging::init().context("初始化日志失败")?;
    logging::capture_panics();
    let plugins = load_plugins(&cli.plugin_dirs);
    tracing::info!(target: "embdbg::plugins", "插件目录扫描完成：{plugins} 个插件");

    // Subcommands that need no window run before the graphics stack is set up at all.
    match &cli.command {
        Some(Command::Ports) => return headless::ports(),
        Some(Command::Mcp) => return mcp_host::run_stdio(),
        Some(Command::Plugin { action }) => {
            match action {
                PluginAction::Dirs => {
                    for dir in crate::app::plugin_dirs(&cli.plugin_dirs) {
                        println!("{}", dir.display());
                    }
                }
                PluginAction::List => {
                    let plugins = embdbg_core::plugins::list();
                    if plugins.is_empty() {
                        println!("没有已安装的插件。插件目录：");
                        for dir in crate::app::plugin_dirs(&cli.plugin_dirs) {
                            println!("  {}", dir.display());
                        }
                    }
                    for info in plugins {
                        println!("{} v{} — {}", info.name, info.version, info.description);
                    }
                }
            }
            return Ok(());
        }
        Some(Command::Serial {
            port,
            baud,
            decoder,
            listen,
            send,
            ending,
            hex_send,
            send_after,
        }) => {
            let decoder = parse_decoder(decoder)?;
            let ending: &[u8] = match ending.as_str() {
                "none" => b"",
                "cr" => b"\r",
                "lf" => b"\n",
                "crlf" => b"\r\n",
                other => anyhow::bail!("未知换行方式 {other}：用 none、cr、lf 或 crlf"),
            };
            let send_bytes = match (hex_send, send) {
                (Some(hex), _) => Some(parse_hex(hex)?),
                (None, Some(text)) => {
                    let mut bytes = text.as_bytes().to_vec();
                    bytes.extend_from_slice(ending);
                    Some(bytes)
                }
                (None, None) => None,
            };
            return headless::serial(headless::SerialArgs {
                port: port.clone(),
                baud: *baud,
                decoder,
                listen: *listen,
                send: send_bytes,
                send_after: *send_after,
                hex: hex_send.is_some(),
            });
        }
        None => {}
    }

    // Timed waits in this process, including repaint scheduling, are only as fine as the system timer.
    let _fine_timer = (!cli.coarse_timers).then(TimerResolution::finest);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 Tokio 运行时失败")?;
    let settings = Settings::load().with_cli(cli.renderer, cli.max_fps);
    let engine = Arc::new(Engine::new(runtime.handle().clone(), DEFAULT_CAPACITY));
    if cli.sim || cli.perf_test.is_some() {
        engine.start_simulator(SimulatorConfig::default());
    }

    let startup_serial = cli.serial.as_ref().map(|port| SerialConfig {
        port: port.clone(),
        baud: cli.serial_baud,
        ..SerialConfig::default()
    });
    let startup_decoder = parse_decoder(&cli.serial_decoder)?;

    let result = app::run(&cli, engine, settings, startup_serial, startup_decoder);
    runtime.shutdown_timeout(std::time::Duration::from_millis(300));
    result
}

/// Reads a decoder name from the command line.
fn parse_decoder(name: &str) -> anyhow::Result<DecoderSpec> {
    match name {
        "line" | "文本行" => Ok(DecoderSpec::Line),
        "justfloat" | "JustFloat" => Ok(DecoderSpec::JustFloat),
        "justint" | "JustInt" => Ok(DecoderSpec::JustInt),
        other => anyhow::bail!("未知协议 {other}：用 line、justfloat 或 justint"),
    }
}

/// Scans the extra directories, the user's plugin directory and the one next to the program.
/// A broken plugin is logged and skipped; it must never keep the program from starting.
fn load_plugins(extra: &[PathBuf]) -> usize {
    embdbg_core::plugins::load_from_dirs(&crate::app::plugin_dirs(extra))
}

/// Parses "01 02 0A" or "0x01,0x02" into bytes.
fn parse_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = text
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',' && *character != ':')
        .collect();
    if !cleaned.len().is_multiple_of(2) {
        anyhow::bail!("HEX 输入有 {} 个字符，凑不成整字节", cleaned.len());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&cleaned[index..index + 2], 16)
                .map_err(|_| anyhow::anyhow!("不是有效的 HEX：{}", &cleaned[index..index + 2]))
        })
        .collect()
}

/// A window program starts without a console. A headless subcommand run from a terminal still
/// wants its output there, so borrow the parent's console when there is one; a double-click has
/// no parent console, which is exactly when nothing should appear behind the window.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }

    // SAFETY: attaching a console touches no memory this process owns.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_parent_console() {}

/// Shows a startup failure in a dialog, since a window program has no console to print it to.
/// The log file has the details either way.
#[cfg(all(windows, not(debug_assertions)))]
fn show_failure(message: &str) {
    use std::os::windows::ffi::OsStrExt;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            flags: u32,
        ) -> i32;
    }

    let (text, caption) = (wide(message), wide("embdbg 启动失败"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            0x0000_0010,
        );
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn show_failure(_message: &str) {}
