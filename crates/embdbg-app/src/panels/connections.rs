//! The connection panel: one interface is picked at a time, and only its controls show.
//!
//! This follows the pattern VOFA+ uses: a transport selector at the top, the parameters of that
//! transport below it, and the list of open connections underneath. Only the chosen interface
//! takes space, so the panel stays readable as more transports are added.

use egui::{
    Button, ComboBox, DragValue, Frame, Grid, Margin, RichText, Stroke, TextEdit, Ui, vec2,
};
use embdbg_core::engine::{ConnectionState, Engine, TransportConfig};
use embdbg_core::pipeline::DecoderSpec;
use embdbg_core::plugins::{self, PluginField};
use embdbg_core::transport::{
    FlowChoice, ModbusBlock, ModbusConfig, ModbusEndpoint, MqttConfig, ParityChoice, PortInfo,
    SerialConfig, SimulatorConfig, TcpClientConfig, TcpServerConfig, UdpConfig, WsConfig,
    list_ports,
};

use crate::fonts::{icon, icon_label};
use crate::icons::outline;
use crate::settings::{ConnectionForm, Interface};
use crate::theme::{self, DANGER, FG_DIM, SPACE_1, SPACE_2, SPACE_3};
use crate::util::format_rate;

/// Sample rates offered for the simulator, per channel.
const RATES: [f64; 4] = [1e3, 1e4, 1e5, 1e6];

/// Baud rates the serial form offers.
const BAUDS: [u32; 7] = [9_600, 19_200, 38_400, 57_600, 115_200, 921_600, 3_000_000];

/// Batches the simulator delivers per second at any rate.
const BATCH_HZ: f64 = 50.0;

/// The name in the selector.
fn interface_label(interface: Interface) -> &'static str {
    match interface {
        Interface::Serial => "串口",
        Interface::TcpClient => "TCP 客户端",
        Interface::TcpServer => "TCP 服务器",
        Interface::Udp => "UDP",
        Interface::WebSocket => "WebSocket",
        Interface::Mqtt => "MQTT",
        Interface::ModbusTcp => "Modbus TCP",
        Interface::ModbusRtu => "Modbus RTU",
        Interface::Simulator => "模拟信号源",
    }
}

/// One line under the selector.
fn interface_hint(interface: Interface) -> &'static str {
    match interface {
        Interface::Serial => "USB 转串口、COM 口",
        Interface::TcpClient => "主动连接远端地址",
        Interface::TcpServer => "在本机监听，等待设备接入",
        Interface::Udp => "无连接收发，保留每条报文的边界",
        Interface::WebSocket => "ws:// 或 wss:// 地址，文本与二进制都收",
        Interface::Mqtt => "订阅主题收数据，发送发布到另一个主题",
        Interface::ModbusTcp => "按轮询表读寄存器，结果直接进通道",
        Interface::ModbusRtu => "串口上的 Modbus 从站",
        Interface::Simulator => "没有硬件时产生的 8 通道波形",
    }
}

/// What the user picked on the sidebar. Kept across frames.
pub struct ConnectionsState {
    /// Which form the panel shows.
    pub interface: Interface,
    /// Index into [`RATES`].
    pub rate: usize,
    /// Serial ports known at the last refresh.
    pub ports: Vec<PortInfo>,
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: ParityChoice,
    pub stop_bits: u8,
    pub flow: FlowChoice,
    pub host: String,
    pub client_port: String,
    pub bind: String,
    pub server_port: String,
    pub udp_port: String,
    pub udp_target: String,
    pub ws_url: String,
    pub mqtt_port: String,
    pub mqtt_subscribe: String,
    pub mqtt_publish: String,
    pub mqtt_user: String,
    pub mqtt_password: String,
    pub mqtt_qos: u8,
    pub modbus_slave: u8,
    pub modbus_interval: u64,
    pub modbus_port: String,
    pub modbus_blocks: String,
    /// Why the last open failed, shown in the panel instead of only in the log.
    last_error: Option<(Interface, String)>,
}

impl Default for ConnectionsState {
    fn default() -> Self {
        Self::from_form(ConnectionForm::default())
    }
}

impl ConnectionsState {
    /// Rebuilds the panel from what was saved. A port that has gone missing falls back to the
    /// first one that is there now, and values the form no longer offers fall back to defaults.
    pub fn from_form(form: ConnectionForm) -> Self {
        let ports = list_ports();
        let port = if ports.iter().any(|port| port.name == form.port) {
            form.port
        } else {
            ports
                .first()
                .map_or_else(String::new, |port| port.name.clone())
        };
        Self {
            interface: form.interface,
            rate: RATES
                .iter()
                .position(|rate| (rate - form.simulator_rate).abs() < 1.0)
                .unwrap_or(RATES.len() - 1),
            ports,
            port,
            baud: if BAUDS.contains(&form.baud) {
                form.baud
            } else {
                115_200
            },
            data_bits: if (5..=8).contains(&form.data_bits) {
                form.data_bits
            } else {
                8
            },
            parity: parse_parity(&form.parity),
            stop_bits: if form.stop_bits == 2 { 2 } else { 1 },
            flow: parse_flow(&form.flow),
            host: form.host,
            client_port: form.client_port,
            bind: form.bind,
            server_port: form.server_port,
            udp_port: form.udp_port,
            udp_target: form.udp_target,
            ws_url: form.ws_url,
            mqtt_port: form.mqtt_port,
            mqtt_subscribe: form.mqtt_subscribe,
            mqtt_publish: form.mqtt_publish,
            mqtt_user: form.mqtt_user,
            mqtt_password: form.mqtt_password,
            mqtt_qos: form.mqtt_qos,
            modbus_slave: form.modbus_slave,
            modbus_interval: form.modbus_interval,
            modbus_port: form.modbus_port,
            modbus_blocks: form.modbus_blocks,
            last_error: None,
        }
    }

