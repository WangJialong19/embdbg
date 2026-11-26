//! The MCP server: the tools an AI uses to drive the engine.
//!
//! Every client — the window, a stdio bridge, a headless run — serves the same [`McpService`]
//! over its own transport, so an AI sees the same tools whether a person is watching or not.
//! Errors are returned as tool results, not protocol errors, so the model can read them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rmcp::{
    ServerHandler,
    handler::server::wrapper::{Json, Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars::{self, JsonSchema},
    serde::{Deserialize, Serialize},
    tool, tool_handler, tool_router,
};
use tokio::sync::{broadcast, oneshot};

use crate::analysis::{FftAnalyzer, Window, histogram};
use crate::engine::{Connection, ConnectionState, Engine, TransportConfig};
use crate::pipeline::Direction;
use crate::transport::{
    MqttConfig, SerialConfig, SimulatorConfig, TcpClientConfig, TcpServerConfig, UdpConfig,
    WsConfig, list_ports,
};

/// How many tool calls the activity feed keeps for the window.
const ACTIVITY_CAPACITY: usize = 64;

/// How often a wait looks for a new line.
const WAIT_POLL: Duration = Duration::from_millis(20);

/// The longest a wait may run, whatever the caller asks for.
const WAIT_MAX_MS: u64 = 60_000;

/// One tool call, for the AI activity panel in the window.
#[derive(Debug, Clone)]
pub struct Activity {
    pub tool: String,
    pub ok: bool,
    pub summary: String,
    pub at: SystemTime,
}

/// How the service asks the window for a screenshot: hand it a reply channel and return whether
/// a window is there to take the request.
pub type ScreenshotPort =
    Arc<dyn Fn(oneshot::Sender<Result<String, String>>) -> bool + Send + Sync>;

/// The service behind every MCP transport.
pub struct McpService {
    engine: Arc<Engine>,
    activity: broadcast::Sender<Activity>,
    screenshot: Option<ScreenshotPort>,
}

/// A feed shared by every transport, so the window sees the calls from all MCP clients.
pub fn activity_channel() -> (broadcast::Sender<Activity>, broadcast::Receiver<Activity>) {
    broadcast::channel(ACTIVITY_CAPACITY)
}

impl McpService {
    pub fn new(engine: Arc<Engine>) -> Self {
        let (activity, _) = activity_channel();
        Self::with_activity(engine, activity)
    }

    /// A service that reports into an existing activity feed.
    pub fn with_activity(engine: Arc<Engine>, activity: broadcast::Sender<Activity>) -> Self {
        Self {
            engine,
            activity,
            screenshot: None,
        }
    }

    /// Attaches the window, so the `screenshot` tool can ask it for an image.
    pub fn with_screenshot(mut self, port: ScreenshotPort) -> Self {
        self.screenshot = Some(port);
        self
    }

    /// A feed of what the AI does, for the window's activity panel.
    pub fn activity(&self) -> broadcast::Receiver<Activity> {
        self.activity.subscribe()
    }

    /// The connection a tool was aimed at: the named one, or the active one.
    fn connection(&self, id: Option<u64>) -> Result<Arc<Connection>, String> {
        id.map_or_else(
            || {
                self.engine
                    .active()
                    .ok_or_else(|| "没有活动连接；先调用 open_connection".to_owned())
            },
            |id| {
                self.engine
                    .connection(id)
                    .ok_or_else(|| format!("连接 {id} 不存在"))
            },
        )
    }

    fn line_count(&self, connection: &Connection) -> usize {
        connection.shared.lines.read().len()
    }

