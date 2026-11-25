//! The engine: owns the connections and runs them on a Tokio runtime, away from any UI thread.
//!
//! Every client (GUI, MCP server, CLI) goes through [`Engine`], and none of them needs a window.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, watch};

use crate::api::Event;
use crate::clock::Clock;
use crate::pipeline::{DataSession, DecoderSpec, SessionConfig, SessionShared};
use crate::transport::{
    SIM_CHANNEL_NAMES, SIM_CHANNELS, SerialConfig, SerialEvent, Simulator, SimulatorConfig,
    TcpClientConfig, TcpEvent, TcpServerConfig, serial, tcp,
};

/// Identifies a connection for as long as the program runs.
pub type ConnectionId = u64;

/// How many events a slow client may lag behind before it starts missing some.
const EVENT_CAPACITY: usize = 256;

/// Frames stored per lock acquisition. Bounds how long a reader, such as the UI thread, has to wait.
const PUSH_CHUNK_FRAMES: usize = 2048;

/// A connection that received data this recently counts as streaming.
const STREAMING_WINDOW: Duration = Duration::from_millis(500);

/// How often an open connection checks for a partial frame that will never finish.
const IDLE_FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// What a connection talks to.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportConfig {
    Simulator(SimulatorConfig),
    Serial(SerialConfig),
    TcpClient(TcpClientConfig),
    TcpServer(TcpServerConfig),
}

impl TransportConfig {
    /// The name in the connection list.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Simulator(_) => "模拟信号源",
            Self::Serial(_) => "串口",
            Self::TcpClient(_) => "TCP 客户端",
            Self::TcpServer(_) => "TCP 服务器",
        }
    }

    /// The one line after the name.
    pub fn describe(&self) -> String {
        match self {
            Self::Simulator(config) => {
                format!("8 通道 · {} 样本/s 每通道", si_rate(config.sample_rate))
            }
            Self::Serial(config) => config.describe(),
            Self::TcpClient(config) => format!("{}:{}", config.host, config.port),
            Self::TcpServer(config) => format!("{}:{}", config.bind, config.port),
        }
    }

    /// The protocol a fresh connection starts with.
    pub fn default_decoder(&self) -> DecoderSpec {
        match self {
            Self::Simulator(_) => DecoderSpec::JustFloat,
            _ => DecoderSpec::Line,
        }
    }
}

/// Where a connection stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Open,
    Closed,
    Failed(String),
}

impl ConnectionState {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Connecting => "连接中".to_owned(),
            Self::Open => "已打开".to_owned(),
            Self::Closed => "已关闭".to_owned(),
            Self::Failed(error) => format!("失败：{error}"),
        }
    }
}