    /// What to save for the next start.
    pub fn form(&self) -> ConnectionForm {
        ConnectionForm {
            interface: self.interface,
            port: self.port.clone(),
            baud: self.baud,
            data_bits: self.data_bits,
            parity: parity_name(self.parity).to_owned(),
            stop_bits: self.stop_bits,
            flow: flow_name(self.flow).to_owned(),
            host: self.host.clone(),
            client_port: self.client_port.clone(),
            bind: self.bind.clone(),
            server_port: self.server_port.clone(),
            udp_port: self.udp_port.clone(),
            udp_target: self.udp_target.clone(),
            ws_url: self.ws_url.clone(),
            mqtt_port: self.mqtt_port.clone(),
            mqtt_subscribe: self.mqtt_subscribe.clone(),
            mqtt_publish: self.mqtt_publish.clone(),
            mqtt_user: self.mqtt_user.clone(),
            mqtt_password: self.mqtt_password.clone(),
            mqtt_qos: self.mqtt_qos,
            modbus_slave: self.modbus_slave,
            modbus_interval: self.modbus_interval,
            modbus_port: self.modbus_port.clone(),
            modbus_blocks: self.modbus_blocks.clone(),
            simulator_rate: RATES[self.rate],
        }
    }

    pub fn simulator_config(&self) -> SimulatorConfig {
        SimulatorConfig {
            sample_rate: RATES[self.rate],
            batch_hz: BATCH_HZ,
        }
    }

    fn serial_config(&self) -> SerialConfig {
        SerialConfig {
            port: self.port.clone(),
            baud: self.baud,
            data_bits: self.data_bits,
            parity: self.parity,
            stop_bits: self.stop_bits,
            flow: self.flow,
        }
    }
}

/// What the panel did, so a failure can be written to the log.
pub enum Action {
    None,
    Failed(String),
}