    /// Waits for a line containing `pattern`, looking only at lines after `start`.
    async fn wait_from(
        &self,
        connection: &Arc<Connection>,
        start: usize,
        pattern: &str,
        timeout_ms: u64,
    ) -> WaitOut {
        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms.min(WAIT_MAX_MS));
        loop {
            let (lines, hit) = lines_since(connection, start, pattern);
            let elapsed_ms = started.elapsed().as_millis() as u64;
            if let Some(line) = hit {
                return WaitOut {
                    found: true,
                    line: Some(line),
                    text_since: join_lines(&lines),
                    elapsed_ms,
                };
            }
            if started.elapsed() >= timeout {
                return WaitOut {
                    found: false,
                    line: None,
                    text_since: join_lines(&lines),
                    elapsed_ms,
                };
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    /// Records a tool call for the activity panel. The summary is the result's JSON, or the
    /// error, cut short so the panel stays readable.
    fn record<T: Serialize>(&self, tool: &str, result: &Result<Json<T>, String>) {
        let (ok, summary) = match result {
            Ok(value) => (true, serde_json::to_string(&value.0).unwrap_or_default()),
            Err(error) => (false, error.clone()),
        };
        let summary: String = summary.chars().take(240).collect();
        let _ = self.activity.send(Activity {
            tool: tool.to_owned(),
            ok,
            summary,
            at: SystemTime::now(),
        });
    }
}

#[tool_router]
impl McpService {
    #[tool(
        name = "list_serial_ports",
        description = "List the serial ports on this computer."
    )]
    fn list_serial_ports(&self) -> Result<Json<PortsOut>, String> {
        let result = Ok(Json(PortsOut {
            ports: list_ports()
                .into_iter()
                .map(|port| PortOut {
                    name: port.name,
                    details: port.details,
                })
                .collect(),
        }));
        self.record("list_serial_ports", &result);
        result
    }

    #[tool(
        name = "open_connection",
        description = "Open a serial port, a TCP client, a TCP server or the built-in simulator. \
                      Returns the connection id used by every other tool."
    )]
    fn open_connection(
        &self,
        Parameters(params): Parameters<OpenParams>,
    ) -> Result<Json<OpenedOut>, String> {
        let result = self.open_inner(params);
        self.record("open_connection", &result);
        result
    }

    #[tool(name = "close_connection", description = "Close a connection by id.")]
    fn close_connection(
        &self,
        Parameters(params): Parameters<ConnectionParams>,
    ) -> Result<Json<ClosedOut>, String> {
        let result = self.close_inner(params);
        self.record("close_connection", &result);
        result
    }

    #[tool(
        name = "list_connections",
        description = "List the open connections and which one is active."
    )]
    fn list_connections(&self) -> Result<Json<ConnectionsOut>, String> {
        let active = self.engine.active_id();
        let connections = self
            .engine
            .connections()
            .into_iter()
            .map(|connection| ConnectionOut {
                id: connection.id,
                kind: connection.config.kind().to_owned(),
                describe: connection.config.describe(),
                state: state_text(&connection.state()),
                active: active == Some(connection.id),
                decoder: connection.decoder().id().to_owned(),
            })
            .collect();
        let result = Ok(Json(ConnectionsOut {
            connections,
            active_id: active,
        }));
        self.record("list_connections", &result);
        result
    }

    #[tool(
        name = "send",
        description = "Send text or hex bytes to a connection. Text can carry a line ending; \
                       hex is sent exactly as given, for example \"01 02 0A\"."
    )]
    fn send(&self, Parameters(params): Parameters<SendParams>) -> Result<Json<SentOut>, String> {
        let result = self.send_inner(params);
        self.record("send", &result);
        result
    }

    #[tool(
        name = "read_lines",
        description = "Read text lines a connection received or sent, newest last. \
                       `limit` keeps only the last lines."
    )]
    fn read_lines(
        &self,
        Parameters(params): Parameters<ReadLinesParams>,
    ) -> Result<Json<LinesOut>, String> {
        let result = self.read_lines_inner(params);
        self.record("read_lines", &result);
        result
    }

    #[tool(
        name = "read_terminal",
        description = "Read the terminal screen as text: what a person would see, including \
                       carriage-return redraws and colours resolved to plain text."
    )]
    fn read_terminal(
        &self,
        Parameters(params): Parameters<ConnectionParams>,
    ) -> Result<Json<TerminalOut>, String> {
        let result = self.read_terminal_inner(params);
        self.record("read_terminal", &result);
        result
    }

    #[tool(
        name = "wait_for",
        description = "Wait for a new line containing `pattern`. Only lines that arrive after \
                       the call count, so an old answer is never mistaken for a new one."
    )]
    async fn wait_for(
        &self,
        Parameters(params): Parameters<WaitParams>,
    ) -> Result<Json<WaitOut>, String> {
        let result = self.wait_for_inner(params).await;
        self.record("wait_for", &result);
        result
    }

    #[tool(
        name = "send_and_wait",
        description = "Send a command and wait for a line containing `pattern`. Useful for \
                       request/response protocols such as AT commands."
    )]
    async fn send_and_wait(
        &self,
        Parameters(params): Parameters<SendAndWaitParams>,
    ) -> Result<Json<WaitOut>, String> {
        let result = self.send_and_wait_inner(params).await;
        self.record("send_and_wait", &result);
        result
    }

    #[tool(
        name = "list_channels",
        description = "List the numeric channels decoded from a connection, with their names \
                       and statistics."
    )]
    fn list_channels(
        &self,
        Parameters(params): Parameters<ConnectionParams>,
    ) -> Result<Json<ChannelsOut>, String> {
        let result = self.list_channels_inner(params);
        self.record("list_channels", &result);
        result
    }

    #[tool(
        name = "read_samples",
        description = "Read the newest samples of one channel. The sample rate is estimated \
                       from arrival times."
    )]
    fn read_samples(
        &self,
        Parameters(params): Parameters<ReadSamplesParams>,
    ) -> Result<Json<SamplesOut>, String> {
        let result = self.read_samples_inner(params);
        self.record("read_samples", &result);
        result
    }

    #[tool(
        name = "get_fft",
        description = "Compute the spectrum of a channel's newest samples and report the \
                       strongest peak. `size` is the transform length; `window` is one of \
                       hann, hamming, blackman, flattop or rectangle."
    )]
    fn get_fft(&self, Parameters(params): Parameters<FftParams>) -> Result<Json<FftOut>, String> {
        let result = self.fft_inner(params);
        self.record("get_fft", &result);
        result
    }

    #[tool(
        name = "get_histogram",
        description = "Count a channel's newest samples into `bins` equal bins over their range."
    )]
    fn get_histogram(
        &self,
        Parameters(params): Parameters<HistogramParams>,
    ) -> Result<Json<HistogramOut>, String> {
        let result = self.histogram_inner(params);
        self.record("get_histogram", &result);
        result
    }

    #[tool(
        name = "screenshot",
        description = "Save a PNG of the window and return its path. Only works while a window                        is open; a headless session has nothing to capture."
    )]
    async fn screenshot(&self) -> Result<Json<ScreenshotOut>, String> {
        let result = self.screenshot_inner().await;
        self.record("screenshot", &result);
        result
    }

    #[tool(
        name = "get_stats",
        description = "Read a connection's byte, frame and sample counters."
    )]
    fn get_stats(
        &self,
        Parameters(params): Parameters<ConnectionParams>,
    ) -> Result<Json<StatsOut>, String> {
        let result = self.stats_inner(params);
        self.record("get_stats", &result);
        result
    }
}

