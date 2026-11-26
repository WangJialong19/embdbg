//! The window: tool bar, dock area and status bar, all driven by the engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::glow::HasContext as _;
use egui::{Button, Frame, Layout, Margin, RichText, Sense, Ui, vec2};
use embdbg_core::api::Event;
use embdbg_core::engine::{Engine, TransportConfig};
use embdbg_core::pipeline::DecoderSpec;
use embdbg_core::transport::SerialConfig;
use tokio::sync::broadcast;

use crate::fonts;
use crate::fonts::icon_label;
use crate::icons::outline;
use crate::panels;
use crate::panels::ai::AiState;
use crate::panels::connections::ConnectionsState;
use crate::panels::hex::HexState;
use crate::panels::histogram::HistogramState;
use crate::panels::inspector::ChannelUi;
use crate::panels::log::LogBuffer;
use crate::panels::net_tools::NetToolsState;
use crate::panels::send::SendState;
use crate::panels::spectrum::SpectrumState;
use crate::panels::stack::{StackState, StackTool};
use crate::panels::terminal::TerminalState;
use crate::panels::xy::XyState;
use crate::plots::wave::{WaveScratch, WaveView};
use crate::settings::{RendererChoice, Settings};
use crate::theme;
use crate::util::{format_count, format_rate};

const TOOLBAR_HEIGHT: f32 = 38.0;
const MENUBAR_HEIGHT: f32 = 26.0;
const STATUSBAR_HEIGHT: f32 = 24.0;

/// What the rest of the window keeps when the channel table is split off to the right; the table
/// takes the remainder. `split_right` gives the fraction to the old, left-hand side.
const INSPECTOR_SPLIT: f32 = 0.56;
/// What the main views keep when the tool stack is split off to the right.
const TOOLSTACK_SPLIT: f32 = 0.5;

/// Share of the window width the left sidebar starts with.
const SIDEBAR_SPLIT: f32 = 0.18;
/// The sidebar may be dragged between these widths, in points. Smaller is hidden, not narrow.
const SIDEBAR_MIN: f32 = 200.0;
const SIDEBAR_MAX: f32 = 400.0;

/// How often the window repaints when nothing streams, just to notice state changes.
const IDLE_INTERVAL: Duration = Duration::from_millis(250);

/// How often the dock layout is compared with the saved file, so dragging a divider does not
/// rewrite it on every frame.
const LAYOUT_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Frames the performance test discards first, while fonts, textures and shaders warm up.
const PERF_WARMUP_SECONDS: f64 = 2.0;

/// How long the startup screenshot waits for data to arrive and fonts to settle.
const SCREENSHOT_DELAY: Duration = Duration::from_secs(2);

/// Environment variable that makes [`probe_wgpu`] fail, to exercise the fallback.
const FORCE_WGPU_FAILURE: &str = "EMBDBG_TEST_WGPU_FAIL";

/// What the window needs to know before it opens.
#[derive(Debug, Clone)]
pub struct WindowSettings {
    /// Repaint rate ceiling while data streams; `None` follows the display, which is the default
    /// and the steadiest: timed wake-ups jitter by 1–2 ms and miss the display's sync that way.
    pub max_fps: Option<f64>,
    /// What the settings window edits and saves. The command line may have overridden it for
    /// this one run.
    pub persisted: Settings,
    pub perf_test: Option<f64>,
    pub perf_report: Option<PathBuf>,
    /// Save the window here shortly after startup, then exit.
    pub screenshot: Option<PathBuf>,
    /// Open the settings window right away.
    pub open_settings: bool,
    /// The tab to show first, by name.
    pub initial_tab: Option<String>,
    /// A serial port to open right away.
    pub startup_serial: Option<SerialConfig>,
    pub startup_decoder: DecoderSpec,
    /// Whether this run may write the layout. Screenshot and performance runs keep their hands
    /// off the user's saved window.
    pub persist_layout: bool,
}

/// Tabs of the dock area. The names are part of `layout.json`: renaming one drops the saved
/// layout, which is the intended way to retire a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum Tab {
    Connections,
    Waveform,
    Xy,
    Spectrum,
    Histogram,
    ToolStack,
}

impl Tab {
    /// The name the command line uses.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "connections" => Self::Connections,
            "waveform" | "wave" => Self::Waveform,
            "xy" => Self::Xy,
            "spectrum" | "fft" => Self::Spectrum,
            "histogram" | "hist" => Self::Histogram,
            // The tools live in the right-hand stack now; these names select it.
            "terminal" | "hex" | "ai" | "mcp" | "send" | "log" | "inspector" | "channels"
            | "tools" | "toolstack" | "net" | "network" => Self::ToolStack,
            _ => return None,
        })
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Connections => "连接",
            Tab::Waveform => "波形",
            Tab::Xy => "X-Y",
            Tab::Spectrum => "频谱",
            Tab::Histogram => "直方图",
            Tab::ToolStack => "工具",
        }
    }
}

/// The layout the window starts with: the one saved last time, with `initial_tab` selected if it
/// names one; the default when there is nothing usable to restore.
fn startup_dock(initial_tab: Option<&str>) -> egui_dock::DockState<Tab> {
    let mut dock = crate::layout::load().unwrap_or_else(default_dock);
    if let Some(tab) = initial_tab.and_then(Tab::from_name)
        && let Some(path) = dock.find_tab(&tab)
    {
        let _ = dock.set_active_tab(path);
    }
    dock
}

/// The layout a fresh window starts with.
pub(crate) fn default_dock() -> egui_dock::DockState<Tab> {
    use egui_dock::NodeIndex;

    let mut dock =
        egui_dock::DockState::new(vec![Tab::Waveform, Tab::Xy, Tab::Spectrum, Tab::Histogram]);
    let tree = dock.main_surface_mut();
    // `split_*` returns `[old, new]`, and the fraction is the share of whichever child comes
    // first in layout order: the new node for a split to the left, the old one otherwise.
    let [center, _sidebar] =
        tree.split_left(NodeIndex::root(), SIDEBAR_SPLIT, vec![Tab::Connections]);
    // The tool stack keeps the other half: terminals side by side need the width.
    let [_center, _tools] = tree.split_right(center, TOOLSTACK_SPLIT, vec![Tab::ToolStack]);
    dock
}

/// Whether the connection sidebar is part of the layout.
pub(crate) fn connections_panel_open(dock: &egui_dock::DockState<Tab>) -> bool {
    dock.find_tab(&Tab::Connections).is_some()
}

/// Closes or reopens the connection sidebar against the left edge.
pub(crate) fn set_connections_panel(dock: &mut egui_dock::DockState<Tab>, open: bool) {
    if open == connections_panel_open(dock) {
        return;
    }
    if open {
        if dock.main_surface().root_node().is_none() {
            dock.push_to_focused_leaf(Tab::Connections);
        } else {
            dock.main_surface_mut().split_left(
                egui_dock::NodeIndex::root(),
                SIDEBAR_SPLIT,
                vec![Tab::Connections],
            );
        }
    } else if let Some(path) = dock.find_tab(&Tab::Connections) {
        dock.remove_tab(path);
    }
}