/// One connection and everything the UI reads from it.
pub struct Connection {
    pub id: ConnectionId,
    pub config: TransportConfig,
    pub shared: Arc<SessionShared>,
    state: RwLock<ConnectionState>,
    /// Shared with the reading task, which checks it on every chunk.
    decoder: Arc<RwLock<DecoderSpec>>,
    writer: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    /// How to stop the transport; taken by [`Connection::stop`].
    closer: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    last_rx: RwLock<Option<Instant>>,
    /// Clients a TCP server currently has.
    clients: RwLock<usize>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("config", &self.config)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Connection {
    fn new(id: ConnectionId, config: TransportConfig, shared: Arc<SessionShared>) -> Self {
        let decoder = config.default_decoder();
        Self {
            id,
            config,
            shared,
            state: RwLock::new(ConnectionState::Connecting),
            decoder: Arc::new(RwLock::new(decoder)),
            writer: Mutex::new(None),
            closer: Mutex::new(None),
            last_rx: RwLock::new(None),
            clients: RwLock::new(0),
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state.read().clone()
    }

    fn set_state(&self, state: ConnectionState) {
        *self.state.write() = state;
    }

    pub fn decoder(&self) -> DecoderSpec {
        *self.decoder.read()
    }

    /// Changes the protocol; an open transport picks it up on its next chunk.
    pub fn set_decoder(&self, spec: DecoderSpec) {
        *self.decoder.write() = spec;
    }

    pub fn clients(&self) -> usize {
        *self.clients.read()
    }

    /// Whether data arrived within the last moment.
    pub fn is_streaming(&self) -> bool {
        self.last_rx
            .read()
            .is_some_and(|at| at.elapsed() < STREAMING_WINDOW)
    }

    fn mark_rx(&self) {
        *self.last_rx.write() = Some(Instant::now());
    }

    fn set_writer(&self, writer: mpsc::UnboundedSender<Vec<u8>>) {
        *self.writer.lock() = Some(writer);
    }

    fn set_closer(&self, closer: Box<dyn FnOnce() + Send>) {
        *self.closer.lock() = Some(closer);
    }

    /// Stops the transport. Its own end event then reports the final state.
    pub fn stop(&self) {
        *self.writer.lock() = None;
        if let Some(closer) = self.closer.lock().take() {
            closer();
        }
        if matches!(
            self.state(),
            ConnectionState::Connecting | ConnectionState::Open
        ) {
            self.set_state(ConnectionState::Closed);
        }
    }

    /// Sends bytes and records them for the terminal and hex views.
    pub fn send(&self, clock: Clock, bytes: &[u8]) -> Result<(), String> {
        let writer = self.writer.lock().clone();
        let Some(writer) = writer else {
            return Err(format!("{} 现在不能发送数据", self.config.kind()));
        };
        writer
            .send(bytes.to_vec())
            .map_err(|_| "连接已关闭".to_owned())?;
        DataSession::log_sent(clock.now(), bytes, &self.shared);
        Ok(())
    }
}

pub struct Engine {
    rt: Handle,
    clock: Clock,
    capacity: usize,
    connections: RwLock<BTreeMap<ConnectionId, Arc<Connection>>>,
    next_id: AtomicU64,
    active: RwLock<Option<ConnectionId>>,
    /// Stands in for the active connection while there is none.
    empty: Arc<SessionShared>,
    events: broadcast::Sender<Event>,
    version: Arc<AtomicU64>,
    simulator_open: Arc<AtomicBool>,
}

impl Engine {
    /// Creates an engine whose tasks run on `rt`, retaining `capacity` samples per channel.
    pub fn new(rt: Handle, capacity: usize) -> Self {
        let capacity = capacity.max(1024);
        Self {
            rt,
            clock: Clock::new(),
            capacity,
            connections: RwLock::new(BTreeMap::new()),
            next_id: AtomicU64::new(0),
            active: RwLock::new(None),
            empty: Arc::new(SessionShared::with_capacity(capacity)),
            events: broadcast::channel(EVENT_CAPACITY).0,
            version: Arc::new(AtomicU64::new(0)),
            simulator_open: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// A handle to the runtime the transports run on, for work that belongs beside them.
    pub fn runtime(&self) -> Handle {
        self.rt.clone()
    }

    /// Increments whenever any connection stores new samples or lines.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// The connection the views show.
    pub fn active(&self) -> Option<Arc<Connection>> {
        let id = *self.active.read();
        id.and_then(|id| self.connection(id))
    }

    pub fn active_id(&self) -> Option<ConnectionId> {
        *self.active.read()
    }

    /// The active connection's data, or a stand-in with no channels and no lines.
    pub fn active_shared(&self) -> Arc<SessionShared> {
        self.active().map_or_else(
            || Arc::clone(&self.empty),
            |connection| Arc::clone(&connection.shared),
        )
    }

    pub fn set_active(&self, id: ConnectionId) {
        if self.connections.read().contains_key(&id) {
            *self.active.write() = Some(id);
        }
    }

    /// Every connection, oldest first.
    pub fn connections(&self) -> Vec<Arc<Connection>> {
        self.connections.read().values().cloned().collect()
    }

    pub fn connection(&self, id: ConnectionId) -> Option<Arc<Connection>> {
        self.connections.read().get(&id).cloned()
    }

    /// Opens a connection and makes it the active one. A serial port that will not open is
    /// reported right here; TCP failures arrive later as events and a state change.
    pub fn open(&self, config: TransportConfig) -> Result<ConnectionId, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let shared = Arc::new(SessionShared::with_capacity(self.capacity));
        let connection = Arc::new(Connection::new(id, config.clone(), shared));

        let opens_at_once = match &config {
            TransportConfig::Simulator(simulator) => {
                self.spawn_simulator(&connection, *simulator);
                true
            }
            TransportConfig::Serial(serial_config) => {
                self.spawn_serial(&connection, serial_config.clone())?;
                true
            }
            TransportConfig::TcpClient(tcp_config) => {
                self.spawn_tcp_client(&connection, tcp_config.clone());
                false
            }
            TransportConfig::TcpServer(tcp_config) => {
                self.spawn_tcp_server(&connection, tcp_config.clone());
                false
            }
        };
        if opens_at_once {
            connection.set_state(ConnectionState::Open);
        }

        self.connections.write().insert(id, Arc::clone(&connection));
        *self.active.write() = Some(id);
        self.emit(Event::Info(format!(
            "{} 已打开：{}",
            config.kind(),
            config.describe()
        )));
        Ok(id)
    }

    /// Stops a connection. It stays in the list, so its data can still be read.
    pub fn close(&self, id: ConnectionId) {
        if let Some(connection) = self.connection(id) {
            connection.stop();
            self.emit(Event::Info(format!("{} 已关闭", connection.config.kind())));
        }
    }

    /// Forgets everything a connection collected.
    pub fn clear(&self, id: ConnectionId) {
        if let Some(connection) = self.connection(id) {
            connection.shared.clear();
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn send(&self, id: ConnectionId, bytes: &[u8]) -> Result<(), String> {
        let Some(connection) = self.connection(id) else {
            return Err("连接不存在".to_owned());
        };
        connection.send(self.clock, bytes)
    }

    /// Whether any connection received data within the last moment.
    pub fn is_streaming(&self) -> bool {
        self.connections
            .read()
            .values()
            .any(|connection| connection.is_streaming())
    }

    pub fn simulator_running(&self) -> bool {
        self.simulator_open.load(Ordering::Acquire)
    }

    /// Starts the built-in signal generator, replacing a running one.
    pub fn start_simulator(&self, config: SimulatorConfig) {
        self.stop_simulator();
        match self.open(TransportConfig::Simulator(config)) {
            Ok(_) => {
                self.simulator_open.store(true, Ordering::Release);
                self.emit(Event::Simulator { running: true });
            }
            Err(error) => self.emit(Event::Error(error)),
        }
    }

    pub fn stop_simulator(&self) {
        let simulator = self
            .connections
            .read()
            .values()
            .find(|connection| matches!(connection.config, TransportConfig::Simulator(_)))
            .cloned();
        if let Some(connection) = simulator {
            connection.stop();
            self.simulator_open.store(false, Ordering::Release);
            self.emit(Event::Simulator { running: false });
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn spawn_simulator(&self, connection: &Arc<Connection>, config: SimulatorConfig) {
        let (stop_tx, mut stop_rx) = watch::channel(false);
        connection.set_closer(Box::new(move || {
            let _ = stop_tx.send(true);
        }));

        let shared = Arc::clone(&connection.shared);
        let connection = Arc::clone(connection);
        let version = Arc::clone(&self.version);
        let clock = self.clock;
        self.rt.spawn(async move {
            let (sample_rate, batch_hz) = (config.sample_rate, config.batch_hz);
            let frames_per_batch = sample_rate / batch_hz;
            let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / batch_hz));
            // If a tick is late, skip ahead instead of catching up with a burst.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut simulator = Simulator::new(config);
            let started_at = clock.now();
            let mut credit = 0.0f64;
            let mut buffer: Vec<f32> = Vec::new();
            let mut named = false;

            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    _ = ticker.tick() => {}
                }
                credit += frames_per_batch;
                let frames = credit.floor() as usize;
                credit -= frames as f64;
                if frames == 0 {
                    continue;
                }
                buffer.clear();
                simulator.generate(frames, &mut buffer);
                // Ideal time: a source with a known sample rate is timed by its sample index, not
                // by when the OS got around to delivering the batch.
                let t = started_at + simulator.frames() as f64 / sample_rate;
                // One batch, declared whole and filled in bounded pieces: the 20 000 frames of a
                // batch share one arrival time and one timestamp entry, and no single lock hold
                // stalls the UI.
                let frames = buffer.len() / SIM_CHANNELS;
                shared
                    .channels
                    .write()
                    .begin_batch(t, SIM_CHANNELS, frames as u64);
                for chunk in buffer.chunks(SIM_CHANNELS * PUSH_CHUNK_FRAMES) {
                    shared.channels.write().push_chunk(SIM_CHANNELS, chunk);
                }
                if !named {
                    shared
                        .channels
                        .write()
                        .set_default_names(&SIM_CHANNEL_NAMES);
                    named = true;
                }
                version.fetch_add(1, Ordering::Release);
                connection.mark_rx();
            }
        });
    }

    fn spawn_serial(
        &self,
        connection: &Arc<Connection>,
        config: SerialConfig,
    ) -> Result<(), String> {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<SerialEvent>();
        let mut link = serial::open(&config, events_tx, self.clock)?;
        connection.set_writer(link.writes.clone());
        connection.set_closer(Box::new(move || link.stop()));

        let shared = Arc::clone(&connection.shared);
        let spec = Arc::clone(&connection.decoder);
        let clock = self.clock;
        let version = Arc::clone(&self.version);
        let events = self.events.clone();
        let connection = Arc::clone(connection);
        self.rt.spawn(async move {
            let mut session = DataSession::new(SessionConfig {
                decoder: *spec.read(),
            });
            run_receiver(
                &mut session,
                &spec,
                &connection,
                &shared,
                clock,
                &version,
                &events,
                &mut events_rx,
                |event| match event {
                    SerialEvent::Data { t, bytes } => Ok((t, bytes)),
                    SerialEvent::Ended(reason) => Err(reason),
                },
            )
            .await;
        });
        Ok(())
    }

    fn spawn_tcp_client(&self, connection: &Arc<Connection>, config: TcpClientConfig) {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<TcpEvent>();
        let mut link = tcp::spawn_client(&config, events_tx, self.clock);
        connection.set_writer(link.writes.clone());
        connection.set_closer(Box::new(move || link.stop()));

        let shared = Arc::clone(&connection.shared);
        let spec = Arc::clone(&connection.decoder);
        let clock = self.clock;
        let version = Arc::clone(&self.version);
        let events = self.events.clone();
        let connection = Arc::clone(connection);
        self.rt.spawn(async move {
            let mut session = DataSession::new(SessionConfig {
                decoder: *spec.read(),
            });
            let mut flush = tokio::time::interval(IDLE_FLUSH_INTERVAL);
            let mut idle_watch: Option<(usize, Instant)> = None;
            loop {
                tokio::select! {
                    _ = flush.tick() => {
                        let pending = session.pending();
                        match idle_watch {
                            Some((count, since)) if count == pending && since.elapsed() >= IDLE_FLUSH_INTERVAL => {
                                session.on_idle(clock.now(), &shared);
                                version.fetch_add(1, Ordering::Release);
                                idle_watch = None;
                            }
                            _ => idle_watch = Some((pending, Instant::now())),
                        }
                    }
                    event = events_rx.recv() => match event {
                        Some(TcpEvent::Data { t, bytes }) => {
                            let current = *spec.read();
                            if current != session.spec() {
                                session.reconfigure(SessionConfig { decoder: current }, &shared);
                            }
                            session.on_bytes(t, &bytes, &shared);
                            version.fetch_add(1, Ordering::Release);
                            connection.mark_rx();
                        }
                        Some(TcpEvent::Info(text)) => {
                            if text.starts_with("已连接") {
                                connection.set_state(ConnectionState::Open);
                            }
                            let _ = events.send(Event::Info(format!(
                                "{}：{text}",
                                connection.config.kind()
                            )));
                        }
                        Some(TcpEvent::Clients(_)) => {}
                        Some(TcpEvent::Ended(reason)) => {
                            finish(&connection, reason, &events);
                            break;
                        }
                        None => break,
                    },
                }
            }
        });
    }

    fn spawn_tcp_server(&self, connection: &Arc<Connection>, config: TcpServerConfig) {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<TcpEvent>();

        // Each client gets its own session, so one client's partial frames cannot mix with another's.
        let spec = Arc::clone(&connection.decoder);
        let shared_for_sinks = Arc::clone(&connection.shared);
        let clock = self.clock;
        let version_for_sinks = Arc::clone(&self.version);
        let connection_for_sinks = Arc::clone(connection);
        let make_sink: tcp::ClientSinkFactory = Box::new(move |_client| {
            let mut session = DataSession::new(SessionConfig {
                decoder: *spec.read(),
            });
            let spec = Arc::clone(&spec);
            let shared = Arc::clone(&shared_for_sinks);
            let version = Arc::clone(&version_for_sinks);
            let connection = Arc::clone(&connection_for_sinks);
            Box::new(move |bytes: &[u8]| {
                let current = *spec.read();
                if current != session.spec() {
                    session.reconfigure(SessionConfig { decoder: current }, &shared);
                }
                session.on_bytes(clock.now(), bytes, &shared);
                version.fetch_add(1, Ordering::Release);
                connection.mark_rx();
            })
        });

        let mut link = tcp::spawn_server(&config, events_tx, self.clock, make_sink);
        connection.set_writer(link.writes.clone());
        connection.set_closer(Box::new(move || link.stop()));

        let events = self.events.clone();
        let connection = Arc::clone(connection);
        self.rt.spawn(async move {
            while let Some(event) = events_rx.recv().await {
                match event {
                    TcpEvent::Info(text) => {
                        if text.starts_with("正在监听") {
                            connection.set_state(ConnectionState::Open);
                        }
                        let _ = events
                            .send(Event::Info(format!("{}：{text}", connection.config.kind())));
                    }
                    TcpEvent::Clients(count) => *connection.clients.write() = count,
                    TcpEvent::Data { .. } => {}
                    TcpEvent::Ended(reason) => {
                        finish(&connection, reason, &events);
                        break;
                    }
                }
            }
        });
    }
}

/// The read loop shared by the serial and TCP client transports: data into the session, an idle
/// flush for part of a frame that never completes, and the end of the transport.
#[allow(clippy::too_many_arguments)]
async fn run_receiver<E>(
    session: &mut DataSession,
    spec: &RwLock<DecoderSpec>,
    connection: &Connection,
    shared: &SessionShared,
    clock: Clock,
    version: &AtomicU64,
    events: &broadcast::Sender<Event>,
    events_rx: &mut mpsc::UnboundedReceiver<E>,
    mut classify: impl FnMut(E) -> Result<(f64, Vec<u8>), Option<String>>,
) {
    let mut flush = tokio::time::interval(IDLE_FLUSH_INTERVAL);
    let mut idle_watch: Option<(usize, Instant)> = None;

    loop {
        tokio::select! {
            _ = flush.tick() => {
                // A line that has stopped growing will not get its line break: flush it.
                let pending = session.pending();
                match idle_watch {
                    Some((count, since)) if count == pending && since.elapsed() >= IDLE_FLUSH_INTERVAL => {
                        session.on_idle(clock.now(), shared);
                        version.fetch_add(1, Ordering::Release);
                        idle_watch = None;
                    }
                    _ => idle_watch = Some((pending, Instant::now())),
                }
            }
            event = events_rx.recv() => {
                let Some(event) = event else { break };
                match classify(event) {
                    Ok((t, bytes)) => {
                        let current = *spec.read();
                        if current != session.spec() {
                            session.reconfigure(SessionConfig { decoder: current }, shared);
                        }
                        session.on_bytes(t, &bytes, shared);
                        version.fetch_add(1, Ordering::Release);
                        connection.mark_rx();
                    }
                    Err(None) => {}
                    Err(Some(reason)) => {
                        finish(connection, Some(reason), events);
                        break;
                    }
                }
            }
        }
    }
}

/// A rate with an SI prefix, so 1 000 000 reads as 1.000 M.
fn si_rate(value: f64) -> String {
    for (prefix, scale) in [("G", 1e9), ("M", 1e6), ("k", 1e3), ("", 1.0)] {
        if value >= scale * 0.9995 {
            return format!("{:.3} {prefix}", value / scale);
        }
    }
    format!("{value:.3}")
}

/// Records how a transport ended, and says so in the log.
fn finish(connection: &Connection, reason: Option<String>, events: &broadcast::Sender<Event>) {
    let state = reason.map_or(ConnectionState::Closed, ConnectionState::Failed);
    connection.set_state(state.clone());
    let _ = events.send(match state {
        ConnectionState::Failed(error) => {
            Event::Error(format!("{}：{error}", connection.config.kind()))
        }
        _ => Event::Info(format!("{} 已断开", connection.config.kind())),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::decoder::JUST_FLOAT_TAIL;
    use tokio::io::AsyncWriteExt;

    const CAPACITY: usize = 1 << 16;

    fn open_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Drains events already sent until one satisfies `wanted`.
    fn drain_until(
        events: &mut broadcast::Receiver<Event>,
        wanted: impl Fn(&Event) -> bool,
    ) -> bool {
        loop {
            match events.try_recv() {
                Ok(event) => {
                    if wanted(&event) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    }

    /// Waits for a condition, running the runtime meanwhile.
    async fn eventually(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..300 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[test]
    fn simulator_streams_frames_into_the_channels() {
        open_runtime().block_on(async {
            let engine = Engine::new(Handle::current(), CAPACITY);
            let mut events = engine.subscribe();
            engine.start_simulator(SimulatorConfig {
                sample_rate: 20_000.0,
                batch_hz: 100.0,
            });

            // Ten batches in, the measured rate is meaningful (it needs two batches).
            assert!(eventually(|| engine.active_shared().channels.read().frames() > 2_000).await);
            // Opening the connection logs a line first; the announcement follows it.
            assert!(drain_until(&mut events, |event| {
                matches!(event, Event::Simulator { running: true })
            }));
            assert!(engine.simulator_running());
            assert!(engine.is_streaming());

            let shared = engine.active_shared();
            {
                let data = shared.channels.read();
                assert_eq!(data.channels().len(), SIM_CHANNELS);
                assert_eq!(data.channel(0).unwrap().name(), SIM_CHANNEL_NAMES[0]);
                assert!(data.channel(6).unwrap().stats().mean > 3.0);
                let rate = data.axis().frame_rate().unwrap();
                assert!((rate - 20_000.0).abs() < 1.0, "frame rate {rate}");
            }
            assert!(engine.version() > 0);

            engine.stop_simulator();
            assert!(drain_until(&mut events, |event| {
                matches!(event, Event::Simulator { running: false })
            }));
            assert!(!engine.simulator_running());
            // Stopping is cooperative: a batch already being generated still lands. Let it.
            tokio::time::sleep(Duration::from_millis(40)).await;
            let frames = engine.active_shared().channels.read().frames();
            tokio::time::sleep(Duration::from_millis(60)).await;
            assert_eq!(engine.active_shared().channels.read().frames(), frames);
        });
    }

    #[test]
    fn restarting_the_simulator_close_the_old_connection() {
        open_runtime().block_on(async {
            let engine = Engine::new(Handle::current(), CAPACITY);
            let slow = SimulatorConfig {
                sample_rate: 1_000.0,
                batch_hz: 100.0,
            };
            engine.start_simulator(slow);
            let first = engine.connections()[0].clone();
            engine.start_simulator(slow);
            let connections = engine.connections();
            assert_eq!(connections.len(), 2);
            assert_eq!(first.state(), ConnectionState::Closed);
            assert_eq!(connections[1].state(), ConnectionState::Open);
            assert_eq!(engine.active_id(), Some(connections[1].id));
            assert!(engine.simulator_running());
        });
    }

    #[test]
    fn a_tcp_server_feeds_frames_from_every_client_into_the_channels() {
        open_runtime().block_on(async {
            let engine = Engine::new(Handle::current(), CAPACITY);
            let id = engine
                .open(TransportConfig::TcpServer(TcpServerConfig {
                    bind: "127.0.0.1".to_owned(),
                    port: 0,
                }))
                .unwrap();

            // Learn the port the system picked from the log.
            let mut events = engine.subscribe();
            let mut port = None;
            while let Ok(event) = events.try_recv() {
                port = port.or_else(|| parse_port(&event));
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while port.is_none() && Instant::now() < deadline {
                if let Ok(event) = events.try_recv() {
                    port = parse_port(&event);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let port = port.expect("the server never reported a port");

            // Plot JustFloat instead of lines, and wait for the listener.
            let connection = engine.connection(id).unwrap();
            connection.set_decoder(DecoderSpec::JustFloat);
            assert!(eventually(|| connection.state().is_open()).await);

            // Two clients, each sending whole frames: the channel set receives them both.
            for _ in 0..2 {
                let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                let mut frame = Vec::new();
                for value in [1.0f32, 2.0, 3.0] {
                    frame.extend_from_slice(&value.to_le_bytes());
                }
                frame.extend_from_slice(&JUST_FLOAT_TAIL);
                client.write_all(&frame).await.unwrap();
                client.write_all(&frame).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            assert!(eventually(|| connection.shared.channels.read().frames() >= 4).await);
            assert_eq!(connection.shared.channels.read().channels().len(), 3);
            engine.close(id);
            assert_eq!(connection.state(), ConnectionState::Closed);
        });
    }

    #[test]
    fn what_the_ui_sends_reaches_the_peer() {
        open_runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let (received_tx, received_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 64];
                let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                    .await
                    .unwrap();
                let _ = received_tx.send(buffer[..n].to_vec());
            });

            let engine = Engine::new(Handle::current(), CAPACITY);
            let id = engine
                .open(TransportConfig::TcpClient(TcpClientConfig {
                    host: "127.0.0.1".to_owned(),
                    port,
                    connect_timeout_ms: 2000,
                }))
                .unwrap();
            let connection = engine.connection(id).unwrap();
            assert!(eventually(|| connection.state().is_open()).await);

            engine.send(id, b"ping").unwrap();
            let received = tokio::time::timeout(Duration::from_secs(2), received_rx)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received, b"ping");
            // The terminal shows what we sent.
            let lines = connection.shared.lines.read();
            let last = lines.lines().last().unwrap();
            assert_eq!(last.text, "ping");
            engine.close(id);
        });
    }

    /// Pulls `127.0.0.1:PORT` out of a "正在监听" line.
    fn parse_port(event: &Event) -> Option<u16> {
        match event {
            Event::Info(text) if text.contains("正在监听") => text
                .split(':')
                .next_back()
                .and_then(|port| port.parse::<u16>().ok()),
            _ => None,
        }
    }
}