#[tool_handler]
impl ServerHandler for McpService {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("embdbg", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Tools for debugging embedded hardware: open serial or TCP connections, send \
                 and wait for text, and read waveforms, spectra and histograms.",
            )
    }
}

impl McpService {
    fn open_inner(&self, params: OpenParams) -> Result<Json<OpenedOut>, String> {
        let config = match params.transport {
            TransportKind::Serial => {
                let port = params
                    .port
                    .ok_or_else(|| "串口需要 port，例如 COM5 或 /dev/ttyUSB0".to_owned())?;
                TransportConfig::Serial(SerialConfig {
                    port,
                    baud: params.baud.unwrap_or(115_200),
                    ..SerialConfig::default()
                })
            }
            TransportKind::TcpClient => {
                let host = params
                    .host
                    .ok_or_else(|| "TCP 客户端需要 host".to_owned())?;
                let port = params
                    .tcp_port
                    .ok_or_else(|| "TCP 客户端需要 tcp_port".to_owned())?;
                TransportConfig::TcpClient(TcpClientConfig {
                    host,
                    port,
                    connect_timeout_ms: 3000,
                })
            }
            TransportKind::TcpServer => {
                let bind = params.bind.unwrap_or_else(|| "0.0.0.0".to_owned());
                let port = params
                    .tcp_port
                    .ok_or_else(|| "TCP 服务器需要 tcp_port".to_owned())?;
                TransportConfig::TcpServer(TcpServerConfig { bind, port })
            }
            TransportKind::Udp => {
                let port = params
                    .udp_port
                    .ok_or_else(|| "UDP 需要 udp_port（本地端口）".to_owned())?;
                TransportConfig::Udp(UdpConfig {
                    bind: params.bind.unwrap_or_else(|| "0.0.0.0".to_owned()),
                    port,
                    target: params.target.filter(|target| !target.trim().is_empty()),
                })
            }
            TransportKind::WebSocket => {
                let url = params
                    .url
                    .filter(|url| !url.trim().is_empty())
                    .ok_or_else(|| "WebSocket 需要 url，例如 ws://127.0.0.1:8080/".to_owned())?;
                TransportConfig::WebSocket(WsConfig {
                    url: url.trim().to_owned(),
                })
            }
            TransportKind::Mqtt => {
                let host = params
                    .host
                    .ok_or_else(|| "MQTT 需要 host（broker 地址）".to_owned())?;
                TransportConfig::Mqtt(MqttConfig {
                    host,
                    port: params.tcp_port.unwrap_or(1883),
                    client_id: String::new(),
                    username: params.username.filter(|user| !user.trim().is_empty()),
                    password: params.password.filter(|password| !password.is_empty()),
                    subscribe: params.subscribe.unwrap_or_else(|| "embdbg/#".to_owned()),
                    publish: params.publish.unwrap_or_else(|| "embdbg/tx".to_owned()),
                    qos: params.qos.unwrap_or(0).min(2),
                })
            }
            TransportKind::Simulator => TransportConfig::Simulator(SimulatorConfig::default()),
        };
        let id = self.engine.open(config)?;
        let connection = self
            .engine
            .connection(id)
            .ok_or_else(|| "连接刚打开却查不到".to_owned())?;
        Ok(Json(OpenedOut {
            id,
            kind: connection.config.kind().to_owned(),
            describe: connection.config.describe(),
            state: state_text(&connection.state()),
        }))
    }