/// Keeps the connection sidebar a sensible size: wide enough for the forms, never more than the
/// fixed limit or 40% of the window. Without this the divider can swallow the whole window.
fn clamp_connections_width(dock: &mut egui_dock::DockState<Tab>, width: f32) {
    if width <= 0.0 {
        return;
    }
    let Some((node, _)) = dock.main_surface().find_tab(&Tab::Connections) else {
        return;
    };
    let root = egui_dock::NodeIndex::root();
    if node != root.left() {
        return;
    }
    let min = (SIDEBAR_MIN / width).clamp(0.05, 0.9);
    let max = (SIDEBAR_MAX / width).clamp(min, 0.4);
    if let egui_dock::Node::Horizontal(split) = &mut dock.main_surface_mut()[root] {
        split.fraction = split.fraction.clamp(min, max);
    }
}

/// Whether the tool stack is part of the layout.
pub(crate) fn toolstack_panel_open(dock: &egui_dock::DockState<Tab>) -> bool {
    dock.find_tab(&Tab::ToolStack).is_some()
}

/// Closes or reopens the tool stack against the right edge.
pub(crate) fn set_toolstack_panel(dock: &mut egui_dock::DockState<Tab>, open: bool) {
    if open == toolstack_panel_open(dock) {
        return;
    }
    if open {
        if dock.main_surface().root_node().is_none() {
            // Nothing left to split: the stack becomes the whole surface.
            dock.push_to_focused_leaf(Tab::ToolStack);
        } else {
            dock.main_surface_mut().split_right(
                egui_dock::NodeIndex::root(),
                INSPECTOR_SPLIT,
                vec![Tab::ToolStack],
            );
        }
    } else if let Some(path) = dock.find_tab(&Tab::ToolStack) {
        dock.remove_tab(path);
    }
}

pub fn run(
    cli: &crate::Cli,
    engine: Arc<Engine>,
    settings: Settings,
    startup_serial: Option<SerialConfig>,
    startup_decoder: DecoderSpec,
) -> anyhow::Result<()> {
    let window_settings = WindowSettings {
        max_fps: (settings.max_fps > 0.0).then_some(settings.max_fps),
        persisted: settings.clone(),
        perf_test: cli.perf_test,
        perf_report: cli.perf_report.clone(),
        screenshot: cli.screenshot.clone(),
        open_settings: cli.open_settings,
        initial_tab: cli.tab.clone(),
        startup_serial,
        startup_decoder,
        persist_layout: cli.screenshot.is_none() && cli.perf_test.is_none(),
    };

    let mut failure = String::from("没有尝试任何渲染后端");
    let order = attempts(settings.renderer);
    for (index, &attempt) in order.iter().enumerate() {
        let options = window_options(attempt);
        if attempt.renderer == eframe::Renderer::Wgpu {
            // eframe only logs it when wgpu finds no adapter, and then runs on without a painter,
            // so ask for one first.
            match probe_wgpu(options.wgpu_options.clone()) {
                Ok(adapter) => {
                    tracing::info!(target: "embdbg::renderer", "渲染后端：{}，适配器 {adapter}", attempt.label)
                }
                Err(reason) => {
                    tracing::warn!(target: "embdbg::renderer", "{} 不可用（{reason}）", attempt.label);
                    failure = format!("{} 不可用：{reason}", attempt.label);
                    continue;
                }
            }
        }

        let engine = Arc::clone(&engine);
        let window_settings = window_settings.clone();
        let attempt_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eframe::run_native(
                "embdbg",
                options,
                Box::new(move |cc| {
                    let app = EmbDbgApp::new(cc, engine, window_settings)?;
                    Ok(Box::new(app))
                }),
            )
        }));
        let attempt_result = match attempt_result {
            Ok(result) => result,
            Err(payload) => {
                // eframe gives up on a missing OpenGL 3.3 by panicking while it picks a GL config,
                // which is what a remote desktop or a driverless virtual machine comes to.
                tracing::warn!(
                    target: "embdbg::renderer",
                    "{} 启动时崩溃（{}）{}",
                    attempt.label,
                    panic_message(payload.as_ref()),
                    what_is_next(order.get(index + 1).map(|next| next.label))
                );
                failure = format!("{} 启动时崩溃", attempt.label);
                continue;
            }
        };
        match attempt_result {
            Ok(()) => return Ok(()),
            Err(error) if startup_failure(&error) == StartupFailure::NoOpenGl => {
                tracing::warn!(
                    target: "embdbg::renderer",
                    "OpenGL 不可用（{error}）{}",
                    what_is_next(order.get(index + 1).map(|next| next.label))
                );
                failure = format!("OpenGL 不可用：{error}");
            }
            Err(error) => return Err(anyhow::anyhow!("界面启动失败：{error}")),
        }
    }
    Err(anyhow::anyhow!("界面启动失败：{failure}"))
}

/// One try at starting the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attempt {
    renderer: eframe::Renderer,
    /// Restricts wgpu to one graphics API; `None` lets it choose.
    backend: Option<eframe::wgpu::Backends>,
    /// How the log names this try.
    label: &'static str,
}

const OPENGL: Attempt = Attempt {
    renderer: eframe::Renderer::Glow,
    backend: None,
    label: "OpenGL",
};
const WGPU_ANY: Attempt = Attempt {
    renderer: eframe::Renderer::Wgpu,
    backend: None,
    label: "wgpu",
};
const DIRECTX_12: Attempt = Attempt {
    renderer: eframe::Renderer::Wgpu,
    backend: Some(eframe::wgpu::Backends::DX12),
    label: "DirectX 12",
};
const VULKAN: Attempt = Attempt {
    renderer: eframe::Renderer::Wgpu,
    backend: Some(eframe::wgpu::Backends::VULKAN),
    label: "Vulkan",
};

/// Which renderers to try, in order.
fn attempts(choice: RendererChoice) -> &'static [Attempt] {
    match (choice, cfg!(target_os = "macos")) {
        (RendererChoice::Glow, _) => &[OPENGL],
        (RendererChoice::Wgpu, _) => &[WGPU_ANY, OPENGL],
        (RendererChoice::Dx12, _) => &[DIRECTX_12, OPENGL],
        (RendererChoice::Vulkan, _) => &[VULKAN, OPENGL],
        (RendererChoice::Auto, true) => &[WGPU_ANY, OPENGL],
        (RendererChoice::Auto, false) => &[OPENGL, WGPU_ANY],
    }
}

/// How a log line about a failed attempt ends.
fn what_is_next(next: Option<&'static str>) -> String {
    next.map_or_else(
        || "，没有别的渲染后端可试".to_owned(),
        |next| format!("，改试 {next}"),
    )
}

/// Environment variable that makes the OpenGL renderer fail for real, to exercise the fallback.
const FORCE_GLOW_FAILURE: &str = "EMBDBG_TEST_GLOW_FAIL";

/// No graphics driver offers this much antialiasing, so eframe finds no matching GL config.
const IMPOSSIBLE_MSAA: u16 = 64;