pub fn ui(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState) -> Action {
    let mut action = Action::None;

    ui.add_space(SPACE_2);
    ui.label(RichText::new("协议与连接").strong());

    ui.add_space(SPACE_2);
    ui.label(RichText::new("数据接口").small().color(FG_DIM));
    let previous_interface = state.interface;
    ComboBox::from_id_salt("interface")
        .width(ui.available_width())
        .selected_text(interface_label(state.interface))
        .show_ui(ui, |ui| {
            for interface in [
                Interface::Serial,
                Interface::TcpClient,
                Interface::TcpServer,
                Interface::Udp,
                Interface::WebSocket,
                Interface::Mqtt,
                Interface::ModbusTcp,
                Interface::ModbusRtu,
                Interface::Simulator,
            ] {
                ui.selectable_value(&mut state.interface, interface, interface_label(interface));
            }
        });
    if state.interface != previous_interface {
        state.last_error = None;
    }
    ui.label(
        RichText::new(interface_hint(state.interface))
            .small()
            .color(FG_DIM),
    );

    // Only the chosen interface takes space.
    match state.interface {
        Interface::Serial => serial_form(ui, engine, state, &mut action),
        Interface::TcpClient => tcp_client_form(ui, engine, state, &mut action),
        Interface::TcpServer => tcp_server_form(ui, engine, state, &mut action),
        Interface::Udp => udp_form(ui, engine, state, &mut action),
        Interface::WebSocket => ws_form(ui, engine, state, &mut action),
        Interface::Mqtt => mqtt_form(ui, engine, state, &mut action),
        Interface::ModbusTcp => modbus_form(ui, engine, state, &mut action, true),
        Interface::ModbusRtu => modbus_form(ui, engine, state, &mut action, false),
        Interface::Simulator => simulator_form(ui, engine, state),
    }

    // A failed open is easy to miss when it only goes to the log tab, so the panel says why
    // right under the button that failed.
    if let Action::Failed(message) = &action {
        state.last_error = Some((state.interface, message.clone()));
    }
    if let Some((interface, message)) = &state.last_error
        && *interface == state.interface
    {
        ui.add_space(SPACE_1);
        Frame::new()
            .fill(DANGER.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0, DANGER.gamma_multiply(0.55)))
            .corner_radius(theme::RADIUS)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.label(RichText::new(message).small().color(DANGER));
            });
    }

    ui.add_space(SPACE_3);
    ui.separator();
    ui.add_space(SPACE_2);
    ui.label(RichText::new("连接").strong())
        .on_hover_text("点击连接名称切换；同一个接口换端口或地址可以再开一个");
    let active = engine.active_id();
    let connections = engine.connections();
    if connections.is_empty() {
        ui.label(RichText::new("还没有连接").small().color(FG_DIM));
    }
    let mut close: Option<u64> = None;
    let mut remove: Option<u64> = None;
    for connection in connections {
        let selected = active == Some(connection.id);
        ui.horizontal(|ui| {
            let label = RichText::new(connection.config.kind()).color(if selected {
                theme::FG
            } else {
                FG_DIM
            });
            if ui
                .selectable_label(selected, label)
                .on_hover_text("切换到这个连接")
                .clicked()
            {
                engine.set_active(connection.id);
            }
            let state = connection.state();
            ui.label(
                RichText::new(state.label())
                    .small()
                    .color(state_color(&state)),
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(SPACE_2 + 4.0);
            ui.label(
                RichText::new(connection.config.describe())
                    .small()
                    .color(FG_DIM),
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(SPACE_2 + 4.0);
            let is_open = connection.state().is_open();
            if is_open
                && ui
                    .add(Button::new(icon_label(outline::X, "关闭", 12.0)).small())
                    .clicked()
            {
                close = Some(connection.id);
            }
            if !is_open
                && ui
                    .add(Button::new(icon_label(outline::TRASH, "移除", 12.0)).small())
                    .on_hover_text("从列表里删掉，数据也一起清掉")
                    .clicked()
            {
                remove = Some(connection.id);
            }
            if matches!(connection.config, TransportConfig::TcpServer(_)) {
                ui.label(
                    RichText::new(format!("{} 个客户端", connection.clients()))
                        .small()
                        .color(FG_DIM),
                );
            }
            if ui
                .add(Button::new(icon_label(outline::ERASER, "清空", 12.0)).small())
                .clicked()
            {
                engine.clear(connection.id);
            }
        });
        ui.add_space(SPACE_1);
    }
    if let Some(id) = close {
        engine.close(id);
    }
    if let Some(id) = remove {
        engine.remove(id);
    }

    if let Some(connection) = engine.active() {
        ui.add_space(SPACE_2);
        ui.separator();
        ui.add_space(SPACE_2);
        ui.label(RichText::new("当前连接的协议").strong());
        if matches!(connection.config, TransportConfig::Modbus(_)) {
            ui.label(
                RichText::new("Modbus 数据直接进通道，不使用字节协议")
                    .small()
                    .color(FG_DIM),
            );
        } else {
            let mut spec = connection.decoder();
            ComboBox::from_id_salt("decoder")
                .width(ui.available_width())
                .selected_text(decoder_label(spec))
                .show_ui(ui, |ui| {
                    for candidate in [
                        DecoderSpec::Line,
                        DecoderSpec::JustFloat,
                        DecoderSpec::JustInt,
                    ] {
                        ui.selectable_value(&mut spec, candidate, decoder_label(candidate));
                    }
                    let installed = plugins::list();
                    if !installed.is_empty() {
                        ui.separator();
                        for plugin in installed {
                            let candidate = DecoderSpec::Plugin(plugin.slot);
                            ui.selectable_value(
                                &mut spec,
                                candidate,
                                format!("插件：{}", plugin.name),
                            );
                        }
                    }
                });
            if spec != connection.decoder() {
                connection.set_decoder(spec);
                // A plugin starts from its declared defaults; a built-in needs no options.
                connection.set_decoder_options(default_options(spec));
            }
            if let DecoderSpec::Plugin(slot) = spec
                && let Some(plugin) = plugins::get(slot)
            {
                plugin_fields(ui, &connection, &plugin.info.fields);
                let error = connection.shared.plugin_error.read().clone();
                if let Some(error) = error {
                    ui.label(RichText::new(error).small().color(DANGER));
                }
            }
        }
        if matches!(connection.config, TransportConfig::Simulator(_)) {
            ui.label(
                RichText::new("模拟信号源固定输出 JustFloat 数据")
                    .small()
                    .color(FG_DIM),
            );
        }
    }

    action
}

/// The id of an open connection matching the form, so its button can act as a toggle.
fn open_matching(engine: &Engine, matches: impl Fn(&TransportConfig) -> bool) -> Option<u64> {
    engine
        .connections()
        .into_iter()
        .find(|connection| {
            matches!(
                connection.state(),
                ConnectionState::Connecting | ConnectionState::Open
            ) && matches(&connection.config)
        })
        .map(|connection| connection.id)
}

/// A full-width primary button: accent while closed, red while open, labels included.
fn primary_button(
    ui: &mut Ui,
    open: Option<u64>,
    open_label: &str,
    open_icon: &str,
    close_label: &str,
    enabled: bool,
) -> bool {
    let closing = open.is_some();
    let button = Button::new(icon_label(
        if closing { outline::X } else { open_icon },
        if closing { close_label } else { open_label },
        13.0,
    ))
    .min_size(vec2(ui.available_width(), 0.0))
    .fill(if closing {
        theme::DANGER.gamma_multiply(0.22)
    } else {
        theme::ACCENT.gamma_multiply(0.30)
    });
    ui.add_enabled(enabled, button).clicked()
}

/// Serial: the port, its line settings, and the button that opens it.
fn serial_form(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState, action: &mut Action) {
    Grid::new("serial-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("端口");
            ui.horizontal(|ui| {
                // The refresh button goes first so it never gets clipped on a narrow panel; the
                // port selector takes whatever width is left.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Button::new(icon(outline::REFRESH, 14.0)))
                        .on_hover_text("重新扫描串口")
                        .clicked()
                    {
                        state.ports = list_ports();
                        if !state.ports.iter().any(|port| port.name == state.port) {
                            state.port = state
                                .ports
                                .first()
                                .map_or_else(String::new, |port| port.name.clone());
                        }
                    }
                    ComboBox::from_id_salt("serial-port")
                        .width(ui.available_width())
                        .selected_text(if state.port.is_empty() {
                            "选择端口".to_owned()
                        } else {
                            state.port.clone()
                        })
                        .show_ui(ui, |ui| {
                            for port in &state.ports {
                                ui.selectable_value(&mut state.port, port.name.clone(), &port.name)
                                    .on_hover_text(&port.details);
                            }
                        });
                });
            });
            ui.end_row();

            ui.label("波特率");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("serial-baud")
                    .width(90.0)
                    .selected_text(state.baud.to_string())
                    .show_ui(ui, |ui| {
                        for baud in BAUDS {
                            ui.selectable_value(&mut state.baud, baud, baud.to_string());
                        }
                    });
            });
            ui.end_row();

            ui.label("数据位");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("data-bits")
                    .width(90.0)
                    .selected_text(state.data_bits.to_string())
                    .show_ui(ui, |ui| {
                        for bits in [5, 6, 7, 8] {
                            ui.selectable_value(&mut state.data_bits, bits, bits.to_string());
                        }
                    });
            });
            ui.end_row();

            ui.label("校验位");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("parity")
                    .width(90.0)
                    .selected_text(parity_label(state.parity))
                    .show_ui(ui, |ui| {
                        for parity in [ParityChoice::None, ParityChoice::Odd, ParityChoice::Even] {
                            ui.selectable_value(&mut state.parity, parity, parity_label(parity));
                        }
                    });
            });
            ui.end_row();

            ui.label("停止位");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("stop-bits")
                    .width(90.0)
                    .selected_text(state.stop_bits.to_string())
                    .show_ui(ui, |ui| {
                        for bits in [1, 2] {
                            ui.selectable_value(&mut state.stop_bits, bits, bits.to_string());
                        }
                    });
            });
            ui.end_row();

            ui.label("流控");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("flow")
                    .width(90.0)
                    .selected_text(flow_label(state.flow))
                    .show_ui(ui, |ui| {
                        for flow in [FlowChoice::None, FlowChoice::Software, FlowChoice::Hardware] {
                            ui.selectable_value(&mut state.flow, flow, flow_label(flow));
                        }
                    });
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let existing = open_matching(engine, |config| match config {
        TransportConfig::Serial(serial) => serial.port == state.port,
        _ => false,
    });
    let can_open = existing.is_some() || !state.port.is_empty();
    if primary_button(
        ui,
        existing,
        "打开串口",
        outline::PLUG_CONNECTED,
        "关闭串口",
        can_open,
    ) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else if let Err(error) = engine.open(TransportConfig::Serial(state.serial_config())) {
            *action = Action::Failed(error);
        }
    }
    if existing.is_some() {
        ui.label(
            RichText::new("已打开；改参数请先关闭，换一个端口可同时打开多个串口")
                .small()
                .color(FG_DIM),
        );
    }
}