    fn close_inner(&self, params: ConnectionParams) -> Result<Json<ClosedOut>, String> {
        let connection = self.connection(params.connection_id)?;
        self.engine.close(connection.id);
        Ok(Json(ClosedOut {
            connection_id: connection.id,
        }))
    }

    fn send_inner(&self, params: SendParams) -> Result<Json<SentOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let bytes = build_payload(
            params.text.as_deref(),
            params.hex.as_deref(),
            params.ending.as_deref(),
        )?;
        self.engine.send(connection.id, &bytes)?;
        Ok(Json(SentOut {
            connection_id: connection.id,
            bytes_sent: bytes.len(),
        }))
    }

    fn read_lines_inner(&self, params: ReadLinesParams) -> Result<Json<LinesOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let log = connection.shared.lines.read();
        let total = log.len();
        let limit = params.limit.unwrap_or(100).min(1000);
        let start = total.saturating_sub(limit);
        let lines = log
            .lines()
            .skip(start)
            .map(|line| LineOut {
                t: line.t,
                direction: direction_id(line.direction),
                text: line.text.clone(),
            })
            .collect();
        Ok(Json(LinesOut { lines, total }))
    }

    fn read_terminal_inner(&self, params: ConnectionParams) -> Result<Json<TerminalOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let text = connection.shared.terminal.read().contents();
        Ok(Json(TerminalOut { text }))
    }

    async fn wait_for_inner(&self, params: WaitParams) -> Result<Json<WaitOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let start = self.line_count(&connection);
        let timeout = params.timeout_ms.unwrap_or(2000);
        Ok(Json(
            self.wait_from(&connection, start, &params.pattern, timeout)
                .await,
        ))
    }

    async fn send_and_wait_inner(
        &self,
        params: SendAndWaitParams,
    ) -> Result<Json<WaitOut>, String> {
        let connection = self.connection(params.connection_id)?;
        // Count first: the answer must not already be in the log when the wait starts.
        let start = self.line_count(&connection);
        let bytes = build_payload(
            params.text.as_deref(),
            params.hex.as_deref(),
            params.ending.as_deref(),
        )?;
        self.engine.send(connection.id, &bytes)?;
        let timeout = params.timeout_ms.unwrap_or(2000);
        Ok(Json(
            self.wait_from(&connection, start, &params.pattern, timeout)
                .await,
        ))
    }

    fn list_channels_inner(&self, params: ConnectionParams) -> Result<Json<ChannelsOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let data = connection.shared.channels.read();
        let channels = data
            .channels()
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let stats = channel.stats();
                ChannelOut {
                    index,
                    name: channel.name().to_owned(),
                    samples: stats.count,
                    last: stats.last,
                    min: stats.min,
                    max: stats.max,
                    mean: stats.mean,
                }
            })
            .collect();
        Ok(Json(ChannelsOut {
            channels,
            frames: data.frames(),
            sample_rate: data.axis().frame_rate(),
        }))
    }

    fn read_samples_inner(&self, params: ReadSamplesParams) -> Result<Json<SamplesOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let count = params.count.unwrap_or(1024).clamp(1, 16_384);
        let data = connection.shared.channels.read();
        let channel = data
            .channel(params.channel)
            .ok_or_else(|| format!("通道 {} 不存在", params.channel))?;
        let mut values = Vec::with_capacity(count);
        channel.latest(count, &mut values);
        let sample_rate = data.axis().frame_rate();
        Ok(Json(SamplesOut {
            channel: params.channel,
            sample_rate,
            values,
        }))
    }

    fn fft_inner(&self, params: FftParams) -> Result<Json<FftOut>, String> {
        let size = params.size.unwrap_or(2048).clamp(256, 16_384);
        let window = parse_window(params.window.as_deref())?;
        let connection = self.connection(params.connection_id)?;
        let (values, sample_rate) = {
            let data = connection.shared.channels.read();
            let channel = data
                .channel(params.channel)
                .ok_or_else(|| format!("通道 {} 不存在", params.channel))?;
            let mut values = Vec::with_capacity(size);
            channel.latest(size, &mut values);
            (values, data.axis().frame_rate())
        };
        if values.len() < size {
            return Err(format!("只有 {} 个样本，FFT 需要 {size} 个", values.len()));
        }
        let Some(sample_rate) = sample_rate else {
            return Err("采样率未知：至少需要两批样本".to_owned());
        };
        let mut analyzer = FftAnalyzer::new(size, window);
        let spectrum = analyzer
            .analyze(&values, sample_rate as f32)
            .ok_or_else(|| "FFT 计算失败".to_owned())?;
        let peak = spectrum.peaks.first().copied();
        let bins = spectrum
            .freqs
            .iter()
            .zip(&spectrum.magnitudes)
            .map(|(hz, magnitude)| BinOut {
                hz: *hz,
                magnitude: *magnitude,
            })
            .collect();
        Ok(Json(FftOut {
            sample_rate: spectrum.sample_rate,
            bins,
            peak_hz: peak.map(|peak| peak.freq),
            peak_magnitude: peak.map(|peak| peak.magnitude),
        }))
    }

    fn histogram_inner(&self, params: HistogramParams) -> Result<Json<HistogramOut>, String> {
        let bins = params.bins.unwrap_or(64).clamp(8, 1024);
        let count = params.count.unwrap_or(16_384).clamp(16, 65_536);
        let connection = self.connection(params.connection_id)?;
        let values = {
            let data = connection.shared.channels.read();
            let channel = data
                .channel(params.channel)
                .ok_or_else(|| format!("通道 {} 不存在", params.channel))?;
            let mut values = Vec::with_capacity(count);
            channel.latest(count, &mut values);
            values
        };
        let histogram =
            histogram(&values, bins, None).ok_or_else(|| "没有可统计的有限样本".to_owned())?;
        Ok(Json(HistogramOut {
            min: histogram.min,
            max: histogram.max,
            bin_width: histogram.bin_width(),
            counts: histogram.bins,
            samples: histogram.stats.count,
            mean: histogram.stats.mean,
            std_dev: histogram.stats.std_dev,
        }))
    }

    async fn screenshot_inner(&self) -> Result<Json<ScreenshotOut>, String> {
        let Some(port) = &self.screenshot else {
            return Err("没有窗口在运行；截图只在有界面的会话里可用".to_owned());
        };
        let (reply, answer) = oneshot::channel();
        if !port(reply) {
            return Err("窗口没有响应截图请求".to_owned());
        }
        let path = tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .map_err(|_| "等待截图超时".to_owned())?
            .map_err(|_| "窗口在截图前关闭了".to_owned())??;
        Ok(Json(ScreenshotOut { path }))
    }

    fn stats_inner(&self, params: ConnectionParams) -> Result<Json<StatsOut>, String> {
        let connection = self.connection(params.connection_id)?;
        let (rx_bytes, tx_bytes, frames, samples, dropped_bytes) =
            connection.shared.stats.summary();
        let (channels, sample_rate, channel_frames) = {
            let data = connection.shared.channels.read();
            (
                data.channels().len(),
                data.axis().frame_rate(),
                data.frames(),
            )
        };
        Ok(Json(StatsOut {
            connection_id: connection.id,
            rx_bytes,
            tx_bytes,
            frames,
            samples,
            channel_frames,
            dropped_bytes,
            channels,
            sample_rate,
        }))
    }
}

