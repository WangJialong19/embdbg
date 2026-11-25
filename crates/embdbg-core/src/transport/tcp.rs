//! TCP: one client connection, or a server that accepts many.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{broadcast, mpsc, watch};

use crate::clock::Clock;

/// Connecting to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpClientConfig {
    pub host: String,
    pub port: u16,
    /// 0 waits as long as the system allows.
    pub connect_timeout_ms: u64,
}

impl Default for TcpClientConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 8080,
            connect_timeout_ms: 3000,
        }
    }
}

/// Listening for peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpServerConfig {
    pub bind: String,
    pub port: u16,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_owned(),
            port: 8080,
        }
    }
}

/// What a TCP task reports.
#[derive(Debug)]
pub enum TcpEvent {
    Data {
        t: f64,
        bytes: Vec<u8>,
    },
    /// A server's client count changed.
    Clients(usize),
    /// Something worth logging: listening, a client arriving or leaving.
    Info(String),
    /// The connection or listener closed, with the reason if it was an error.
    Ended(Option<String>),
}

/// The way back into an open TCP connection.
#[derive(Debug)]
pub struct TcpLink {
    pub writes: UnboundedSender<Vec<u8>>,
    stop: watch::Sender<bool>,
}

impl TcpLink {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for TcpLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Connects in the background; a failure to connect arrives as [`TcpEvent::Ended`].
pub fn spawn_client(
    config: &TcpClientConfig,
    events: UnboundedSender<TcpEvent>,
    clock: Clock,
) -> TcpLink {
    let (writes, write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, stop_rx) = watch::channel(false);
    tokio::spawn(run_client(config.clone(), events, clock, write_rx, stop_rx));
    TcpLink { writes, stop }
}

async fn run_client(
    config: TcpClientConfig,
    events: UnboundedSender<TcpEvent>,
    clock: Clock,
    mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let target = format!("{}:{}", config.host, config.port);
    let connect = TcpStream::connect(&target);
    let connected = if config.connect_timeout_ms == 0 {
        connect
            .await
            .map_err(|error| format!("连接 {target} 失败：{error}"))
    } else {
        match tokio::time::timeout(Duration::from_millis(config.connect_timeout_ms), connect).await
        {
            Ok(result) => result.map_err(|error| format!("连接 {target} 失败：{error}")),
            Err(_) => Err(format!("连接 {target} 超时")),
        }
    };
    let mut stream = match connected {
        Ok(stream) => stream,
        Err(error) => {
            let _ = events.send(TcpEvent::Ended(Some(error)));
            return;
        }
    };
    let _ = events.send(TcpEvent::Info(format!("已连接 {target}")));

    let mut buffer = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            _ = stop_rx.changed() => break,
            outgoing = write_rx.recv() => {
                let Some(bytes) = outgoing else { break };
                if let Err(error) = stream.write_all(&bytes).await {
                    let _ = events.send(TcpEvent::Ended(Some(format!("发送失败：{error}"))));
                    return;
                }
            }
            read = stream.read(&mut buffer) => match read {
                Ok(0) => break,
                Ok(n) => {
                    let event = TcpEvent::Data { t: clock.now(), bytes: buffer[..n].to_vec() };
                    if events.send(event).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = events.send(TcpEvent::Ended(Some(format!("读取失败：{error}"))));
                    return;
                }
            },
        }
    }
    let _ = events.send(TcpEvent::Ended(None));
}

/// Handles one connected client. It gets its own sink, so its partial frames cannot mix with
/// another client's.
pub type ClientSink = Box<dyn FnMut(&[u8]) + Send>;
pub type ClientSinkFactory = Box<dyn Fn(u64) -> ClientSink + Send>;

/// Binds in the background and accepts clients. Everything sent through the link goes to all
/// clients currently connected; everything received reaches the sink made for that client.
pub fn spawn_server(
    config: &TcpServerConfig,
    events: UnboundedSender<TcpEvent>,
    clock: Clock,
    make_sink: ClientSinkFactory,
) -> TcpLink {
    let (writes, write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, stop_rx) = watch::channel(false);
    tokio::spawn(run_server(
        config.clone(),
        events,
        clock,
        write_rx,
        stop_rx,
        make_sink,
    ));
    TcpLink { writes, stop }
}

