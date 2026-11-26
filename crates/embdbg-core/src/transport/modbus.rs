//! Modbus master: read register blocks on a timer and turn them into channels.
//!
//! Modbus values are numbers, not bytes, so they skip the byte pipeline and land directly in the
//! channel store: the waveform, FFT and histogram views work on them unchanged.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_modbus::client::{Context, rtu, tcp};
use tokio_modbus::prelude::*;

use crate::clock::Clock;

/// How to reach the slave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModbusEndpoint {
    Tcp { host: String, port: u16 },
    Rtu { port: String, baud: u32 },
}

/// Which register area a block reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Holding,
    Input,
    Coil,
    Discrete,
}

impl BlockKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Holding => "holding",
            Self::Input => "input",
            Self::Coil => "coil",
            Self::Discrete => "discrete",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "holding" | "hr" | "3" => Some(Self::Holding),
            "input" | "ir" | "4" => Some(Self::Input),
            "coil" | "c" | "1" => Some(Self::Coil),
            "discrete" | "di" | "2" => Some(Self::Discrete),
            _ => None,
        }
    }
}

/// One contiguous read in the polling table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModbusBlock {
    pub kind: BlockKind,
    pub start: u16,
    pub count: u16,
    /// Channel name prefix; empty names channels by kind and address.
    pub name: String,
}

impl ModbusBlock {
    /// Parses one line of the table: `holding 0 8 [name]`.
    pub fn parse_spec(line: &str) -> Result<Self, String> {
        let mut parts = line.split_whitespace();
        let kind_text = parts.next().ok_or_else(|| "空行".to_owned())?;
        let kind = BlockKind::parse(kind_text)
            .ok_or_else(|| format!("未知类型 {kind_text}：用 holding、input、coil 或 discrete"))?;
        let start: u16 = parts
            .next()
            .ok_or_else(|| format!("{kind_text} 缺少起始地址"))?
            .parse()
            .map_err(|_| format!("{kind_text} 的起始地址不是数字"))?;
        let count: u16 = parts
            .next()
            .ok_or_else(|| format!("{kind_text} 缺少数量"))?
            .parse()
            .map_err(|_| format!("{kind_text} 的数量不是数字"))?;
        if count == 0 || count > 125 {
            return Err(format!("{kind_text} 的数量应在 1..=125"));
        }
        let name = parts.collect::<Vec<_>>().join(" ");
        Ok(Self {
            kind,
            start,
            count,
            name,
        })
    }

    /// One name per register.
    pub fn channel_names(&self) -> Vec<String> {
        (0..self.count)
            .map(|offset| {
                if self.name.is_empty() {
                    format!("{}[{}]", self.kind.id(), self.start + offset)
                } else if self.count == 1 {
                    self.name.clone()
                } else {
                    format!("{}[{}]", self.name, offset)
                }
            })
            .collect()
    }
}

/// A polling table and where to send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModbusConfig {
    pub endpoint: ModbusEndpoint,
    /// Unit / slave id.
    pub slave: u8,
    pub interval_ms: u64,
    pub blocks: Vec<ModbusBlock>,
}

impl ModbusConfig {
    pub fn describe(&self) -> String {
        let where_ = match &self.endpoint {
            ModbusEndpoint::Tcp { host, port } => format!("{host}:{port}"),
            ModbusEndpoint::Rtu { port, baud } => format!("{port} @ {baud}"),
        };
        format!("{where_} · 从站 {} · {} 块", self.slave, self.blocks.len())
    }

    pub fn channel_names(&self) -> Vec<String> {
        self.blocks
            .iter()
            .flat_map(ModbusBlock::channel_names)
            .collect()
    }

    pub fn total_channels(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| usize::from(block.count))
            .sum()
    }
}

/// What a Modbus task reports.
#[derive(Debug)]
pub enum ModbusEvent {
    /// One polling cycle: all blocks, in table order.
    Samples { t: f64, values: Vec<f32> },
    /// Connected.
    Info(String),
    /// The master stopped; a read or connection failure is reported here.
    Ended(Option<String>),
}