fn window_options(attempt: Attempt) -> eframe::NativeOptions {
    let viewport = egui::ViewportBuilder::default()
        .with_title(format!("embdbg {}", env!("CARGO_PKG_VERSION")))
        .with_app_id("embdbg")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([820.0, 520.0]);
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: attempt.renderer,
        centered: true,
        ..Default::default()
    };
    if attempt.renderer == eframe::Renderer::Wgpu {
        options.wgpu_options = wgpu_options(attempt.backend);
    }
    if attempt.renderer == eframe::Renderer::Glow && std::env::var_os(FORCE_GLOW_FAILURE).is_some()
    {
        options.multisampling = IMPOSSIBLE_MSAA;
    }
    options
}

/// The message a panic carries.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "（没有文字说明）".to_owned()
    }
}

/// Why a renderer could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupFailure {
    /// No OpenGL 3.3 context could be created. Another renderer may still work.
    NoOpenGl,
    /// Anything else: a window problem, or an error raised by the app itself.
    Other,
}

/// Unlike wgpu, which eframe only logs, a failing OpenGL setup comes back as an error. These
/// are the variants that say "no OpenGL here" rather than "something else broke".
fn startup_failure(error: &eframe::Error) -> StartupFailure {
    match error {
        eframe::Error::NoGlutinConfigs(..)
        | eframe::Error::Glutin(_)
        | eframe::Error::OpenGL(_) => StartupFailure::NoOpenGl,
        eframe::Error::AppCreation(_)
        | eframe::Error::Winit(_)
        | eframe::Error::WinitEventLoop(_)
        | eframe::Error::Wgpu(_) => StartupFailure::Other,
    }
}

/// The wgpu configuration the window runs with.
///
/// The startup probe works from the same rules, so what it reports is what the window gets.
fn wgpu_options(backends: Option<eframe::wgpu::Backends>) -> eframe::WgpuConfiguration {
    use eframe::wgpu;

    let mut config = eframe::WgpuConfiguration::default();
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup else {
        return config;
    };
    if let Some(backends) = backends {
        setup.instance_descriptor.backends = backends;
    }
    setup.native_adapter_selector = Some(std::sync::Arc::new(choose_adapter));
    setup.device_descriptor = Arc::new(|adapter| {
        let base_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };
        wgpu::DeviceDescriptor {
            label: Some("embdbg"),
            // Large enough for 4K displays.
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..base_limits
            },
            // wgpu's default carves memory out of large blocks to favour throughput. A tool that
            // draws a few thousand triangles a frame has no use for them.
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }
    });
    config
}

/// The adapter selection eframe calls into, in the shape it expects.
fn choose_adapter(
    adapters: &[eframe::wgpu::Adapter],
    _surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    adapters
        .iter()
        .max_by_key(|adapter| adapter_rank(&adapter.get_info()))
        .cloned()
        .ok_or_else(|| "没有可用的图形适配器".to_owned())
}

/// How much an adapter is preferred; higher wins. Real hardware beats a software rasterizer,
/// then a discrete GPU beats an integrated one, then the backend decides.
fn adapter_rank(info: &eframe::wgpu::AdapterInfo) -> (u8, u8, u8) {
    use eframe::wgpu::DeviceType;

    let hardware = match info.device_type {
        DeviceType::DiscreteGpu => 3,
        DeviceType::IntegratedGpu | DeviceType::VirtualGpu | DeviceType::Other => 2,
        DeviceType::Cpu => 0,
    };
    (hardware, hardware, backend_rank(info.backend))
}

/// Backend preference. On Windows, DirectX 12 outranks Vulkan: on the machine this was built on
/// (Intel Arc integrated graphics), Intel's Vulkan driver kept 27% of a core busy while the window
/// sat idle and cost about 8 ms of CPU time per frame, where DirectX 12 showed neither.
fn backend_rank(backend: eframe::wgpu::Backend) -> u8 {
    use eframe::wgpu::Backend::*;

    if cfg!(target_os = "windows") {
        match backend {
            Dx12 => 3,
            Vulkan => 2,
            Gl => 1,
            _ => 0,
        }
    } else if cfg!(target_os = "macos") {
        match backend {
            Metal => 3,
            _ => 0,
        }
    } else {
        match backend {
            Vulkan => 3,
            Gl => 2,
            _ => 0,
        }
    }
}

fn probe_wgpu(config: eframe::WgpuConfiguration) -> Result<String, String> {
    if std::env::var_os(FORCE_WGPU_FAILURE).is_some() {
        return Err(format!("{FORCE_WGPU_FAILURE} 已设置"));
    }
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = config.wgpu_setup else {
        return Err("eframe 的 wgpu 配置不是新建实例".to_owned());
    };
    let backends = setup.instance_descriptor.backends;
    let instance = eframe::wgpu::Instance::new(setup.instance_descriptor);
    // A throwaway runtime: enumerating adapters is one call that returns before the window opens.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // The window, and with it the surface eframe checks adapters against, does not exist yet.
    let adapters = runtime.block_on(instance.enumerate_adapters(backends));
    let info = choose_adapter(&adapters, None)?.get_info();
    Ok(format!(
        "{}，{:?}，{:?}",
        info.name, info.backend, info.device_type
    ))
}

/// Names the renderer and the hardware behind it, for the status bar and the log.
fn describe_backend(cc: &eframe::CreationContext<'_>) -> String {
    if let Some(state) = &cc.wgpu_render_state {
        let info = state.adapter.get_info();
        return format!("wgpu · {:?} · {}", info.backend, info.name);
    }
    if let Some(gl) = &cc.gl {
        // SAFETY: the GL context is current on this thread while the app is being created.
        let renderer = unsafe { gl.get_parameter_string(eframe::glow::RENDERER) };
        return format!("glow · {renderer}");
    }
    "未知".to_owned()
}

pub struct EmbDbgApp {
    engine: Arc<Engine>,
    events: broadcast::Receiver<Event>,
    ai_feed: broadcast::Receiver<embdbg_core::mcp::Activity>,
    /// Screenshot requests from MCP clients, answered by the next frame.
    screenshot_asks:
        tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<Result<String, String>>>,
    screenshot_seq: u64,
    backend: String,
    settings: WindowSettings,
    dock: egui_dock::DockState<Tab>,
    connections: ConnectionsState,
    channels: ChannelUi,
    log: LogBuffer,
    terminal: TerminalState,
    hex: HexState,
    send: SendState,
    xy: XyState,
    spectrum: SpectrumState,
    histogram: HistogramState,
    ai: AiState,
    net: NetToolsState,
    wave: WaveView,
    wave_scratch: WaveScratch,
    perf: PerfMonitor,
    perf_test: Option<PerfTest>,
    screenshot: Option<ScreenshotRequest>,
    /// The right-hand stack: terminals and the tools opened from the menu.
    stack: StackState,
    /// Whether the about window is open.
    about_open: bool,
    /// Whether the settings window is open.
    settings_open: bool,
    /// The edits in that window, saved on request.
    pending: Settings,
    /// What is currently in the settings file.
    saved: Settings,
    /// Whether this run may write the layout; screenshot and performance runs may not.
    persist_layout: bool,
    /// The layout as last written, so a quiet session does not rewrite the file.
    saved_layout: String,
    /// When the layout was last compared with the file.
    layout_checked: Instant,
    /// The connection form as last written, so a quiet panel does not rewrite the file.
    saved_form: crate::settings::ConnectionForm,
    /// When the connection form was last compared with the file.
    form_checked: Instant,
}

