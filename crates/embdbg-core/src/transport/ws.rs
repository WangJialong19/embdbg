//! WebSocket client: text and binary messages both become bytes for the pipeline.

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::clock::Clock;

/// A WebSocket to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsConfig {
    /// `ws://host:port/path` or `wss://…`.
    pub url: String,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8080/".to_owned(),
        }
    }
}

impl WsConfig {
    pub fn describe(&self) -> String {
        self.url.clone()
    }
}

/// What a WebSocket task reports.
#[derive(Debug)]
pub enum WsEvent {
    Data {
        t: f64,
        bytes: Vec<u8>,
    },
    /// Something worth logging: connecting, connected.
    Info(String),
    /// The socket closed, with the reason if it was an error.
    Ended(Option<String>),
}

pub struct WsLink {
    pub writes: UnboundedSender<Vec<u8>>,
    stop: watch::Sender<bool>,
}

impl WsLink {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for WsLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Connects in the background. A failed connection is reported as an event, not as an error
/// here, the same way TCP clients behave.
pub fn spawn_client(config: &WsConfig, events: UnboundedSender<WsEvent>, clock: Clock) -> WsLink {
    let (writes, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, mut stop_rx) = watch::channel(false);
    let url = config.url.clone();

    tokio::spawn(async move {
        let _ = events.send(WsEvent::Info(format!("正在连接 {url}")));
        let stream = tokio::select! {
            _ = stop_rx.changed() => return,
            result = tokio_tungstenite::connect_async(url.as_str()) => match result {
                Ok((stream, _response)) => stream,
                Err(error) => {
                    let _ = events.send(WsEvent::Ended(Some(format!("WebSocket 连接失败：{error}"))));
                    return;
                }
            },
        };
        let _ = events.send(WsEvent::Info("已连接".to_owned()));

        let (mut sink, mut incoming) = stream.split();
        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,
                outgoing = write_rx.recv() => match outgoing {
                    Some(bytes) => {
                        // Text frames are what most JSON-like protocols expect; anything that is
                        // not valid UTF-8 can only be binary.
                        let message = match String::from_utf8(bytes) {
                            Ok(text) => Message::Text(text.into()),
                            Err(error) => Message::Binary(error.into_bytes().into()),
                        };
                        if let Err(error) = sink.send(message).await {
                            let _ = events.send(WsEvent::Ended(Some(format!(
                                "WebSocket 发送失败：{error}"
                            ))));
                            return;
                        }
                    }
                    None => break,
                },
                received = incoming.next() => match received {
                    Some(Ok(Message::Text(text))) => {
                        let _ = events.send(WsEvent::Data {
                            t: clock.now(),
                            bytes: text.as_str().as_bytes().to_vec(),
                        });
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        let _ = events.send(WsEvent::Data {
                            t: clock.now(),
                            bytes: bytes.to_vec(),
                        });
                    }
                    Some(Ok(Message::Close(_))) => {
                        let _ = events.send(WsEvent::Info("对端关闭了 WebSocket".to_owned()));
                        break;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Err(error)) => {
                        let _ = events.send(WsEvent::Ended(Some(format!(
                            "WebSocket 接收失败：{error}"
                        ))));
                        return;
                    }
                    None => break,
                },
            }
        }
        let _ = events.send(WsEvent::Ended(None));
    });

    WsLink { writes, stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn text_messages_go_both_ways() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        let reply = format!("echo:{text}");
                        socket.send(Message::Text(reply.into())).await.unwrap();
                    }
                    Message::Binary(bytes) => {
                        socket.send(Message::Binary(bytes)).await.unwrap();
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });

        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let mut link = spawn_client(
            &WsConfig {
                url: format!("ws://{addr}/"),
            },
            events_tx,
            Clock::new(),
        );

        // Wait for the connection before writing: the client reports it as an info event.
        let mut connected = false;
        for _ in 0..50 {
            match timeout(Duration::from_millis(100), events_rx.recv()).await {
                Ok(Some(WsEvent::Info(text))) if text.contains("已连接") => {
                    connected = true;
                    break;
                }
                Ok(Some(_)) | Err(_) => continue,
                Ok(None) => break,
            }
        }
        assert!(connected, "the client should connect");

        link.writes.send(b"hello".to_vec()).unwrap();
        let mut reply = None;
        for _ in 0..50 {
            match timeout(Duration::from_millis(100), events_rx.recv()).await {
                Ok(Some(WsEvent::Data { bytes, .. })) => {
                    reply = Some(bytes);
                    break;
                }
                Ok(Some(_)) | Err(_) => continue,
                Ok(None) => break,
            }
        }
        assert_eq!(reply.as_deref(), Some(&b"echo:hello"[..]));
        link.stop();
    }
}