/// Which transport to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum TransportKind {
    #[default]
    Serial,
    TcpClient,
    TcpServer,
    Udp,
    #[serde(rename = "websocket", alias = "web-socket")]
    WebSocket,
    #[serde(rename = "mqtt")]
    Mqtt,
    /// The built-in 8-channel signal generator, for testing the tools without hardware.
    Simulator,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct OpenParams {
    /// One of serial, tcp-client, tcp-server, udp, websocket or simulator.
    transport: TransportKind,
    /// Serial port name, such as COM5 or /dev/ttyUSB0.
    port: Option<String>,
    /// Bits per second for a serial port; default 115200.
    baud: Option<u32>,
    /// Remote host for a TCP client.
    host: Option<String>,
    /// TCP port for a client or server.
    tcp_port: Option<u16>,
    /// Local address a TCP server or UDP socket binds; default 0.0.0.0.
    bind: Option<String>,
    /// Local port for a UDP socket.
    udp_port: Option<u16>,
    /// Where UDP sends go, as `host:port`; empty replies to the last sender.
    target: Option<String>,
    /// WebSocket address, `ws://` or `wss://`.
    url: Option<String>,
    /// MQTT username and password; both optional.
    username: Option<String>,
    password: Option<String>,
    /// MQTT topic filter to subscribe to and topic to publish on.
    subscribe: Option<String>,
    publish: Option<String>,
    /// MQTT QoS level, 0 to 2.
    qos: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
struct ConnectionParams {
    /// A connection id; the active connection when omitted.
    connection_id: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct SendParams {
    connection_id: Option<u64>,
    /// Text to send, encoded as UTF-8.
    text: Option<String>,
    /// Hex bytes, such as "01 02 0A"; takes precedence over text.
    hex: Option<String>,
    /// Line ending appended to text: none, cr, lf or crlf. Default none.
    ending: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct ReadLinesParams {
    connection_id: Option<u64>,
    /// How many of the newest lines to return, at most 1000; default 100.
    limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct WaitParams {
    connection_id: Option<u64>,
    /// Substring to look for in a new line.
    pattern: String,
    /// How long to wait, in milliseconds, at most 60000; default 2000.
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct SendAndWaitParams {
    connection_id: Option<u64>,
    text: Option<String>,
    hex: Option<String>,
    ending: Option<String>,
    pattern: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct ReadSamplesParams {
    connection_id: Option<u64>,
    channel: usize,
    /// How many of the newest samples to return, at most 16384; default 1024.
    count: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct FftParams {
    connection_id: Option<u64>,
    channel: usize,
    /// Transform length, a power of two from 256 to 16384; default 2048.
    size: Option<usize>,
    /// hann, hamming, blackman, flattop or rectangle; default hann.
    window: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
struct HistogramParams {
    connection_id: Option<u64>,
    channel: usize,
    /// Number of bins, 8 to 1024; default 64.
    bins: Option<usize>,
    /// How many of the newest samples to count, at most 65536; default 16384.
    count: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PortOut {
    name: String,
    details: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PortsOut {
    ports: Vec<PortOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct OpenedOut {
    id: u64,
    kind: String,
    describe: String,
    state: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ClosedOut {
    connection_id: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ConnectionOut {
    id: u64,
    kind: String,
    describe: String,
    state: String,
    active: bool,
    decoder: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ConnectionsOut {
    connections: Vec<ConnectionOut>,
    active_id: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SentOut {
    connection_id: u64,
    bytes_sent: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct LineOut {
    /// Seconds on the engine's clock.
    t: f64,
    /// rx or tx.
    direction: &'static str,
    text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LinesOut {
    lines: Vec<LineOut>,
    total: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TerminalOut {
    text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct WaitOut {
    found: bool,
    line: Option<LineOut>,
    /// Every line that arrived while waiting, joined with newlines.
    text_since: String,
    elapsed_ms: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChannelOut {
    index: usize,
    name: String,
    samples: u64,
    last: Option<f32>,
    min: Option<f32>,
    max: Option<f32>,
    mean: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ChannelsOut {
    channels: Vec<ChannelOut>,
    frames: u64,
    sample_rate: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SamplesOut {
    channel: usize,
    sample_rate: Option<f64>,
    values: Vec<f32>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct BinOut {
    hz: f32,
    magnitude: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
struct FftOut {
    sample_rate: f32,
    bins: Vec<BinOut>,
    peak_hz: Option<f32>,
    peak_magnitude: Option<f32>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct HistogramOut {
    min: f32,
    max: f32,
    bin_width: f32,
    counts: Vec<u32>,
    samples: usize,
    mean: f64,
    std_dev: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ScreenshotOut {
    /// Absolute path of the saved PNG.
    path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatsOut {
    connection_id: u64,
    rx_bytes: u64,
    tx_bytes: u64,
    frames: u64,
    samples: u64,
    /// Frames in the channel store; the simulator fills these without any byte traffic.
    channel_frames: u64,
    dropped_bytes: u64,
    channels: usize,
    sample_rate: Option<f64>,
}

fn state_text(state: &ConnectionState) -> String {
    match state {
        ConnectionState::Connecting => "connecting".to_owned(),
        ConnectionState::Open => "open".to_owned(),
        ConnectionState::Closed => "closed".to_owned(),
        ConnectionState::Failed(error) => format!("failed: {error}"),
    }
}

fn direction_id(direction: Direction) -> &'static str {
    match direction {
        Direction::RemoteToHere => "rx",
        Direction::HereToRemote => "tx",
    }
}

fn lines_since(
    connection: &Connection,
    start: usize,
    pattern: &str,
) -> (Vec<LineOut>, Option<LineOut>) {
    let log = connection.shared.lines.read();
    let mut all = Vec::new();
    let mut hit = None;
    for line in log.lines().skip(start) {
        let out = LineOut {
            t: line.t,
            direction: direction_id(line.direction),
            text: line.text.clone(),
        };
        if hit.is_none() && line.text.contains(pattern) {
            hit = Some(out.clone());
        }
        all.push(out);
    }
    (all, hit)
}

fn join_lines(lines: &[LineOut]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bytes a send stands for. Hex wins over text; text may carry a line ending.
fn build_payload(
    text: Option<&str>,
    hex: Option<&str>,
    ending: Option<&str>,
) -> Result<Vec<u8>, String> {
    if let Some(hex) = hex {
        return parse_hex(hex);
    }
    let text = text.ok_or_else(|| "需要 text 或 hex".to_owned())?;
    let ending: &[u8] = match ending {
        None | Some("none") => b"",
        Some("cr") => b"\r",
        Some("lf") => b"\n",
        Some("crlf") => b"\r\n",
        Some(other) => return Err(format!("未知换行方式 {other}：用 none、cr、lf 或 crlf")),
    };
    let mut bytes = text.as_bytes().to_vec();
    bytes.extend_from_slice(ending);
    Ok(bytes)
}

/// Parses `01 02 0A`, `0x01,0x02` or `01020A` alike.
fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',' && *character != ':')
        .collect();
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(format!("HEX 输入有 {} 个字符，凑不成整字节", cleaned.len()));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&cleaned[index..index + 2], 16)
                .map_err(|_| format!("不是有效的 HEX：{}", &cleaned[index..index + 2]))
        })
        .collect()
}

fn parse_window(text: Option<&str>) -> Result<Window, String> {
    Ok(match text.unwrap_or("hann") {
        "rectangle" => Window::Rectangle,
        "hann" => Window::Hann,
        "hamming" => Window::Hamming,
        "blackman" => Window::Blackman,
        "flattop" => Window::FlatTop,
        other => {
            return Err(format!(
                "未知窗函数 {other}：用 rectangle、hann、hamming、blackman 或 flattop"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::pipeline::DecoderSpec;

    #[tokio::test]
    async fn the_simulator_flows_through_the_tools() {
        let engine = Arc::new(Engine::new(tokio::runtime::Handle::current(), 1 << 16));
        let service = McpService::new(Arc::clone(&engine));

        let opened = service
            .open_inner(OpenParams {
                transport: TransportKind::Simulator,
                ..OpenParams::default()
            })
            .unwrap()
            .0;
        assert_eq!(opened.kind, "模拟信号源");

        let mut ready = false;
        for _ in 0..300 {
            // The guard is scoped away before the await: clippy is right to insist on it.
            let ready_now = {
                let shared = engine.active_shared();
                let data = shared.channels.read();
                data.frames() > 8192 && data.axis().frame_rate().is_some()
            };
            if ready_now {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready, "the simulator should produce frames");

        let channels = service
            .list_channels_inner(ConnectionParams::default())
            .unwrap()
            .0;
        assert_eq!(channels.channels.len(), 8);
        assert!(channels.sample_rate.unwrap_or(0.0) > 0.0);

        let samples = service
            .read_samples_inner(ReadSamplesParams {
                channel: 0,
                count: Some(100),
                ..ReadSamplesParams::default()
            })
            .unwrap()
            .0;
        assert_eq!(samples.values.len(), 100);

        let fft = service
            .fft_inner(FftParams {
                channel: 0,
                size: Some(4096),
                ..FftParams::default()
            })
            .unwrap()
            .0;
        let peak = fft.peak_hz.expect("a sine has a peak");
        assert!((peak - 1000.0).abs() < 30.0, "peak at {peak} Hz");

        let histogram = service
            .histogram_inner(HistogramParams {
                channel: 0,
                ..HistogramParams::default()
            })
            .unwrap()
            .0;
        assert_eq!(histogram.counts.len(), 64);
        assert_eq!(histogram.samples, 16_384);

        let stats = service.stats_inner(ConnectionParams::default()).unwrap().0;
        assert_eq!(stats.channels, 8);
    }

    #[tokio::test]
    async fn waits_time_out_when_nothing_matches() {
        let engine = Arc::new(Engine::new(tokio::runtime::Handle::current(), 1 << 16));
        let service = McpService::new(engine);
        let result = service
            .wait_for_inner(WaitParams {
                pattern: "never".to_owned(),
                timeout_ms: Some(30),
                ..WaitParams::default()
            })
            .await;
        // No connection is open, so the wait cannot even start.
        assert!(result.is_err());
    }

    #[test]
    fn a_bad_hex_payload_is_refused() {
        assert!(build_payload(None, Some("012"), None).is_err());
        assert_eq!(
            build_payload(Some("AT"), None, Some("crlf")).unwrap(),
            b"AT\r\n".to_vec()
        );
        assert_eq!(
            build_payload(None, Some("01 02"), None).unwrap(),
            vec![1, 2]
        );
    }

    #[test]
    fn the_websocket_transport_name_has_no_dash() {
        let params: OpenParams = serde_json::from_str(r#"{"transport":"websocket"}"#).unwrap();
        assert!(matches!(params.transport, TransportKind::WebSocket));
    }

    #[tokio::test]
    async fn websocket_flows_through_the_tools() {
        use futures_util::{SinkExt, StreamExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                    let reply = format!("echo:{text}");
                    let _ = socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(reply.into()))
                        .await;
                }
            }
        });

        let engine = Arc::new(Engine::new(tokio::runtime::Handle::current(), 1 << 16));
        let service = McpService::new(Arc::clone(&engine));
        let opened = service
            .open_inner(OpenParams {
                transport: TransportKind::WebSocket,
                url: Some(format!("ws://{addr}/")),
                ..OpenParams::default()
            })
            .unwrap()
            .0;
        assert_eq!(opened.kind, "WebSocket");

        let mut open = false;
        for _ in 0..100 {
            if engine
                .connection(opened.id)
                .is_some_and(|connection| connection.state().is_open())
            {
                open = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(open, "the websocket should connect");

        service
            .send_inner(SendParams {
                connection_id: Some(opened.id),
                text: Some("hello".to_owned()),
                ..SendParams::default()
            })
            .unwrap();

        let mut heard = None;
        for _ in 0..50 {
            let lines = service
                .read_lines_inner(ReadLinesParams {
                    connection_id: Some(opened.id),
                    limit: Some(5),
                })
                .unwrap()
                .0;
            // The sent text is logged as a tx line too; wait for the echoed reply.
            if let Some(line) = lines.lines.iter().find(|line| line.text == "echo:hello") {
                heard = Some(line.text.clone());
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(heard.as_deref(), Some("echo:hello"));
    }

    #[test]
    fn every_decoder_has_a_stable_id() {
        assert_eq!(DecoderSpec::Line.id(), "line");
        assert_eq!(DecoderSpec::JustFloat.id(), "justfloat");
        assert_eq!(DecoderSpec::JustInt.id(), "justint");
    }
}