/// A pending `--screenshot`, taken once its delay has passed.
struct ScreenshotRequest {
    path: PathBuf,
    due: Instant,
    requested: bool,
}

impl EmbDbgApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        engine: Arc<Engine>,
        mut settings: WindowSettings,
    ) -> anyhow::Result<Self> {
        theme::install(&cc.egui_ctx);
        fonts::install(&cc.egui_ctx);

        let backend = describe_backend(cc);
        tracing::info!(target: "embdbg::renderer", "渲染后端已就绪：{backend}");
        let mut log = LogBuffer::default();
        log.info(format!(
            "embdbg {} 已启动，渲染后端 {backend}",
            env!("CARGO_PKG_VERSION")
        ));

        let saved = settings.persisted.clone();
        let macros = saved.macros.clone();
        let startup_decoder = settings.startup_decoder;
        let pending = saved.clone();
        let open_settings = settings.open_settings;
        let initial_tab = settings.initial_tab.clone();
        if let Some(config) = &settings.startup_serial {
            match engine.open(TransportConfig::Serial(config.clone())) {
                Ok(id) => {
                    if let Some(connection) = engine.connection(id) {
                        connection.set_decoder(startup_decoder);
                    }
                }
                Err(error) => log.error(format!("打开串口失败：{error}")),
            }
        }

        let perf_test = match (&settings.perf_test, &settings.perf_report) {
            (Some(seconds), Some(report)) => {
                if !engine.simulator_running() {
                    engine.start_simulator(embdbg_core::transport::SimulatorConfig::default());
                }
                Some(PerfTest {
                    started: None,
                    seconds: *seconds,
                    report: report.clone(),
                    frame_times: Vec::new(),
                    ui_times: Vec::new(),
                })
            }
            _ => None,
        };

        let screenshot = settings.screenshot.take().map(|path| ScreenshotRequest {
            path,
            due: Instant::now() + SCREENSHOT_DELAY,
            requested: false,
        });

        let dock = startup_dock(initial_tab.as_deref());
        let saved_layout = crate::layout::serialize(&dock).unwrap_or_default();
        let persist_layout = settings.persist_layout;
        let form = saved.connections.clone();
        let (shot_tx, shot_rx) = tokio::sync::mpsc::unbounded_channel();
        let port: embdbg_core::mcp::ScreenshotPort =
            Arc::new(move |reply| shot_tx.send(reply).is_ok());
        let ai_feed = crate::mcp_host::spawn_server(Arc::clone(&engine), Some(port));

        Ok(Self {
            events: engine.subscribe(),
            ai_feed,
            screenshot_asks: shot_rx,
            screenshot_seq: 0,
            engine,
            backend,
            settings,
            dock,
            connections: ConnectionsState::from_form(form.clone()),
            channels: ChannelUi::default(),
            terminal: TerminalState::with_names(saved.terminal_names.clone()),
            hex: HexState::default(),
            send: SendState::with_macros(macros),
            xy: XyState::default(),
            spectrum: SpectrumState::default(),
            histogram: HistogramState::default(),
            ai: AiState::default(),
            net: NetToolsState::default(),
            log,
            wave: WaveView::default(),
            wave_scratch: WaveScratch::default(),
            perf: PerfMonitor::default(),
            perf_test,
            screenshot,
            stack: StackState::from_settings(&saved.stack_tools),
            about_open: false,
            settings_open: open_settings,
            pending,
            saved,
            persist_layout,
            saved_form: form,
            form_checked: Instant::now(),
            saved_layout,
            layout_checked: Instant::now(),
        })
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(theme::SPACE_2);
            ui.label(RichText::new("embdbg").strong().size(15.0));
            ui.separator();

            if ui
                .add(Button::new(icon_label(outline::ERASER, "清空数据", 13.5)))
                .clicked()
                && let Some(id) = self.engine.active_id()
            {
                self.engine.clear(id);
            }

            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(theme::SPACE_2);
                let gear = Button::new(fonts::icon(outline::SETTINGS, 16.0));
                if ui.add(gear).on_hover_text("设置").clicked() {
                    self.settings_open = !self.settings_open;
                }
                ui.separator();
                let connections_open = connections_panel_open(&self.dock);
                let toggle = Button::new(icon_label(outline::LAYOUT_SIDEBAR, "连接", 13.5))
                    .selected(connections_open);
                if ui
                    .add(toggle)
                    .on_hover_text(if connections_open {
                        "隐藏左侧的连接栏"
                    } else {
                        "显示左侧的连接栏"
                    })
                    .clicked()
                {
                    set_connections_panel(&mut self.dock, !connections_open);
                }
                let open = toolstack_panel_open(&self.dock);
                let toggle = Button::new(icon_label(outline::LAYOUT_SIDEBAR_RIGHT, "工具", 13.5))
                    .selected(open);
                if ui
                    .add(toggle)
                    .on_hover_text(if open {
                        "隐藏右侧的工具栈"
                    } else {
                        "显示右侧的工具栈"
                    })
                    .clicked()
                {
                    set_toolstack_panel(&mut self.dock, !open);
                }
                ui.separator();
                ui.label(RichText::new(&self.backend).small().color(theme::FG_DIM));
            });
        });
    }

    /// The row of menus: tools to open, panel visibility, and about.
    fn menu_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(theme::SPACE_2);
            ui.menu_button("工具", |ui| {
                for tool in StackTool::ALL {
                    let mut open = self.stack.is_open(tool);
                    if ui.checkbox(&mut open, tool.title()).changed() {
                        self.stack.set_open(tool, open);
                    }
                }
                ui.separator();
                if ui.button("全部关闭").clicked() {
                    for tool in StackTool::ALL {
                        self.stack.set_open(tool, false);
                    }
                    ui.close();
                }
            });
            ui.menu_button("窗口", |ui| {
                let mut connections = connections_panel_open(&self.dock);
                if ui.checkbox(&mut connections, "连接栏").changed() {
                    set_connections_panel(&mut self.dock, connections);
                }
                let mut stack = toolstack_panel_open(&self.dock);
                if ui.checkbox(&mut stack, "工具栈").changed() {
                    set_toolstack_panel(&mut self.dock, stack);
                }
                ui.separator();
                if ui.button("重置布局").clicked() {
                    self.dock = default_dock();
                    ui.close();
                }
            });
            ui.menu_button("帮助", |ui| {
                if ui.button("关于 embdbg").clicked() {
                    self.about_open = true;
                    ui.close();
                }
                ui.hyperlink_to("GitHub 仓库", "https://github.com/WangJialong19/embdbg");
            });
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("「工具」菜单里打开终端以外的面板，面板右上角可关闭")
                        .small()
                        .color(theme::FG_DIM),
                );
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let streaming = self.engine.is_streaming();
        let shared = self.engine.active_shared();
        let (channels, frames, rate) = {
            let data = shared.channels.read();
            let channels = data.channels().len();
            let rate = data.axis().frame_rate().unwrap_or(0.0) * channels as f64;
            (channels, data.frames(), rate)
        };

        ui.horizontal(|ui| {
            ui.add_space(theme::SPACE_2 + 4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            let lamp = if streaming {
                theme::SUCCESS
            } else {
                theme::FG_DIM
            };
            ui.painter().circle_filled(rect.center(), 4.0, lamp);
            let state = if streaming { "数据流入" } else { "空闲" };
            let detail = if channels == 0 {
                String::new()
            } else {
                format!(
                    " · {channels} 通道 · {} 帧 · {}",
                    format_count(frames),
                    format_rate(rate, "样本/s")
                )
            };
            ui.label(
                RichText::new(format!("{state}{detail}"))
                    .small()
                    .color(theme::FG_DIM),
            );

            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(theme::SPACE_2);
                let (fps, frame_ms) = (self.perf.fps, self.perf.frame_ms);
                ui.label(
                    RichText::new(format!("{fps:.0} FPS · {frame_ms:.2} ms"))
                        .monospace()
                        .small()
                        .color(theme::FG_DIM),
                );
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "界面缩放 {:.0}%",
                        ui.ctx().pixels_per_point() * 100.0
                    ))
                    .small()
                    .color(theme::FG_DIM),
                );
            });
        });
    }

    fn show_dock(&mut self, ui: &mut Ui) {
        let Self {
            dock,
            engine,
            connections,
            channels,
            log,
            wave,
            wave_scratch,
            terminal,
            hex,
            send,
            xy,
            spectrum,
            histogram,
            ai,
            net,
            stack,
            ..
        } = self;
        let mut viewer = DockViewer {
            engine,
            connections,
            channels,
            log,
            wave,
            wave_scratch,
            terminal,
            hex,
            send,
            xy,
            spectrum,
            histogram,
            ai,
            net,
            stack,
        };
        let width = ui.available_width();
        egui_dock::DockArea::new(dock)
            .style(egui_dock::Style::from_egui(ui.style()))
            // The tab's own close button is enough; a second one for the whole pane is noise.
            .show_leaf_close_all_buttons(false)
            // The arrow egui_dock puts at the left of a tab bar folds the pane into a blank strip.
            // Hiding is what the toolbar toggle and the tab's close button do instead.
            .show_leaf_collapse_buttons(false)
            // Dragging the junction resizes several panes at once, which is how the sidebar used
            // to shrink upwards as well. Individual separators are still draggable.
            .draggable_separator_junctions(false)
            .draggable_tabs(false)
            .show_inside(ui, &mut viewer);
        clamp_connections_width(dock, width);
    }

    fn schedule_repaint(&self, ctx: &egui::Context) {
        let interval = if self.engine.is_streaming() {
            self.settings
                .max_fps
                .map_or(Duration::ZERO, |fps| Duration::from_secs_f64(1.0 / fps))
        } else {
            IDLE_INTERVAL
        };
        if interval.is_zero() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(interval);
        }
    }

    /// Ends the performance test: write the report and close the window.
    fn finish_perf_test(&self, ctx: &egui::Context) {
        let Some(test) = &self.perf_test else { return };
        let (channels, rate) = {
            let shared = self.engine.active_shared();
            let data = shared.channels.read();
            let channels = data.channels().len();
            (
                channels,
                data.axis().frame_rate().unwrap_or(0.0) * channels as f64,
            )
        };
        let screen = ctx.input(|i| i.raw.screen_rect).map_or_else(
            || "?x?".to_owned(),
            |r| format!("{}x{}", r.width(), r.height()),
        );
        let context = [
            ("backend", self.backend.clone()),
            (
                "max_fps",
                self.settings
                    .max_fps
                    .map_or_else(|| "uncapped".to_owned(), |fps| format!("{fps:.0}")),
            ),
            ("scale", format!("{:.2}", ctx.pixels_per_point())),
            ("window", screen),
            ("channels", channels.to_string()),
            ("samples_per_second", format!("{rate:.0}")),
        ];
        match test.write_report(&context) {
            Ok(()) => {
                tracing::info!(target: "embdbg::perf", "性能自检完成，报告写入 {}", test.report.display())
            }
            Err(e) => tracing::error!(target: "embdbg::perf", "写性能报告失败：{e}"),
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Draws the tabs of the dock area. Borrows exactly the parts of [`EmbDbgApp`] the tabs need.
struct DockViewer<'a> {
    engine: &'a Arc<Engine>,
    connections: &'a mut ConnectionsState,
    channels: &'a mut ChannelUi,
    log: &'a mut LogBuffer,
    wave: &'a mut WaveView,
    wave_scratch: &'a mut WaveScratch,
    terminal: &'a mut TerminalState,
    hex: &'a mut HexState,
    send: &'a mut SendState,
    xy: &'a mut XyState,
    spectrum: &'a mut SpectrumState,
    histogram: &'a mut HistogramState,
    ai: &'a mut AiState,
    net: &'a mut NetToolsState,
    stack: &'a mut StackState,
}

impl egui_dock::TabViewer for DockViewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::unique(("embdbg-tab", *tab))
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Connections => {
                if let panels::connections::Action::Failed(error) =
                    panels::connections::ui(ui, self.engine, self.connections)
                {
                    self.log.error(error);
                }
            }
            Tab::Waveform => {
                let shared = self.engine.active_shared();
                let data = shared.channels.read();
                let visible = self.channels.visible(data.channels().len());
                crate::plots::wave::show(ui, &data, self.wave, visible, self.wave_scratch);
            }
            Tab::Xy => {
                let shared = self.engine.active_shared();
                panels::xy::ui(ui, &shared, self.xy);
            }
            Tab::Spectrum => {
                let shared = self.engine.active_shared();
                panels::spectrum::ui(ui, &shared, self.spectrum);
            }
            Tab::Histogram => {
                let shared = self.engine.active_shared();
                panels::histogram::ui(ui, &shared, self.histogram);
            }
            Tab::ToolStack => {
                let panels = panels::stack::StackPanels {
                    channels: self.channels,
                    terminal: self.terminal,
                    hex: self.hex,
                    send: self.send,
                    log: self.log,
                    ai: self.ai,
                    net: self.net,
                };
                panels::stack::ui(ui, self.engine, self.stack, panels);
            }
        }
    }

    /// The two side panels can be closed, from their tabs or from the toolbar; the main views
    /// stay. `set_connections_panel` and `set_toolstack_panel` reopen them.
    fn is_closeable(&self, tab: &Tab) -> bool {
        matches!(tab, Tab::Connections | Tab::ToolStack)
    }
}

