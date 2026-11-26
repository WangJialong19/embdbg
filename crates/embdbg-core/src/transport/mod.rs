//! Sources of bytes: real hardware, sockets and the built-in simulator.

pub mod serial;
pub mod sim;
pub mod tcp;
pub mod udp;
pub mod ws;

pub use serial::{
    FlowChoice, ParityChoice, PortInfo, SerialConfig, SerialEvent, SerialLink, list_ports,
};
pub use sim::{SIM_CHANNEL_NAMES, SIM_CHANNELS, Simulator, SimulatorConfig};
pub use tcp::{
    ClientSink, ClientSinkFactory, TcpClientConfig, TcpEvent, TcpLink, TcpServerConfig,
    spawn_client, spawn_server,
};
pub use udp::{UdpConfig, UdpEvent, UdpLink};
pub use ws::{WsConfig, WsEvent, WsLink};
