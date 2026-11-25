//! Types shared by every client of the engine (GUI, MCP server, CLI).

use std::fmt;

/// Something the engine reports to its clients. Sent on a broadcast channel, so a
/// client that falls behind may miss events; it must not rely on them for state.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The built-in signal simulator started or stopped.
    Simulator {
        running: bool,
    },
    Info(String),
    Warn(String),
    Error(String),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Simulator { running } => {
                write!(f, "模拟信号源已{}", if *running { "启动" } else { "停止" })
            }
            Event::Info(msg) => write!(f, "{msg}"),
            Event::Warn(msg) => write!(f, "警告：{msg}"),
            Event::Error(msg) => write!(f, "错误：{msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_display_in_chinese() {
        assert_eq!(
            Event::Simulator { running: true }.to_string(),
            "模拟信号源已启动"
        );
        assert_eq!(
            Event::Error("串口被占用".into()).to_string(),
            "错误：串口被占用"
        );
    }
}