/// Frame rate and frame time of the recent past, smoothed so the digits do not flicker.
#[derive(Debug, Default)]
struct PerfMonitor {
    fps: f64,
    frame_ms: f64,
}

impl PerfMonitor {
    /// Folds this frame into the smoothed averages. Returns the raw interval since the previous
    /// frame, once there is one.
    fn observe(&mut self, ctx: &egui::Context) -> Option<f32> {
        let raw = ctx.input(|i| i.unstable_dt);
        let dt = ctx.input(|i| i.stable_dt);
        // The first frame, and any frame egui could not time, has no usable interval.
        if raw.is_nan() || raw <= 0.0 || dt.is_nan() || dt <= 0.0 {
            return None;
        }
        let (fps, ms) = (1.0 / f64::from(dt), f64::from(dt) * 1000.0);
        if self.fps == 0.0 {
            (self.fps, self.frame_ms) = (fps, ms);
        } else {
            self.fps += (fps - self.fps) * 0.1;
            self.frame_ms += (ms - self.frame_ms) * 0.1;
        }
        Some(raw)
    }
}

/// How the intervals between frames spread out, which tells a steady pace from bursts of repaints.
///
/// Counts frames per bucket, in milliseconds: `≤2`, `≤6`, `≤10`, `≤14`, `≤18`, `≤25`, `>25`.
fn interval_histogram(frame_times: &[f32]) -> Vec<(&'static str, usize)> {
    const BUCKETS: [(&str, f32); 7] = [
        ("<=2", 2.0),
        ("<=6", 6.0),
        ("<=10", 10.0),
        ("<=14", 14.0),
        ("<=18", 18.0),
        ("<=25", 25.0),
        (">25", f32::INFINITY),
    ];
    let mut counts = vec![0usize; BUCKETS.len()];
    for &t in frame_times {
        let ms = t * 1000.0;
        let bucket = BUCKETS
            .iter()
            .position(|(_, bound)| ms <= *bound)
            .unwrap_or(BUCKETS.len() - 1);
        counts[bucket] += 1;
    }
    BUCKETS
        .iter()
        .map(|(label, _)| *label)
        .zip(counts)
        .collect()
}