/// TCP client: where to connect and the button that does it.
fn tcp_client_form(
    ui: &mut Ui,
    engine: &Engine,
    state: &mut ConnectionsState,
    action: &mut Action,
) {
    Grid::new("tcp-client-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("地址");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.host).desired_width(96.0));
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut state.client_port).desired_width(56.0));
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let port = parse_port(&state.client_port).ok();
    let existing = port.and_then(|port| {
        open_matching(engine, |config| {
            matches!(config, TransportConfig::TcpClient(client)
                if client.host == state.host && client.port == port)
        })
    });
    if primary_button(ui, existing, "连接", outline::WORLD_WWW, "断开", true) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else {
            match parse_port(&state.client_port) {
                Ok(port) => {
                    let config = TcpClientConfig {
                        host: state.host.clone(),
                        port,
                        connect_timeout_ms: 3000,
                    };
                    if let Err(error) = engine.open(TransportConfig::TcpClient(config)) {
                        *action = Action::Failed(error);
                    }
                }
                Err(error) => *action = Action::Failed(error),
            }
        }
    }
}

/// TCP server: what to bind and the button that starts listening.
fn tcp_server_form(
    ui: &mut Ui,
    engine: &Engine,
    state: &mut ConnectionsState,
    action: &mut Action,
) {
    Grid::new("tcp-server-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("绑定");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.bind).desired_width(96.0));
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut state.server_port).desired_width(56.0));
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let port = parse_port(&state.server_port).ok();
    let existing = port.and_then(|port| {
        open_matching(engine, |config| {
            matches!(config, TransportConfig::TcpServer(server)
                if server.bind == state.bind && server.port == port)
        })
    });
    if primary_button(ui, existing, "监听", outline::SERVER, "停止监听", true) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else {
            match parse_port(&state.server_port) {
                Ok(port) => {
                    let config = TcpServerConfig {
                        bind: state.bind.clone(),
                        port,
                    };
                    if let Err(error) = engine.open(TransportConfig::TcpServer(config)) {
                        *action = Action::Failed(error);
                    }
                }
                Err(error) => *action = Action::Failed(error),
            }
        }
    }
}

