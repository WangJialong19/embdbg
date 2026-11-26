//! Serial ports.
//!
//! The port API blocks, so each port gets a thread of its own: it reads with a short timeout,
//! forwards whatever arrived, and writes what the UI asked for between reads.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::clock::Clock;

/// How the port is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    pub port: String,
    pub baud: u32,
    /// 5, 6, 7 or 8.
    pub data_bits: u8,
    pub parity: ParityChoice,
    /// 1 or 2.
    pub stop_bits: u8,
    pub flow: FlowChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParityChoice {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlowChoice {
    #[default]
    None,
    Software,
    Hardware,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud: 115_200,
            data_bits: 8,
            parity: ParityChoice::None,
            stop_bits: 1,
            flow: FlowChoice::None,
        }
    }
}

impl SerialConfig {
    /// What the connection list shows.
    pub fn describe(&self) -> String {
        format!("{} @ {}", self.port, self.baud)
    }
}

/// One entry of the port list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    /// USB ids or the plain port type, for telling two adapters apart.
    pub details: String,
}

/// Lists the serial ports the system knows about.
pub fn list_ports() -> Vec<PortInfo> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    ports
        .into_iter()
        .map(|port| {
            let details = match &port.port_type {
                serialport::SerialPortType::UsbPort(usb) => {
                    let mut details = format!("USB {:04X}:{:04X}", usb.vid, usb.pid);
                    if let Some(product) = &usb.product {
                        details.push_str(" · ");
                        details.push_str(product);
                    }
                    details
                }
                serialport::SerialPortType::BluetoothPort => "蓝牙".to_owned(),
                serialport::SerialPortType::PciPort => "PCI".to_owned(),
                serialport::SerialPortType::Unknown => "未知类型".to_owned(),
            };
            PortInfo {
                name: port.port_name,
                details,
            }
        })
        .collect()
}

/// What the reading thread reports.
#[derive(Debug)]
pub enum SerialEvent {
    Data {
        t: f64,
        bytes: Vec<u8>,
    },
    /// The port closed, with the reason if it was an error.
    Ended(Option<String>),
}

/// The way back into an open port.
#[derive(Debug)]
pub struct SerialLink {
    /// Bytes for the port to write.
    pub writes: UnboundedSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SerialLink {
    /// Asks the thread to finish; it closes the port on its way out.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // The thread wakes up from its read timeout within a few milliseconds.
            let _ = thread.join();
        }
    }
}

impl Drop for SerialLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A message for a failed open.
///
/// Windows reports both a port held by another program and a port that does not exist as
/// `NoDevice`, so the port list decides which one it was: a port that is still listed could not
/// be opened because something else holds it.
fn open_error_message(port: &str, error: &serialport::Error, listed: bool) -> String {
    match error.kind() {
        serialport::ErrorKind::NoDevice if listed => {
            format!("打开串口 {port} 失败：端口被其他程序占用，请关闭正在使用它的程序")
        }
        serialport::ErrorKind::NoDevice => format!(
            "打开串口 {port} 失败：系统里没有这个端口，设备可能已拔出或端口号变了，点刷新重新扫描"
        ),
        serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied) => format!(
            "打开串口 {port} 失败：没有权限访问这个端口，可能被其他程序占用，或当前用户不在串口用户组里（{error}）"
        ),
        _ => format!("打开串口 {port} 失败：{error}"),
    }
}

/// Opens the port and starts reading it. The first error after the port is open ends the thread.
pub fn open(
    config: &SerialConfig,
    events: UnboundedSender<SerialEvent>,
    clock: Clock,
) -> Result<SerialLink, String> {
    let mut port = serialport::new(&config.port, config.baud)
        .data_bits(match config.data_bits {
            5 => serialport::DataBits::Five,
            6 => serialport::DataBits::Six,
            7 => serialport::DataBits::Seven,
            _ => serialport::DataBits::Eight,
        })
        .parity(match config.parity {
            ParityChoice::None => serialport::Parity::None,
            ParityChoice::Odd => serialport::Parity::Odd,
            ParityChoice::Even => serialport::Parity::Even,
        })
        .stop_bits(if config.stop_bits == 2 {
            serialport::StopBits::Two
        } else {
            serialport::StopBits::One
        })
        .flow_control(match config.flow {
            FlowChoice::None => serialport::FlowControl::None,
            FlowChoice::Software => serialport::FlowControl::Software,
            FlowChoice::Hardware => serialport::FlowControl::Hardware,
        })
        // Short enough that writes are handled promptly, long enough not to spin.
        .timeout(Duration::from_millis(10))
        .open()
        .map_err(|error| {
            // Windows reports "in use" and "no such port" with the same error kind; a port that
            // is still in the list was therefore taken by another program.
            let listed = list_ports()
                .iter()
                .any(|info| info.name.eq_ignore_ascii_case(&config.port));
            open_error_message(&config.port, &error, listed)
        })?;

    let (writes, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let stop = Arc::new(AtomicBool::new(false));

    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name(format!("serial {}", config.port))
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            let mut ended = None;
            while !thread_stop.load(Ordering::Acquire) {
                while let Ok(bytes) = write_rx.try_recv() {
                    if let Err(error) = port.write_all(&bytes) {
                        ended = Some(format!("写串口失败：{error}"));
                        break;
                    }
                }
                if ended.is_some() {
                    break;
                }
                match port.read(&mut buffer) {
                    Ok(0) => {}
                    Ok(n) => {
                        let event = SerialEvent::Data {
                            t: clock.now(),
                            bytes: buffer[..n].to_vec(),
                        };
                        if events.send(event).is_err() {
                            // Nobody is listening any more: the connection was closed.
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                    Err(error) => {
                        ended = Some(format!("读串口失败：{error}"));
                        break;
                    }
                }
            }
            let _ = events.send(SerialEvent::Ended(ended));
        })
        .map_err(|error| format!("创建串口线程失败：{error}"))?;

    Ok(SerialLink {
        writes,
        stop,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_port_opens_at_115200_8n1() {
        let config = SerialConfig::default();
        assert_eq!(config.baud, 115_200);
        assert_eq!(config.data_bits, 8);
        assert_eq!(config.parity, ParityChoice::None);
        assert_eq!(config.stop_bits, 1);
        assert_eq!(config.flow, FlowChoice::None);
        assert_eq!(config.describe(), " @ 115200");
    }

    #[test]
    fn opening_a_missing_port_reports_why() {
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let config = SerialConfig {
            port: "COM-not-there".to_owned(),
            ..SerialConfig::default()
        };
        let error = open(&config, events, Clock::new()).unwrap_err();
        assert!(error.contains("COM-not-there"), "{error}");
    }

    #[test]
    fn a_listed_port_that_will_not_open_is_called_occupied() {
        let error = serialport::Error::new(serialport::ErrorKind::NoDevice, "拒绝访问。");
        let busy = open_error_message("COM5", &error, true);
        assert!(busy.contains("被其他程序占用"), "{busy}");
        assert!(busy.contains("COM5"), "{busy}");
        let missing = open_error_message("COM5", &error, false);
        assert!(missing.contains("没有这个端口"), "{missing}");
    }

    #[test]
    fn a_permission_error_suggests_the_serial_group() {
        let error = serialport::Error::new(
            serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied),
            "Permission denied",
        );
        let message = open_error_message("ttyUSB0", &error, true);
        assert!(message.contains("没有权限"), "{message}");
        assert!(message.contains("用户组"), "{message}");
    }
}
