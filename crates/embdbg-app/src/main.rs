//! Program entry. `embdbg` opens the window; engine-only subcommands come later.

mod app;
mod fonts;
mod headless;
mod icons;
mod logging;
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    logging::init().context("初始化日志失败")?;
    logging::capture_panics();

    // Subcommands that need no window run before the graphics stack is set up at all.
    match &cli.command {
        Some(Command::Ports) => return headless::ports(),
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
