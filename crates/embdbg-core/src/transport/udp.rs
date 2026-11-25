//! UDP: one socket, datagrams in, datagrams out.
//!
//! A datagram keeps its boundaries, so every read is one frame: text lines, JustFloat packets and
//! whatever else a device sends over UDP arrive intact. Without a target, replies go back to the
//! last peer that wrote to us, which is how most UDP debug tools behave.

use std::net::{SocketAddr, ToSocketAddrs};

use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, watch};

use crate::clock::Clock;

/// A UDP socket to listen and talk on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpConfig {
    /// Local address to bind; `0.0.0.0` listens on every interface.
    pub bind: String,
    /// Local port; 0 asks the system for one.
    pub port: u16,
    /// Where sends go, as `host:port`. Empty sends to the last peer instead.
    pub target: Option<String>,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_owned(),
            port: 9000,
            target: None,
        }
    }
}

impl UdpConfig {
    /// What the connection list shows.
    pub fn describe(&self) -> String {
        match self
            .target
            .as_deref()
            .filter(|target| !target.trim().is_empty())
        {
            Some(target) => format!("{}:{} → {target}", self.bind, self.port),
            None => format!("监听 {}:{}", self.bind, self.port),
        }
    }
}

/// What a UDP task reports.
#[derive(Debug)]
pub enum UdpEvent {
    Data {
        t: f64,
        bytes: Vec<u8>,
    },
    /// Something worth logging: ready, a send with no target.
    Info(String),
    /// The socket closed, with the reason if it was an error.
    Ended(Option<String>),
}

pub struct UdpLink {
    pub writes: UnboundedSender<Vec<u8>>,
    /// The address actually bound, which differs from the request when port 0 was asked for.
    pub local_addr: SocketAddr,
    stop: watch::Sender<bool>,
}

impl UdpLink {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for UdpLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds the socket and starts the receive loop. Binding happens here, so a bad port or address
/// is reported to the caller instead of appearing later as an event.
pub fn spawn(
    config: &UdpConfig,
    events: UnboundedSender<UdpEvent>,
    clock: Clock,
) -> Result<UdpLink, String> {
    let bind = format!("{}:{}", config.bind, config.port);
    let socket = std::net::UdpSocket::bind(&bind)
        .map_err(|error| format!("绑定 UDP {bind} 失败：{error}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| format!("UDP 设置非阻塞失败：{error}"))?;
    let local_addr = socket
        .local_addr()
        .map_err(|error| format!("读取 UDP 地址失败：{error}"))?;
    let target = match config.target.as_deref().map(str::trim) {
        Some(target) if !target.is_empty() => target
            .to_socket_addrs()
            .map_err(|error| format!("解析 UDP 目标 {target} 失败：{error}"))?
            .next()
            .ok_or_else(|| format!("UDP 目标 {target} 没有地址"))?,
        _ => return bind_and_run(socket, local_addr, None, events, clock),
    };
    bind_and_run(socket, local_addr, Some(target), events, clock)
}

/// Registers the socket with the runtime and starts the loop. The caller holds a runtime context.
fn bind_and_run(
    socket: std::net::UdpSocket,
    local_addr: SocketAddr,
    target: Option<SocketAddr>,
    events: UnboundedSender<UdpEvent>,
    clock: Clock,
) -> Result<UdpLink, String> {
    let socket = UdpSocket::from_std(socket).map_err(|error| format!("UDP 注册失败：{error}"))?;
    let (writes, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, mut stop_rx) = watch::channel(false);

    tokio::spawn(async move {
        let _ = events.send(UdpEvent::Info(format!("UDP {local_addr} 已就绪")));
        let mut buffer = vec![0u8; 65_536];
        let mut last_peer: Option<SocketAddr> = None;
        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,
                outgoing = write_rx.recv() => match outgoing {
                    Some(bytes) => {
                        let Some(destination) = target.or(last_peer) else {
                            let _ = events.send(UdpEvent::Info(
                                "UDP 还没有目标地址：先收到一包，或填写目标地址".to_owned(),
                            ));
                            continue;
                        };
                        if let Err(error) = socket.send_to(&bytes, destination).await {
                            let _ = events.send(UdpEvent::Info(format!(
                                "发送到 {destination} 失败：{error}"
                            )));
                        }
                    }
                    None => break,
                },
                received = socket.recv_from(&mut buffer) => match received {
                    Ok((len, peer)) => {
                        last_peer = Some(peer);
                        let _ = events.send(UdpEvent::Data {
                            t: clock.now(),
                            bytes: buffer[..len].to_vec(),
                        });
                    }
                    Err(error) => {
                        let _ = events.send(UdpEvent::Ended(Some(format!("UDP 接收失败：{error}"))));
                        return;
                    }
                },
            }
        }
        let _ = events.send(UdpEvent::Ended(None));
    });

    Ok(UdpLink {
        writes,
        local_addr,
        stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn datagrams_go_both_ways_and_keep_their_edges() {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.local_addr().unwrap();
        let mut link = spawn(
            &UdpConfig {
                bind: "127.0.0.1".to_owned(),
                port: 0,
                target: Some(client_addr.to_string()),
            },
            events_tx,
            Clock::new(),
        )
        .unwrap();

        client
            .send_to(b"first datagram", link.local_addr)
            .await
            .unwrap();
        let event = timeout(Duration::from_secs(2), events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            UdpEvent::Info(text) => assert!(text.contains("已就绪"), "{text}"),
            other => panic!("expected the ready event first, got {other:?}"),
        }
        // The ready event may arrive before or after the first datagram; drain to the data.
        let mut received = None;
        for _ in 0..4 {
            match timeout(Duration::from_secs(2), events_rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                UdpEvent::Data { bytes, .. } => {
                    received = Some(bytes);
                    break;
                }
                _ => continue,
            }
        }
        assert_eq!(received.as_deref(), Some(&b"first datagram"[..]));

        link.writes.send(b"reply".to_vec()).unwrap();
        let mut buffer = [0u8; 64];
        let (len, _) = timeout(Duration::from_secs(2), client.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..len], b"reply");
        link.stop();
    }
}