/// UDP: a local port to listen on and an optional place to send.
fn udp_form(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState, action: &mut Action) {
    Grid::new("udp-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("本地端口");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.udp_port).desired_width(96.0));
            });
            ui.end_row();

            ui.label("目标地址");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.udp_target)
                        .desired_width(150.0)
                        .hint_text("host:port，可留空"),
                );
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let port = parse_port(&state.udp_port).ok();
    let existing = port.and_then(|port| {
        open_matching(
            engine,
            |config| matches!(config, TransportConfig::Udp(udp) if udp.port == port),
        )
    });
    if primary_button(
        ui,
        existing,
        "打开 UDP",
        outline::BROADCAST,
        "关闭 UDP",
        true,
    ) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else {
            match parse_port(&state.udp_port) {
                Ok(port) => {
                    let config = UdpConfig {
                        bind: "0.0.0.0".to_owned(),
                        port,
                        target: Some(state.udp_target.clone())
                            .filter(|target| !target.trim().is_empty()),
                    };
                    if let Err(error) = engine.open(TransportConfig::Udp(config)) {
                        *action = Action::Failed(error);
                    }
                }
                Err(error) => *action = Action::Failed(error),
            }
        }
    }
}

/// WebSocket: one URL and the button that connects.
fn ws_form(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState, action: &mut Action) {
    Grid::new("ws-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("地址");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.ws_url)
                        .desired_width(150.0)
                        .hint_text("ws://host:port/"),
                );
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let url = state.ws_url.trim().to_owned();
    let existing = open_matching(
        engine,
        |config| matches!(config, TransportConfig::WebSocket(ws) if ws.url == url),
    );
    if primary_button(ui, existing, "连接", outline::WORLD_WWW, "断开", true) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else if url.is_empty() {
            *action = Action::Failed("WebSocket 地址不能为空".to_owned());
        } else if let Err(error) =
            engine.open(TransportConfig::WebSocket(WsConfig { url: url.clone() }))
        {
            *action = Action::Failed(error);
        }
    }
}

/// MQTT: broker, topics and the optional credentials.
fn mqtt_form(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState, action: &mut Action) {
    Grid::new("mqtt-params")
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            ui.label("服务器");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.host).desired_width(96.0));
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut state.mqtt_port).desired_width(50.0));
            });
            ui.end_row();

            ui.label("订阅");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.mqtt_subscribe)
                        .desired_width(150.0)
                        .hint_text("embdbg/#"),
                );
            });
            ui.end_row();

            ui.label("发布");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.mqtt_publish)
                        .desired_width(150.0)
                        .hint_text("embdbg/tx"),
                );
            });
            ui.end_row();

            ui.label("用户名");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.mqtt_user)
                        .desired_width(150.0)
                        .hint_text("可留空"),
                );
            });
            ui.end_row();

            ui.label("密码");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.mqtt_password)
                        .desired_width(150.0)
                        .password(true),
                );
            });
            ui.end_row();

            ui.label("QoS");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("mqtt-qos")
                    .width(90.0)
                    .selected_text(state.mqtt_qos.to_string())
                    .show_ui(ui, |ui| {
                        for qos in [0, 1, 2] {
                            ui.selectable_value(&mut state.mqtt_qos, qos, qos.to_string());
                        }
                    });
            });
            ui.end_row();
        });

    ui.add_space(SPACE_1);
    let port = parse_port(&state.mqtt_port).ok();
    let existing = port.and_then(|port| {
        open_matching(engine, |config| {
            matches!(config, TransportConfig::Mqtt(mqtt)
                if mqtt.host == state.host
                    && mqtt.port == port
                    && mqtt.subscribe == state.mqtt_subscribe)
        })
    });
    if primary_button(ui, existing, "连接", outline::BROADCAST, "断开", true) {
        state.last_error = None;
        if let Some(id) = existing {
            engine.close(id);
        } else {
            match parse_port(&state.mqtt_port) {
                Ok(port) => {
                    let config = MqttConfig {
                        host: state.host.clone(),
                        port,
                        client_id: String::new(),
                        username: Some(state.mqtt_user.clone())
                            .filter(|user| !user.trim().is_empty()),
                        password: Some(state.mqtt_password.clone())
                            .filter(|password| !password.is_empty()),
                        subscribe: state.mqtt_subscribe.clone(),
                        publish: state.mqtt_publish.clone(),
                        qos: state.mqtt_qos.min(2),
                    };
                    if let Err(error) = engine.open(TransportConfig::Mqtt(config)) {
                        *action = Action::Failed(error);
                    }
                }
                Err(error) => *action = Action::Failed(error),
            }
        }
    }
}