/// A read or write asked for between polling cycles.
#[derive(Debug)]
pub enum ModbusRequest {
    Read {
        kind: BlockKind,
        start: u16,
        count: u16,
        reply: oneshot::Sender<Result<Vec<u16>, String>>,
    },
    Write {
        kind: BlockKind,
        start: u16,
        values: Vec<u16>,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

pub struct ModbusLink {
    stop: watch::Sender<bool>,
    pub requests: UnboundedSender<ModbusRequest>,
}

impl ModbusLink {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for ModbusLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Connects and starts polling. A connection failure is reported as an event.
pub fn spawn(
    config: &ModbusConfig,
    events: UnboundedSender<ModbusEvent>,
    clock: Clock,
) -> ModbusLink {
    let (stop, mut stop_rx) = watch::channel(false);
    let (requests, mut request_rx) = mpsc::unbounded_channel::<ModbusRequest>();
    let config = config.clone();

    tokio::spawn(async move {
        let mut context = match connect(&config).await {
            Ok(context) => context,
            Err(error) => {
                let _ = events.send(ModbusEvent::Ended(Some(error)));
                return;
            }
        };
        let _ = events.send(ModbusEvent::Info("已连接".to_owned()));

        let interval = Duration::from_millis(config.interval_ms.clamp(20, 60_000));
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,
                request = request_rx.recv() => match request {
                    Some(request) => handle_request(&mut context, request).await,
                    None => break,
                },
                _ = ticker.tick() => match read_all(&mut context, &config.blocks).await {
                    Ok(values) => {
                        let _ = events.send(ModbusEvent::Samples { t: clock.now(), values });
                    }
                    Err(error) => {
                        let _ = events.send(ModbusEvent::Ended(Some(error)));
                        return;
                    }
                },
            }
        }
        let _ = context.disconnect().await;
        let _ = events.send(ModbusEvent::Ended(None));
    });

    ModbusLink { stop, requests }
}

async fn handle_request(context: &mut Context, request: ModbusRequest) {
    match request {
        ModbusRequest::Read {
            kind,
            start,
            count,
            reply,
        } => {
            let _ = reply.send(read_block(context, kind, start, count).await);
        }
        ModbusRequest::Write {
            kind,
            start,
            values,
            reply,
        } => {
            let _ = reply.send(write_block(context, kind, start, &values).await);
        }
    }
}

async fn connect(config: &ModbusConfig) -> Result<Context, String> {
    match &config.endpoint {
        ModbusEndpoint::Tcp { host, port } => {
            let address = format!("{host}:{port}");
            let addr: SocketAddr = address
                .to_socket_addrs()
                .map_err(|error| format!("解析 {address} 失败：{error}"))?
                .next()
                .ok_or_else(|| format!("{address} 没有地址"))?;
            tcp::connect_slave(addr, Slave(config.slave))
                .await
                .map_err(|error| format!("连接 {address} 失败：{error}"))
        }
        ModbusEndpoint::Rtu { port, baud } => {
            let builder = tokio_serial::new(port, *baud).timeout(Duration::from_millis(500));
            let stream = tokio_serial::SerialStream::open(&builder)
                .map_err(|error| format!("打开串口 {port} 失败：{error}"))?;
            Ok(rtu::attach_slave(stream, Slave(config.slave)))
        }
    }
}

async fn read_all(context: &mut Context, blocks: &[ModbusBlock]) -> Result<Vec<f32>, String> {
    let mut values = Vec::new();
    for block in blocks {
        let words = read_block(context, block.kind, block.start, block.count).await?;
        values.extend(words.into_iter().map(f32::from));
    }
    Ok(values)
}

/// Reads one area. Coils and discrete inputs come back as 0 or 1, so every area is a list of
/// words on the way out.
async fn read_block(
    context: &mut Context,
    kind: BlockKind,
    start: u16,
    count: u16,
) -> Result<Vec<u16>, String> {
    let failed = |error: tokio_modbus::Error| {
        format!(
            "读取 {} {}..{} 失败：{error}",
            kind.id(),
            start,
            start.saturating_add(count)
        )
    };
    let exception = |exception: tokio_modbus::ExceptionCode| format!("Modbus 异常：{exception:?}");
    match kind {
        BlockKind::Holding => context
            .read_holding_registers(start, count)
            .await
            .map_err(failed)?
            .map_err(exception),
        BlockKind::Input => context
            .read_input_registers(start, count)
            .await
            .map_err(failed)?
            .map_err(exception),
        BlockKind::Coil => Ok(context
            .read_coils(start, count)
            .await
            .map_err(failed)?
            .map_err(exception)?
            .into_iter()
            .map(u16::from)
            .collect()),
        BlockKind::Discrete => Ok(context
            .read_discrete_inputs(start, count)
            .await
            .map_err(failed)?
            .map_err(exception)?
            .into_iter()
            .map(u16::from)
            .collect()),
    }
}

/// Writes one area. Input registers and discrete inputs are read-only on the wire.
async fn write_block(
    context: &mut Context,
    kind: BlockKind,
    start: u16,
    values: &[u16],
) -> Result<(), String> {
    let failed = |error: tokio_modbus::Error| {
        format!(
            "写 {} {}..{} 失败：{error}",
            kind.id(),
            start,
            start.saturating_add(values.len() as u16)
        )
    };
    let exception = |exception: tokio_modbus::ExceptionCode| format!("Modbus 异常：{exception:?}");
    match kind {
        BlockKind::Holding => {
            if let [word] = values {
                context
                    .write_single_register(start, *word)
                    .await
                    .map_err(failed)?
                    .map_err(exception)
            } else {
                context
                    .write_multiple_registers(start, values)
                    .await
                    .map_err(failed)?
                    .map_err(exception)
            }
        }
        BlockKind::Coil => {
            let coils: Vec<bool> = values.iter().map(|value| *value != 0).collect();
            if let [coil] = coils[..] {
                context
                    .write_single_coil(start, coil)
                    .await
                    .map_err(failed)?
                    .map_err(exception)
            } else {
                context
                    .write_multiple_coils(start, &coils)
                    .await
                    .map_err(failed)?
                    .map_err(exception)
            }
        }
        BlockKind::Input | BlockKind::Discrete => Err(format!("{} 是只读区域，不能写", kind.id())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_lines_parse_into_blocks() {
        let block = ModbusBlock::parse_spec("holding 0 8 母线电压").unwrap();
        assert_eq!(block.kind, BlockKind::Holding);
        assert_eq!((block.start, block.count), (0, 8));
        assert_eq!(block.name, "母线电压");

        let block = ModbusBlock::parse_spec("3 10 2").unwrap();
        assert_eq!(block.kind, BlockKind::Holding);
        assert_eq!(block.channel_names(), ["holding[10]", "holding[11]"]);
    }

    #[test]
    fn bad_spec_lines_say_what_is_wrong() {
        assert!(ModbusBlock::parse_spec("").is_err());
        assert!(
            ModbusBlock::parse_spec("holding 0")
                .unwrap_err()
                .contains("数量")
        );
        assert!(
            ModbusBlock::parse_spec("holding x 4")
                .unwrap_err()
                .contains("地址")
        );
        assert!(
            ModbusBlock::parse_spec("holding 0 0")
                .unwrap_err()
                .contains("1..=125")
        );
        assert!(
            ModbusBlock::parse_spec("nope 0 4")
                .unwrap_err()
                .contains("未知类型")
        );
    }

    #[test]
    fn names_cover_every_register() {
        let config = ModbusConfig {
            endpoint: ModbusEndpoint::Tcp {
                host: "127.0.0.1".to_owned(),
                port: 502,
            },
            slave: 1,
            interval_ms: 500,
            blocks: vec![
                ModbusBlock {
                    kind: BlockKind::Holding,
                    start: 0,
                    count: 2,
                    name: String::new(),
                },
                ModbusBlock {
                    kind: BlockKind::Coil,
                    start: 0,
                    count: 1,
                    name: "泵".to_owned(),
                },
            ],
        };
        assert_eq!(config.total_channels(), 3);
        assert_eq!(config.channel_names(), ["holding[0]", "holding[1]", "泵"]);
    }
}
