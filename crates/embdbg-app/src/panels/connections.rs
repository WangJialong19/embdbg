//! The sidebar: opening connections, choosing the active one, and switching its protocol.

use egui::{Button, ComboBox, RichText, Ui, vec2};
use embdbg_core::engine::{ConnectionState, Engine, TransportConfig};
use embdbg_core::pipeline::DecoderSpec;
use embdbg_core::transport::{
    PortInfo, SerialConfig, SimulatorConfig, TcpClientConfig, TcpServerConfig, list_ports,
};

use crate::fonts::{icon, icon_label};
use crate::icons::outline;
use crate::theme::{self, DANGER, FG_DIM, SPACE_1, SPACE_2, SPACE_3};
use crate::util::format_rate;

/// Sample rates offered for the simulator, per channel.
const RATES: [f64; 4] = [1e3, 1e4, 1e5, 1e6];

/// Baud rates the serial form offers.
const BAUDS: [u32; 7] = [9_600, 19_200, 38_400, 57_600, 115_200, 921_600, 3_000_000];

/// Batches the simulator delivers per second at any rate.
const BATCH_HZ: f64 = 50.0;

/// What the user picked on the sidebar. Kept across frames.
pub struct ConnectionsState {
    /// Index into [`RATES`].
    pub rate: usize,
    /// Serial ports known at the last refresh.
    pub ports: Vec<PortInfo>,
    pub port: String,
    pub baud: u32,
    pub host: String,
    pub client_port: String,
    pub bind: String,
    pub server_port: String,
}

impl Default for ConnectionsState {
    fn default() -> Self {
        let ports = list_ports();
        let port = ports
            .first()
            .map_or_else(String::new, |port| port.name.clone());
        Self {
            rate: RATES.len() - 1,
            ports,
            port,
            baud: 115_200,
            host: "127.0.0.1".to_owned(),
            client_port: "8080".to_owned(),
            bind: "0.0.0.0".to_owned(),
            server_port: "8080".to_owned(),
        }
    }
}

impl ConnectionsState {
    pub fn simulator_config(&self) -> SimulatorConfig {
        SimulatorConfig {
            sample_rate: RATES[self.rate],
            batch_hz: BATCH_HZ,
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
    ui.label(RichText::new("模拟信号源").strong());
    let running = engine.simulator_running();
    let (button_icon, button_text) = if running {
        (outline::PLAYER_STOP, "停止模拟信号源")
    } else {
        (outline::PLAYER_PLAY, "启动模拟信号源")
    };
    let button = Button::new(icon_label(button_icon, button_text, 13.5))
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
    ComboBox::from_id_salt("simulator-rate")
        .selected_text(format_rate(RATES[state.rate], "样本/s"))
        .show_ui(ui, |ui| {
            for (index, rate) in RATES.iter().enumerate() {
                ui.selectable_value(&mut state.rate, index, format_rate(*rate, "样本/s"));
            }
        });

    ui.add_space(SPACE_2);
    ui.separator();
    ui.add_space(SPACE_2);
    ui.label(RichText::new("新建串口").strong());
    ui.horizontal(|ui| {
        ComboBox::from_id_salt("serial-port")
            .width((ui.available_width() - 40.0).max(80.0))
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
    });
    ui.horizontal(|ui| {
        ComboBox::from_id_salt("serial-baud")
            .selected_text(state.baud.to_string())
            .show_ui(ui, |ui| {
                for baud in BAUDS {
                    ui.selectable_value(&mut state.baud, baud, baud.to_string());
                }
            });
        let can_open = !state.port.is_empty();
        if ui
            .add_enabled(can_open, Button::new("打开"))
            .on_disabled_hover_text("先选择端口")
            .clicked()
        {
            let config = SerialConfig {
                port: state.port.clone(),
                baud: state.baud,
                ..SerialConfig::default()
            };
            if let Err(error) = engine.open(TransportConfig::Serial(config)) {
                action = Action::Failed(error);
            }
        }
    });

    ui.add_space(SPACE_2);
    ui.label(RichText::new("新建 TCP 客户端").strong());
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.host).desired_width(90.0));
        ui.label(":");
        ui.add(egui::TextEdit::singleline(&mut state.client_port).desired_width(48.0));
        if ui.button("连接").clicked() {
            match parse_port(&state.client_port) {
                Ok(port) => {
                    let config = TcpClientConfig {
                        host: state.host.clone(),
                        port,
                        connect_timeout_ms: 3000,
                    };
                    if let Err(error) = engine.open(TransportConfig::TcpClient(config)) {
                        action = Action::Failed(error);
                    }
                }
                Err(error) => action = Action::Failed(error),
            }
        }
    });

    ui.add_space(SPACE_2);
    ui.label(RichText::new("新建 TCP 服务器").strong());
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.bind).desired_width(90.0));
        ui.label(":");
        ui.add(egui::TextEdit::singleline(&mut state.server_port).desired_width(48.0));
        if ui.button("监听").clicked() {
            match parse_port(&state.server_port) {
                Ok(port) => {
                    let config = TcpServerConfig {
                        bind: state.bind.clone(),
                        port,
                    };
                    if let Err(error) = engine.open(TransportConfig::TcpServer(config)) {
                        action = Action::Failed(error);
                    }
                }
                Err(error) => action = Action::Failed(error),
            }
        }
    });

    ui.add_space(SPACE_3);
    ui.separator();
    ui.add_space(SPACE_2);
    ui.label(RichText::new("连接").strong());
    let active = engine.active_id();
    let connections = engine.connections();
    if connections.is_empty() {
        ui.label(RichText::new("还没有连接").small().color(FG_DIM));
    }
    let mut close: Option<u64> = None;
    for connection in connections {
        let selected = active == Some(connection.id);
        ui.horizontal(|ui| {
            let label = RichText::new(connection.config.kind()).color(if selected {
                theme::FG
            } else {
                FG_DIM
            });
            if ui.selectable_label(selected, label).clicked() {
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

    if let Some(connection) = engine.active() {
        ui.add_space(SPACE_2);
        ui.separator();
        ui.add_space(SPACE_2);
        ui.label(RichText::new("当前连接的协议").strong());
        let mut spec = connection.decoder();
        ComboBox::from_id_salt("decoder")
            .selected_text(decoder_label(spec))
            .show_ui(ui, |ui| {
                for candidate in [
                    DecoderSpec::Line,
                    DecoderSpec::JustFloat,
                    DecoderSpec::JustInt,
                ] {
                    ui.selectable_value(&mut spec, candidate, decoder_label(candidate));
                }
            });
        if spec != connection.decoder() {
            connection.set_decoder(spec);
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

fn decoder_label(spec: DecoderSpec) -> &'static str {
    match spec {
        DecoderSpec::Line => "文本行（终端协议）",
        DecoderSpec::JustFloat => "JustFloat（f32 数组 + 帧尾）",
        DecoderSpec::JustInt => "JustInt（i32 数组 + 帧尾）",
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
    fn every_decoder_has_a_legible_name() {
        for spec in [
            DecoderSpec::Line,
            DecoderSpec::JustFloat,
            DecoderSpec::JustInt,
        ] {
            assert!(!decoder_label(spec).is_empty());
        }
    }
}