/// Modbus: a polling table and where the slave is.
fn modbus_form(
    ui: &mut Ui,
    engine: &Engine,
    state: &mut ConnectionsState,
    action: &mut Action,
    tcp: bool,
) {
    Grid::new(if tcp {
        "modbus-tcp-params"
    } else {
        "modbus-rtu-params"
    })
    .num_columns(2)
    .spacing([SPACE_2, SPACE_1])
    .show(ui, |ui| {
        if tcp {
            ui.label("服务器");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.host).desired_width(96.0));
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut state.modbus_port).desired_width(50.0));
            });
            ui.end_row();
        } else {
            ui.label("串口");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("modbus-serial-port")
                    .width(96.0)
                    .selected_text(if state.port.is_empty() {
                        "选择端口".to_owned()
                    } else {
                        state.port.clone()
                    })
                    .show_ui(ui, |ui| {
                        for port in &state.ports {
                            ui.selectable_value(&mut state.port, port.name.clone(), &port.name)
                                .on_hover_text(&port.details);
                        }
                    });
            });
            ui.end_row();

            ui.label("波特率");
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("modbus-baud")
                    .width(96.0)
                    .selected_text(state.baud.to_string())
                    .show_ui(ui, |ui| {
                        for baud in BAUDS {
                            ui.selectable_value(&mut state.baud, baud, baud.to_string());
                        }
                    });
            });
            ui.end_row();
        }

        ui.label("从站");
        ui.horizontal(|ui| {
            ui.add(DragValue::new(&mut state.modbus_slave).range(1..=247));
        });
        ui.end_row();

        ui.label("周期");
        ui.horizontal(|ui| {
            ui.add(
                DragValue::new(&mut state.modbus_interval)
                    .range(50..=60_000)
                    .suffix(" ms"),
            );
        });
        ui.end_row();
    });

    ui.add_space(SPACE_1);
    ui.label(
        RichText::new("轮询表（每行：类型 起始 数量 [名称]）")
            .small()
            .color(FG_DIM),
    );
    ui.add(
        egui::TextEdit::multiline(&mut state.modbus_blocks)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(4)
            .hint_text("holding 0 8 电压"),
    );

    ui.add_space(SPACE_1);
    let typed_port = if tcp {
        parse_port(&state.modbus_port).ok()
    } else {
        None
    };
    let existing = open_matching(engine, |config| match config {
        TransportConfig::Modbus(modbus) => match (&modbus.endpoint, tcp) {
            (ModbusEndpoint::Tcp { host, port }, true) => {
                Some(*port) == typed_port
                    && host == &state.host
                    && modbus.slave == state.modbus_slave
            }
            (ModbusEndpoint::Rtu { port, .. }, false) => {
                port == &state.port && modbus.slave == state.modbus_slave
            }
            _ => false,
        },
        _ => false,
    });
    if !primary_button(ui, existing, "连接", outline::PLUG_CONNECTED, "断开", true) {
        return;
    }
    state.last_error = None;
    if let Some(id) = existing {
        engine.close(id);
        return;
    }
    let blocks: Result<Vec<_>, String> = state
        .modbus_blocks
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            ModbusBlock::parse_spec(line)
                .map_err(|error| format!("轮询表第 {} 行：{error}", index + 1))
        })
        .collect();
    let blocks = match blocks {
        Ok(blocks) if blocks.is_empty() => {
            *action = Action::Failed("轮询表不能为空".to_owned());
            return;
        }
        Ok(blocks) => blocks,
        Err(error) => {
            *action = Action::Failed(error);
            return;
        }
    };
    let endpoint = if tcp {
        match parse_port(&state.modbus_port) {
            Ok(port) => ModbusEndpoint::Tcp {
                host: state.host.clone(),
                port,
            },
            Err(error) => {
                *action = Action::Failed(error);
                return;
            }
        }
    } else {
        if state.port.trim().is_empty() {
            *action = Action::Failed("先选择串口".to_owned());
            return;
        }
        ModbusEndpoint::Rtu {
            port: state.port.clone(),
            baud: state.baud,
        }
    };
    let config = ModbusConfig {
        endpoint,
        slave: state.modbus_slave,
        interval_ms: state.modbus_interval,
        blocks,
    };
    if let Err(error) = engine.open(TransportConfig::Modbus(config)) {
        *action = Action::Failed(error);
    }
}