async fn run_server(
    config: TcpServerConfig,
    events: UnboundedSender<TcpEvent>,
    _clock: Clock,
    mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop_rx: watch::Receiver<bool>,
    make_sink: ClientSinkFactory,
) {
    let bind = format!("{}:{}", config.bind, config.port);
    let listener = match TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = events.send(TcpEvent::Ended(Some(format!("监听 {bind} 失败：{error}"))));
            return;
        }
    };
    // Port 0 asks the system for a free port; report the one it chose.
    let local = listener
        .local_addr()
        .map_or_else(|_| bind.clone(), |addr| addr.to_string());
    let _ = events.send(TcpEvent::Info(format!("正在监听 {local}")));

    // What the server sends goes to every client currently connected.
    let (outgoing, _) = broadcast::channel::<Arc<Vec<u8>>>(64);
    let mut next_client = 0u64;
    let mut clients = 0usize;

    loop {
        tokio::select! {
            _ = stop_rx.changed() => break,
            to_send = write_rx.recv() => {
                let Some(bytes) = to_send else { break };
                // No clients is not an error: the data simply goes nowhere.
                let _ = outgoing.send(Arc::new(bytes));
            }
            accepted = listener.accept() => match accepted {
                Ok((mut stream, peer)) => {
                    next_client += 1;
                    clients += 1;
                    let _ = events.send(TcpEvent::Clients(clients));
                    let _ = events.send(TcpEvent::Info(format!("{peer} 已连接（共 {clients} 个）")));

                    let mut sink = make_sink(next_client);
                    let mut outgoing = outgoing.subscribe();
                    let events = events.clone();
                    let mut stop_rx = stop_rx.clone();
                    tokio::spawn(async move {
                        let mut buffer = [0u8; 16 * 1024];
                        loop {
                            tokio::select! {
                                _ = stop_rx.changed() => break,
                                broadcast = outgoing.recv() => match broadcast {
                                    Ok(bytes) => {
                                        if let Err(error) = stream.write_all(&bytes).await {
                                            let _ = events.send(TcpEvent::Info(format!("发送给 {peer} 失败：{error}")));
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                                },
                                read = stream.read(&mut buffer) => match read {
                                    Ok(0) => break,
                                    Ok(n) => sink(&buffer[..n]),
                                    Err(_) => break,
                                },
                            }
                        }
                        let _ = events.send(TcpEvent::Info(format!("{peer} 已断开")));
                    });
                }
                Err(error) => {
                    let _ = events.send(TcpEvent::Ended(Some(format!("接受连接失败：{error}"))));
                    return;
                }
            },
        }
    }
    let _ = events.send(TcpEvent::Ended(None));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads events until one satisfies `wanted`, for at most two seconds.
    async fn wait_for<T>(
        receiver: &mut mpsc::UnboundedReceiver<TcpEvent>,
        mut wanted: impl FnMut(&TcpEvent) -> Option<T>,
    ) -> Option<T> {
        for _ in 0..200 {
            while let Ok(event) = receiver.try_recv() {
                if let Some(value) = wanted(&event) {
                    return Some(value);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    #[tokio::test]
    async fn a_client_and_a_server_exchange_bytes_both_ways() {
        let clock = Clock::new();
        let (server_events, mut server_rx) = mpsc::unbounded_channel();
        // The server hands each client's bytes to its own sink; the test watches that.
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_target = Arc::clone(&received);
        let mut server = spawn_server(
            &TcpServerConfig {
                bind: "127.0.0.1".to_owned(),
                port: 0,
            },
            server_events,
            clock,
            Box::new(move |_id| {
                let sink_target = Arc::clone(&sink_target);
                Box::new(move |bytes: &[u8]| {
                    sink_target.lock().unwrap().extend_from_slice(bytes);
                })
            }),
        );

        // The listener announces the port the system picked.
        let port = wait_for(&mut server_rx, |event| match event {
            TcpEvent::Info(text) if text.starts_with("正在监听") => text
                .split(':')
                .next_back()
                .and_then(|port| port.parse::<u16>().ok()),
            _ => None,
        })
        .await
        .expect("the server never reported its port");

        let (client_events, mut client_rx) = mpsc::unbounded_channel();
        let mut client = spawn_client(
            &TcpClientConfig {
                host: "127.0.0.1".to_owned(),
                port,
                connect_timeout_ms: 2000,
            },
            client_events,
            clock,
        );

        client.writes.send(b"hello".to_vec()).unwrap();
        let mut heard = Vec::new();
        for _ in 0..200 {
            {
                let guard = received.lock().unwrap();
                if !guard.is_empty() {
                    heard = guard.clone();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(heard, b"hello");

        server.writes.send(b"world".to_vec()).unwrap();
        let answered = wait_for(&mut client_rx, |event| match event {
            TcpEvent::Data { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .await;
        assert_eq!(answered.as_deref(), Some(&b"world"[..]));

        client.stop();
        server.stop();
    }

    #[tokio::test]
    async fn a_client_that_cannot_connect_reports_why() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let _client = spawn_client(
            &TcpClientConfig {
                host: "127.0.0.1".to_owned(),
                port: 1,
                connect_timeout_ms: 500,
            },
            events,
            Clock::new(),
        );
        let error = wait_for(&mut rx, |event| match event {
            TcpEvent::Ended(reason) => Some(reason.clone()),
            _ => None,
        })
        .await;
        assert!(error.is_some(), "no end event");
        assert!(error.unwrap().is_some(), "no reason given");
    }
}
