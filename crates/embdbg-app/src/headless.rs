//! Headless runs: no window, everything printed to the console.
//!
//! `embdbg serial` is the smoke test for a real port, and the shape the headless MCP server
//! will take: the same engine the window uses, driven from the command line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use embdbg_core::engine::{Engine, TransportConfig};
use embdbg_core::pipeline::{DEFAULT_CAPACITY, DecoderSpec, Direction};
use embdbg_core::transport::{SerialConfig, list_ports};

/// Opens a serial port, talks on it for `listen` seconds and prints what came back.
pub struct SerialArgs {
    pub port: String,
    pub baud: u32,
    pub decoder: DecoderSpec,
    pub listen: f64,
    /// Text to send, already decoded to bytes (with its line ending).
    pub send: Option<Vec<u8>>,
    pub send_after: f64,
    pub hex: bool,
}

pub fn serial(args: SerialArgs) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 Tokio 运行时失败")?;
    let engine = Arc::new(Engine::new(runtime.handle().clone(), DEFAULT_CAPACITY));

    let id = engine
        .open(TransportConfig::Serial(SerialConfig {
            port: args.port.clone(),
            baud: args.baud,
            ..SerialConfig::default()
        }))
        .map_err(anyhow::Error::msg)?;
    let connection = engine.connection(id).context("连接刚打开却查不到")?;
    connection.set_decoder(args.decoder);

    println!(
        "已打开 {} @ {}，协议 {}，监听 {:.1} 秒",
        args.port,
        args.baud,
        decoder_label(args.decoder),
        args.listen
    );
    if let Some(bytes) = &args.send {
        println!(
            "将在 {:.1} 秒后发送 {} 字节：{}",
            args.send_after,
            bytes.len(),
            if args.hex {
                hex(bytes)
            } else {
                String::from_utf8_lossy(bytes).escape_debug().to_string()
            }
        );
    }

    let mut events = engine.subscribe();
    let mut printer = Printer::default();
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(args.listen);
    let send_at = started + Duration::from_secs_f64(args.send_after);
    let mut sent = false;

    runtime.block_on(async {
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if let Some(bytes) = &args.send
                && !sent
                && now >= send_at
            {
                sent = true;
                match engine.send(id, bytes) {
                    Ok(()) => println!("[发送] {} 字节", bytes.len()),
                    Err(error) => println!("[发送失败] {error}"),
                }
            }
            while let Ok(event) = events.try_recv() {
                println!("[事件] {event}");
            }
            printer.print_new(&connection.shared);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    printer.print_new(&connection.shared);
    printer.print_summary(&connection.shared);

    engine.close(id);
    runtime.shutdown_timeout(Duration::from_millis(200));
    Ok(())
}

/// Lists the serial ports and exits.
pub fn ports() -> anyhow::Result<()> {
    let ports = list_ports();
    if ports.is_empty() {
        println!("没有发现串口");
        return Ok(());
    }
    for port in ports {
        println!("{:<10} {}", port.name, port.details);
    }
    Ok(())
}

/// Remembers what was already printed, so every line and byte chunk appears once.
#[derive(Default)]
struct Printer {
    lines: usize,
    chunks: usize,
}

impl Printer {
    fn print_new(&mut self, shared: &embdbg_core::pipeline::SessionShared) {
        let lines = shared.lines.read();
        for line in lines.lines().skip(self.lines) {
            let marker = match line.direction {
                Direction::RemoteToHere => "收",
                Direction::HereToRemote => "发",
            };
            println!("[{marker}] {}", line.text.escape_debug());
        }
        self.lines = lines.len();
        drop(lines);

        let bytes = shared.bytes.read();
        for chunk in bytes.chunks().skip(self.chunks) {
            let marker = match chunk.direction {
                Direction::RemoteToHere => "收",
                Direction::HereToRemote => "发",
            };
            let shown = hex(&chunk.bytes[..chunk.bytes.len().min(48)]);
            let more = if chunk.bytes.len() > 48 { " …" } else { "" };
            println!("[{marker} {} 字节] {shown}{more}", chunk.bytes.len());
        }
        self.chunks = bytes.chunks().count();
    }

    fn print_summary(&self, shared: &embdbg_core::pipeline::SessionShared) {
        let (rx, tx, frames, samples, dropped) = shared.stats.summary();
        println!("统计：收 {rx} 字节，发 {tx} 字节，帧 {frames}，样本 {samples}，丢弃 {dropped}");
        let data = shared.channels.read();
        if data.channels().is_empty() {
            println!("没有解出通道");
        } else {
            println!(
                "通道 {} 个，样本 {} 帧，采样率 {}",
                data.channels().len(),
                data.frames(),
                data.axis()
                    .frame_rate()
                    .map_or_else(|| "未知".to_owned(), |rate| format!("{rate:.0} /s"))
            );
            for (index, channel) in data.channels().iter().enumerate() {
                let stats = channel.stats();
                println!(
                    "  通道 {} {}：{} 个样本，最小 {}，最大 {}，均值 {:.4}",
                    index + 1,
                    channel.name(),
                    stats.count,
                    stats
                        .min
                        .map_or_else(|| "—".to_owned(), |v| format!("{v:.4}")),
                    stats
                        .max
                        .map_or_else(|| "—".to_owned(), |v| format!("{v:.4}")),
                    stats.mean
                );
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn decoder_label(spec: DecoderSpec) -> &'static str {
    match spec {
        DecoderSpec::Line => "文本行",
        DecoderSpec::JustFloat => "JustFloat",
        DecoderSpec::JustInt => "JustInt",
    }
}