/// The built-in generator: a stand-in for hardware.
fn simulator_form(ui: &mut Ui, engine: &Engine, state: &mut ConnectionsState) {
    let running = engine.simulator_running();
    let (button_icon, button_text) = if running {
        (outline::PLAYER_STOP, "停止模拟信号源")
    } else {
        (outline::PLAYER_PLAY, "启动模拟信号源")
    };
    let button = Button::new(icon_label(button_icon, button_text, 13.0))
        .min_size(vec2(ui.available_width(), 0.0))
        .fill(if running {
            theme::BG_HOVER
        } else {
            theme::ACCENT.gamma_multiply(0.30)
        });
    if ui.add(button).clicked() {
        if running {
            engine.stop_simulator();
        } else {
            engine.start_simulator(state.simulator_config());
        }
    }
    ui.add_space(SPACE_1);
    ComboBox::from_id_salt("simulator-rate")
        .width(ui.available_width())
        .selected_text(format_rate(RATES[state.rate], "样本/s"))
        .show_ui(ui, |ui| {
            for (index, rate) in RATES.iter().enumerate() {
                ui.selectable_value(&mut state.rate, index, format_rate(*rate, "样本/s"));
            }
        });
    ui.label(
        RichText::new("8 通道 · JustFloat · 只进入波形类视图")
            .small()
            .color(FG_DIM),
    );
}

fn parity_label(parity: ParityChoice) -> &'static str {
    match parity {
        ParityChoice::None => "无",
        ParityChoice::Odd => "奇",
        ParityChoice::Even => "偶",
    }
}

fn parity_name(parity: ParityChoice) -> &'static str {
    match parity {
        ParityChoice::None => "none",
        ParityChoice::Odd => "odd",
        ParityChoice::Even => "even",
    }
}

fn parse_parity(text: &str) -> ParityChoice {
    match text {
        "odd" => ParityChoice::Odd,
        "even" => ParityChoice::Even,
        _ => ParityChoice::None,
    }
}

fn flow_label(flow: FlowChoice) -> &'static str {
    match flow {
        FlowChoice::None => "无",
        FlowChoice::Software => "软件",
        FlowChoice::Hardware => "硬件",
    }
}

fn flow_name(flow: FlowChoice) -> &'static str {
    match flow {
        FlowChoice::None => "none",
        FlowChoice::Software => "software",
        FlowChoice::Hardware => "hardware",
    }
}

fn parse_flow(text: &str) -> FlowChoice {
    match text {
        "software" => FlowChoice::Software,
        "hardware" => FlowChoice::Hardware,
        _ => FlowChoice::None,
    }
}

fn decoder_label(spec: DecoderSpec) -> String {
    match spec {
        DecoderSpec::Line => "文本行（终端协议）".to_owned(),
        DecoderSpec::JustFloat => "JustFloat（f32 数组 + 帧尾）".to_owned(),
        DecoderSpec::JustInt => "JustInt（i32 数组 + 帧尾）".to_owned(),
        DecoderSpec::Plugin(slot) => plugins::get(slot).map_or_else(
            || format!("插件 {slot}（未安装）"),
            |plugin| format!("插件：{}", plugin.info.name),
        ),
    }
}

/// A plugin's declared defaults, as the JSON handed to `set_params`.
fn default_options(spec: DecoderSpec) -> String {
    match spec {
        DecoderSpec::Plugin(slot) => plugins::default_params(slot),
        _ => String::new(),
    }
}

/// Renders the manifest's fields and writes the values back as JSON.
fn plugin_fields(
    ui: &mut Ui,
    connection: &embdbg_core::engine::Connection,
    fields: &[PluginField],
) {
    if fields.is_empty() {
        return;
    }
    let mut values: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&connection.decoder_options()).unwrap_or_default();
    let mut changed = false;

    Grid::new(("plugin-fields", connection.id))
        .num_columns(2)
        .spacing([SPACE_2, SPACE_1])
        .show(ui, |ui| {
            for field in fields {
                ui.label(&field.label);
                let entry = values
                    .entry(field.key.clone())
                    .or_insert_with(|| field.default.clone());
                match field.kind.as_str() {
                    "number" => {
                        let mut value = entry.as_f64().unwrap_or(0.0);
                        let mut widget = DragValue::new(&mut value).speed(1.0);
                        if field.min.is_some() || field.max.is_some() {
                            widget = widget
                                .range(field.min.unwrap_or(-1.0e12)..=field.max.unwrap_or(1.0e12));
                        }
                        if ui.add(widget).changed() {
                            *entry = serde_json::json!(value);
                            changed = true;
                        }
                    }
                    "integer" => {
                        let mut value = entry.as_i64().unwrap_or(0);
                        let mut widget = DragValue::new(&mut value).speed(1.0);
                        if field.min.is_some() || field.max.is_some() {
                            widget = widget.range(
                                field.min.unwrap_or(i64::MIN as f64) as i64
                                    ..=field.max.unwrap_or(i64::MAX as f64) as i64,
                            );
                        }
                        if ui.add(widget).changed() {
                            *entry = serde_json::json!(value);
                            changed = true;
                        }
                    }
                    "bool" => {
                        let mut value = entry.as_bool().unwrap_or(false);
                        if ui.checkbox(&mut value, "").changed() {
                            *entry = serde_json::json!(value);
                            changed = true;
                        }
                    }
                    "choice" => {
                        let mut value = entry.as_str().unwrap_or_default().to_owned();
                        let before = value.clone();
                        ComboBox::from_id_salt(("plugin-field", connection.id, &field.key))
                            .width(ui.available_width())
                            .selected_text(if value.is_empty() {
                                "选择".to_owned()
                            } else {
                                value.clone()
                            })
                            .show_ui(ui, |ui| {
                                for option in &field.options {
                                    ui.selectable_value(&mut value, option.clone(), option);
                                }
                            });
                        if value != before {
                            *entry = serde_json::json!(value);
                            changed = true;
                        }
                    }
                    _ => {
                        let mut value = entry.as_str().unwrap_or_default().to_owned();
                        if ui
                            .add(
                                TextEdit::singleline(&mut value)
                                    .desired_width(ui.available_width()),
                            )
                            .changed()
                        {
                            *entry = serde_json::json!(value);
                            changed = true;
                        }
                    }
                }
                ui.end_row();
            }
        });

    if changed {
        connection.set_decoder_options(serde_json::Value::Object(values).to_string());
    }
}