/// Collects frame intervals for a fixed stretch and summarises them into a report file.
struct PerfTest {
    started: Option<Instant>,
    seconds: f64,
    report: PathBuf,
    /// Raw intervals between frames, in seconds. Unlike the smoothed `stable_dt`, these show
    /// every hiccup and every redundant repaint.
    frame_times: Vec<f32>,
    /// Time spent inside `App::ui` per frame, which excludes waiting for the display.
    ui_times: Vec<f32>,
}

impl PerfTest {
    /// Records a frame. Returns `true` once enough frames have been collected.
    fn record(&mut self, frame_seconds: f32, ui_ms: f32) -> bool {
        let started = *self.started.get_or_insert_with(Instant::now);
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed < PERF_WARMUP_SECONDS {
            return false;
        }
        self.frame_times.push(frame_seconds);
        self.ui_times.push(ui_ms);
        elapsed - PERF_WARMUP_SECONDS >= self.seconds
    }

    /// Writes `key = value` lines, so the result can be read by eye or by script.
    fn write_report(&self, context: &[(&str, String)]) -> std::io::Result<()> {
        let mut frames = self.frame_times.clone();
        frames.sort_by(|a, b| a.partial_cmp(b).expect("frame times are never NaN"));
        let frame_ms_at = |quantile: f64| -> f64 {
            let index = ((frames.len() - 1) as f64 * quantile).round() as usize;
            f64::from(frames[index]) * 1000.0
        };
        let total: f64 = self.frame_times.iter().map(|&t| f64::from(t)).sum();
        let ui_ms_at = |pick: fn(&[f32]) -> f32| f64::from(pick(&self.ui_times));

        let mut lines = vec![format!("version = {}", env!("CARGO_PKG_VERSION"))];
        for (key, value) in context {
            lines.push(format!("{key} = {value}"));
        }
        lines.extend([
            format!("seconds = {total:.2}"),
            format!("frames = {}", self.frame_times.len()),
            format!("avg_fps = {:.1}", self.frame_times.len() as f64 / total),
            // The frame rate the user sees in the worst one frame in twenty.
            format!("p95_fps = {:.1}", 1000.0 / frame_ms_at(0.95)),
            format!("median_frame_ms = {:.2}", frame_ms_at(0.5)),
            format!("p95_frame_ms = {:.2}", frame_ms_at(0.95)),
            format!("max_frame_ms = {:.2}", frame_ms_at(1.0)),
            format!(
                "avg_ui_ms = {:.2}",
                ui_ms_at(|v| v.iter().sum::<f32>() / v.len() as f32)
            ),
            format!(
                "max_ui_ms = {:.2}",
                ui_ms_at(|v| v.iter().copied().fold(0.0, f32::max))
            ),
            format!(
                "intervals_ms = {}",
                interval_histogram(&self.frame_times)
                    .iter()
                    .map(|(label, count)| format!("{label}:{count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ]);
        std::fs::write(&self.report, lines.join("\n") + "\n")
    }
}

impl eframe::App for EmbDbgApp {
    /// Runs before every frame, and also while the window is hidden. The place to drain engine
    /// events: nothing here may touch the UI.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            self.log.push_event(&event);
        }
        while let Ok(reply) = self.screenshot_asks.try_recv() {
            let path = std::env::temp_dir().join("embdbg").join(format!(
                "screenshot-{}-{}.png",
                std::process::id(),
                self.screenshot_seq
            ));
            self.screenshot_seq = self.screenshot_seq.wrapping_add(1);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let saved = path.clone();
            ctx.request_screenshot(move |image| {
                let result = crate::screenshot::save_png(&image, &saved)
                    .map(|()| saved.display().to_string())
                    .map_err(|error| format!("保存截图失败：{error}"));
                let _ = reply.send(result);
            });
        }
        loop {
            match self.ai_feed.try_recv() {
                Ok(activity) => self.ai.push(activity),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let started = Instant::now();
        let bar = || {
            Frame::new()
                .fill(theme::BG_PANEL)
                .inner_margin(Margin::symmetric(10, 0))
        };
        egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .frame(bar())
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::top("menubar")
            .exact_size(MENUBAR_HEIGHT)
            .frame(bar())
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("statusbar")
            .exact_size(STATUSBAR_HEIGHT)
            .frame(bar())
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG_WINDOW))
            .show(ui, |ui| self.show_dock(ui));

        // The dock has taken this frame's clicks and drags by now.
        self.save_layout(false);
        self.save_connection_form(false);
        if self.send.take_macros_changed() {
            self.save_macros();
        }
        if self.terminal.take_names_changed() {
            self.save_terminal_names();
        }
        if self.stack.take_changed() {
            self.save_stack_tools();
        }

        let ctx = ui.ctx().clone();
        self.handle_settings(&ctx);
        self.about_window(&ctx);

        let ui_ms = started.elapsed().as_secs_f32() * 1000.0;
        let frame_time = self.perf.observe(&ctx);
        if let (Some(frame_time), Some(test)) = (frame_time, &mut self.perf_test)
            && test.record(frame_time, ui_ms)
        {
            self.finish_perf_test(&ctx);
        }
        self.request_screenshot_when_due(&ctx);
        self.schedule_repaint(&ctx);
    }

    /// The last chance to keep a layout or a connection form the one-second check has not
    /// written yet.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_layout(true);
        self.save_connection_form(true);
        if self.send.take_macros_changed() {
            self.save_macros();
        }
        if self.terminal.take_names_changed() {
            self.save_terminal_names();
        }
        if self.stack.take_changed() {
            self.save_stack_tools();
        }
    }
}

impl EmbDbgApp {
    /// Draws the settings window and carries out what it returns.
    fn handle_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        match panels::settings::ui(
            ctx,
            &mut self.settings_open,
            &mut self.pending,
            &self.backend,
        ) {
            panels::settings::Action::None => {}
            panels::settings::Action::Save => self.save_settings(),
            panels::settings::Action::SaveAndRestart => {
                self.save_settings();
                self.restart(ctx);
            }
        }
    }

    /// Writes the settings file and applies what can change without a restart.
    fn save_settings(&mut self) {
        match self.pending.save() {
            Ok(()) => {
                let path = Settings::path().map_or_else(
                    || "config.toml".to_owned(),
                    |path| path.display().to_string(),
                );
                self.log.info(format!("设置已保存到 {path}"));
                self.saved = self.pending.clone();
                // The frame rate ceiling takes effect at once; a new renderer needs the restart.
                self.settings.max_fps =
                    (self.pending.max_fps > 0.0).then_some(self.pending.max_fps);
            }
            Err(error) => self.log.error(format!("保存设置失败：{error}")),
        }
    }

    /// A small about window: version, purpose, repository.
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        egui::Window::new("关于 embdbg")
            .open(&mut self.about_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("embdbg {}", env!("CARGO_PKG_VERSION"))).strong());
                ui.label(
                    RichText::new("串口、网络、波形与 AI 验证，一个窗口里完成。")
                        .color(theme::FG_DIM),
                );
                ui.separator();
                ui.hyperlink_to("GitHub 仓库", "https://github.com/WangJialong19/embdbg");
                ui.label(RichText::new(&self.backend).small().color(theme::FG_DIM));
            });
    }

    /// Writes the dock layout when it has changed since the last check: a tab moved, the channel
    /// table closed, a divider dragged. At most once a second, and never for screenshot or
    /// performance runs.
    fn save_layout(&mut self, force: bool) {
        if !self.persist_layout {
            return;
        }
        if !force && self.layout_checked.elapsed() < LAYOUT_CHECK_INTERVAL {
            return;
        }
        self.layout_checked = Instant::now();
        let Ok(text) = crate::layout::serialize(&self.dock) else {
            return;
        };
        if !force && text == self.saved_layout {
            return;
        }
        // Remember the text even when writing fails: a full disk must not make the app retry
        // every second. The next real change tries again.
        self.saved_layout = text.clone();
        if let Err(error) = crate::layout::write(&text) {
            self.log.error(format!("保存窗口布局失败：{error}"));
        }
    }

    /// Writes the connection form when it has changed: the interface and the values in its
    /// fields. At most once a second, so typing an address does not rewrite the file per key.
    fn save_connection_form(&mut self, force: bool) {
        if !self.persist_layout {
            return;
        }
        if !force && self.form_checked.elapsed() < LAYOUT_CHECK_INTERVAL {
            return;
        }
        self.form_checked = Instant::now();
        let form = self.connections.form();
        if !force && form == self.saved_form {
            return;
        }
        self.saved_form = form.clone();
        // Write the settings as last saved, with only this section replaced, so an open settings
        // window's unsaved edits stay unsaved. The pending copy follows, or its next save would
        // put the old values back.
        let mut next = self.saved.clone();
        next.connections = form.clone();
        self.pending.connections = form;
        match next.save() {
            Ok(()) => self.saved = next,
            Err(error) => self.log.error(format!("保存连接设置失败：{error}")),
        }
    }

    /// Writes the send macros when one was added, replaced or removed.
    fn save_macros(&mut self) {
        let macros = self.send.macros.clone();
        let mut next = self.saved.clone();
        next.macros = macros.clone();
        self.pending.macros = macros;
        match next.save() {
            Ok(()) => self.saved = next,
            Err(error) => self.log.error(format!("保存发送宏失败：{error}")),
        }
    }

    /// Writes which tools are open in the right-hand stack.
    fn save_stack_tools(&mut self) {
        let tools = self.stack.ids();
        let mut next = self.saved.clone();
        next.stack_tools = tools.clone();
        self.pending.stack_tools = tools;
        match next.save() {
            Ok(()) => self.saved = next,
            Err(error) => self.log.error(format!("保存工具面板设置失败：{error}")),
        }
    }

    /// Writes the custom terminal-tab names.
    fn save_terminal_names(&mut self) {
        let names = self.terminal.names().clone();
        let mut next = self.saved.clone();
        next.terminal_names = names.clone();
        self.pending.terminal_names = names;
        match next.save() {
            Ok(()) => self.saved = next,
            Err(error) => self.log.error(format!("保存终端名称失败：{error}")),
        }
    }

    /// Starts the program over, which is what a new renderer needs.
    fn restart(&self, ctx: &egui::Context) {
        match std::env::current_exe() {
            Ok(exe) => match std::process::Command::new(exe).spawn() {
                Ok(_child) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => tracing::error!(target: "embdbg::settings", "重启失败：{error}"),
            },
            Err(error) => tracing::error!(target: "embdbg::settings", "找不到程序自身：{error}"),
        }
    }

    /// Asks for the startup screenshot once its delay has passed. The window closes once the
    /// pixels are saved.
    fn request_screenshot_when_due(&mut self, ctx: &egui::Context) {
        let Some(request) = &mut self.screenshot else {
            return;
        };
        if request.requested || Instant::now() < request.due {
            return;
        }
        request.requested = true;
        let path = request.path.clone();
        let ctx = ctx.clone();
        let closing_ctx = ctx.clone();
        ctx.request_screenshot(move |image| {
            match crate::screenshot::save_png(&image, &path) {
                Ok(()) => {
                    tracing::info!(target: "embdbg::screenshot", "窗口截图已保存：{}", path.display())
                }
                Err(e) => tracing::error!(target: "embdbg::screenshot", "保存截图失败：{e}"),
            }
            closing_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_holds_every_tab_once() {
        let dock = default_dock();
        let mut tabs: Vec<Tab> = dock.main_surface().tabs().copied().collect();
        tabs.sort_by_key(|tab| tab.title());
        let mut expected = [
            Tab::Connections,
            Tab::Waveform,
            Tab::Xy,
            Tab::Spectrum,
            Tab::Histogram,
            Tab::ToolStack,
        ];
        expected.sort_by_key(|tab| tab.title());
        assert_eq!(tabs, expected);
    }

    #[test]
    fn the_tool_stack_closes_and_reopens_on_the_right() {
        let mut dock = default_dock();
        assert!(toolstack_panel_open(&dock));

        set_toolstack_panel(&mut dock, false);
        assert!(!toolstack_panel_open(&dock));
        // Everything else stays in the layout.
        let tabs: Vec<Tab> = dock.main_surface().tabs().copied().collect();
        assert_eq!(tabs.len(), 5);
        assert!(!tabs.contains(&Tab::ToolStack));

        set_toolstack_panel(&mut dock, true);
        assert!(toolstack_panel_open(&dock));
        // It comes back against the right edge, where it started.
        let tree = dock.main_surface();
        let root = egui_dock::NodeIndex::root();
        assert!(tree[root].is_horizontal());
        let right: Vec<Tab> = tree[root.right()]
            .tabs()
            .map(<[Tab]>::to_vec)
            .unwrap_or_default();
        assert_eq!(right, [Tab::ToolStack]);
    }

    #[test]
    fn the_connections_panel_closes_and_reopens_on_the_left() {
        let mut dock = default_dock();
        assert!(connections_panel_open(&dock));

        set_connections_panel(&mut dock, false);
        assert!(!connections_panel_open(&dock));
        assert_eq!(dock.main_surface().tabs().count(), 5);

        set_connections_panel(&mut dock, true);
        assert!(connections_panel_open(&dock));
        let tree = dock.main_surface();
        let root = egui_dock::NodeIndex::root();
        assert!(tree[root].is_horizontal());
        let left: Vec<Tab> = tree[root.left()]
            .tabs()
            .map(<[Tab]>::to_vec)
            .unwrap_or_default();
        assert_eq!(left, [Tab::Connections]);
    }

    #[test]
    fn the_connections_sidebar_keeps_a_sane_width() {
        let mut dock = default_dock();
        let root = egui_dock::NodeIndex::root();
        let set = |dock: &mut egui_dock::DockState<Tab>, fraction: f32| {
            if let egui_dock::Node::Horizontal(split) = &mut dock.main_surface_mut()[root] {
                split.fraction = fraction;
            }
        };
        let get = |dock: &egui_dock::DockState<Tab>| match &dock.main_surface()[root] {
            egui_dock::Node::Horizontal(split) => split.fraction,
            _ => panic!("the root should be the sidebar split"),
        };

        set(&mut dock, 0.95);
        clamp_connections_width(&mut dock, 1000.0);
        assert!(get(&dock) <= 0.4 + 1e-4, "fraction {}", get(&dock));

        set(&mut dock, 0.01);
        clamp_connections_width(&mut dock, 1000.0);
        assert!(get(&dock) >= 0.2 - 1e-4, "fraction {}", get(&dock));
    }

    #[test]
    fn the_automatic_order_starts_with_opengl_everywhere_but_macos() {
        let auto = attempts(RendererChoice::Auto);
        if cfg!(target_os = "macos") {
            assert_eq!(&auto[0], &WGPU_ANY);
        } else {
            assert_eq!(&auto[0], &OPENGL);
        }
        // Never the only option: if the first one cannot start, something must follow.
        assert!(auto.len() >= 2);
    }

    #[test]
    fn an_explicit_choice_goes_first_and_opengl_stays_as_the_last_resort() {
        assert_eq!(attempts(RendererChoice::Glow), [OPENGL]);
        assert_eq!(attempts(RendererChoice::Wgpu), [WGPU_ANY, OPENGL]);
        assert_eq!(attempts(RendererChoice::Dx12), [DIRECTX_12, OPENGL]);
        assert_eq!(attempts(RendererChoice::Vulkan), [VULKAN, OPENGL]);
        assert_eq!(DIRECTX_12.backend, Some(eframe::wgpu::Backends::DX12));
        assert_eq!(VULKAN.backend, Some(eframe::wgpu::Backends::VULKAN));
        assert_eq!(OPENGL.renderer, eframe::Renderer::Glow);
    }

    #[test]
    fn a_failed_attempt_says_what_comes_next() {
        assert_eq!(what_is_next(None), "，没有别的渲染后端可试");
        assert_eq!(what_is_next(Some("wgpu")), "，改试 wgpu");
    }

    #[test]
    fn panic_messages_are_read_from_either_string_type() {
        let text: Box<dyn std::any::Any + Send> = Box::new("no GL config");
        assert_eq!(panic_message(text.as_ref()), "no GL config");
        let text: Box<dyn std::any::Any + Send> = Box::new(String::from("owned"));
        assert_eq!(panic_message(text.as_ref()), "owned");
        let opaque: Box<dyn std::any::Any + Send> = Box::new(42_u8);
        assert_eq!(panic_message(opaque.as_ref()), "（没有文字说明）");
    }

    #[test]
    fn the_default_layout_puts_the_sidebar_left_the_channels_right_and_the_log_below() {
        let dock = default_dock();
        let tree = dock.main_surface();
        // The tree is stored as a binary tree; `left` is the first child in layout order.
        let tabs_at = |index: egui_dock::NodeIndex| -> Vec<Tab> {
            tree[index].tabs().map(<[Tab]>::to_vec).unwrap_or_default()
        };
        let root = egui_dock::NodeIndex::root();
        assert!(tree[root].is_horizontal());
        assert_eq!(tabs_at(root.left()), [Tab::Connections]);

        let rest = root.right();
        assert!(tree[rest].is_horizontal());
        assert_eq!(tabs_at(rest.right()), [Tab::ToolStack]);

        let center = rest.left();
        assert!(tree[center].is_leaf());
        assert_eq!(
            tabs_at(center),
            [Tab::Waveform, Tab::Xy, Tab::Spectrum, Tab::Histogram]
        );
    }

    #[test]
    fn perf_report_summarises_the_frames() {
        let dir = std::env::temp_dir().join(format!("embdbg-perf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let report = dir.join("perf.txt");
        let test = PerfTest {
            started: None,
            seconds: 1.0,
            report: report.clone(),
            // 0.01 s, 0.02 s and 0.03 s: 60 frames per second on average.
            frame_times: vec![0.01, 0.02, 0.03],
            ui_times: vec![1.0, 2.0, 3.0],
        };
        test.write_report(&[("backend", "test".to_owned())])
            .unwrap();
        let text = std::fs::read_to_string(&report).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let value = |key: &str| {
            text.lines()
                .find_map(|line| {
                    line.strip_prefix(key)
                        .and_then(|rest| rest.trim_start_matches(" = ").parse::<f64>().ok())
                })
                .unwrap_or_else(|| panic!("{key} missing from:\n{text}"))
        };
        assert_eq!(value("frames"), 3.0);
        assert!((value("avg_fps") - 50.0).abs() < 1e-9);
        assert!((value("median_frame_ms") - 20.0).abs() < 1e-9);
        assert!((value("max_frame_ms") - 30.0).abs() < 1e-9);
        assert!((value("avg_ui_ms") - 2.0).abs() < 1e-9);
        // Intervals of 10, 20 and 30 ms land one each in their buckets.
        assert!(
            text.contains("intervals_ms = <=2:0 <=6:0 <=10:1 <=14:0 <=18:0 <=25:1 >25:1"),
            "{text}"
        );
    }

    #[test]
    fn the_interval_histogram_sorts_frames_into_buckets() {
        let histogram = interval_histogram(&[0.001, 0.004, 0.009, 0.016, 0.030]);
        assert_eq!(histogram[0], ("<=2", 1));
        assert_eq!(histogram[1], ("<=6", 1));
        assert_eq!(histogram[2], ("<=10", 1));
        assert_eq!(histogram[3], ("<=14", 0));
        assert_eq!(histogram[4], ("<=18", 1));
        assert_eq!(histogram[5], ("<=25", 0));
        assert_eq!(histogram[6], (">25", 1));
        assert!(interval_histogram(&[]).iter().all(|(_, count)| *count == 0));
    }
}