fn state_color(state: &ConnectionState) -> egui::Color32 {
    match state {
        ConnectionState::Connecting => theme::WARNING,
        ConnectionState::Open => theme::SUCCESS,
        ConnectionState::Closed => FG_DIM,
        ConnectionState::Failed(_) => DANGER,
    }
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.trim()
        .parse::<u16>()
        .map_err(|_| format!("端口号无效：{text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_must_be_numbers_in_range() {
        assert_eq!(parse_port("8080"), Ok(8080));
        assert_eq!(parse_port(" 21 "), Ok(21));
        assert!(parse_port("80800").is_err());
        assert!(parse_port("").is_err());
        assert!(parse_port("abc").is_err());
    }

    #[test]
    fn the_simulator_defaults_to_the_fastest_rate() {
        let state = ConnectionsState::default();
        assert_eq!(state.simulator_config().sample_rate, 1e6);
    }

    #[test]
    fn the_panel_starts_on_the_serial_form() {
        assert_eq!(ConnectionsState::default().interface, Interface::Serial);
    }

    #[test]
    fn every_interface_and_decoder_has_a_legible_name() {
        for interface in [
            Interface::Serial,
            Interface::TcpClient,
            Interface::TcpServer,
            Interface::Simulator,
        ] {
            assert!(!interface_label(interface).is_empty());
            assert!(!interface_hint(interface).is_empty());
        }
        for spec in [
            DecoderSpec::Line,
            DecoderSpec::JustFloat,
            DecoderSpec::JustInt,
        ] {
            assert!(!decoder_label(spec).is_empty());
        }
    }

    #[test]
    fn the_form_round_trips_through_the_panel() {
        let first = ConnectionsState::default();
        let second = ConnectionsState::from_form(first.form());
        assert_eq!(second.form(), first.form());

        let state = ConnectionsState::from_form(ConnectionForm {
            interface: Interface::TcpServer,
            bind: "127.0.0.1".to_owned(),
            server_port: "9999".to_owned(),
            parity: "even".to_owned(),
            flow: "hardware".to_owned(),
            ..ConnectionForm::default()
        });
        assert_eq!(state.interface, Interface::TcpServer);
        assert_eq!(state.bind, "127.0.0.1");
        assert_eq!(state.server_port, "9999");
        assert_eq!(state.parity, ParityChoice::Even);
        assert_eq!(state.flow, FlowChoice::Hardware);
    }

    #[test]
    fn values_the_form_no_longer_offers_fall_back_to_defaults() {
        let state = ConnectionsState::from_form(ConnectionForm {
            baud: 123_456,
            data_bits: 9,
            stop_bits: 7,
            parity: "unknown".to_owned(),
            flow: "unknown".to_owned(),
            ..ConnectionForm::default()
        });
        assert_eq!(state.baud, 115_200);
        assert_eq!(state.data_bits, 8);
        assert_eq!(state.stop_bits, 1);
        assert_eq!(state.parity, ParityChoice::None);
        assert_eq!(state.flow, FlowChoice::None);
    }

    #[test]
    fn serial_settings_reach_the_config() {
        let state = ConnectionsState {
            port: "COM9".to_owned(),
            baud: 921_600,
            data_bits: 7,
            parity: ParityChoice::Even,
            stop_bits: 2,
            flow: FlowChoice::Hardware,
            ..ConnectionsState::default()
        };
        let config = state.serial_config();
        assert_eq!(config.port, "COM9");
        assert_eq!(config.baud, 921_600);
        assert_eq!(config.data_bits, 7);
        assert_eq!(config.parity, ParityChoice::Even);
        assert_eq!(config.stop_bits, 2);
        assert_eq!(config.flow, FlowChoice::Hardware);
    }
}
