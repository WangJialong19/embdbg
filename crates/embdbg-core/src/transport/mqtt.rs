//! MQTT client: subscribe to a topic filter, publish to a topic, payloads into the pipeline.
//!
//! Each publish payload keeps its boundaries, so a JSON message, a text line and a JustFloat
//! packet all arrive whole. The event loop reconnects on its own; a broken link is reported and
//! retried rather than ending the connection.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, watch};

use crate::clock::Clock;

/// Unique client ids for connections that did not bring their own.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// An MQTT 3.1.1 broker to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    /// Empty asks for one based on the process id.
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Topic filter to subscribe to, wildcards allowed.
    pub subscribe: String,
    /// Topic to publish sends to; no wildcards.
    pub publish: String,
    /// 0, 1 or 2.
    pub qos: u8,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 1883,
            client_id: String::new(),
            username: None,
            password: None,
            subscribe: "embdbg/#".to_owned(),
            publish: "embdbg/tx".to_owned(),
            qos: 0,
        }
    }
}

impl MqttConfig {
    pub fn describe(&self) -> String {
        format!("{}:{} · 订阅 {}", self.host, self.port, self.subscribe)
    }
}

/// What an MQTT task reports.
#[derive(Debug)]
pub enum MqttEvent {
    Data {
        t: f64,
        bytes: Vec<u8>,
    },
    /// Connecting, connected, subscribed, reconnecting.
    Info(String),
    /// The client stopped; errors are reported through Info while it retries.
    Ended(Option<String>),
}

pub struct MqttLink {
    pub writes: UnboundedSender<Vec<u8>>,
    stop: watch::Sender<bool>,
}

impl MqttLink {
    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for MqttLink {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the client and its event loop.
pub fn spawn(config: &MqttConfig, events: UnboundedSender<MqttEvent>, clock: Clock) -> MqttLink {
    let (writes, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop, mut stop_rx) = watch::channel(false);
    let config = config.clone();

    tokio::spawn(async move {
        let client_id = if config.client_id.trim().is_empty() {
            format!(
                "embdbg-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            )
        } else {
            config.client_id.clone()
        };
        let mut options = MqttOptions::new(client_id, &config.host, config.port);
        options.set_keep_alive(Duration::from_secs(20));
        if let Some(username) = config.username.as_deref().filter(|name| !name.is_empty()) {
            options.set_credentials(username, config.password.clone().unwrap_or_default());
        }
        let (client, mut eventloop) = AsyncClient::new(options, 64);
        let qos = qos_of(config.qos);

        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,
                outgoing = write_rx.recv() => match outgoing {
                    Some(bytes) => {
                        if let Err(error) = client.publish(&config.publish, qos, false, bytes).await {
                            let _ = events.send(MqttEvent::Info(format!(
                                "发布到 {} 失败：{error}",
                                config.publish
                            )));
                        }
                    }
                    None => break,
                },
                event = eventloop.poll() => match event {
                    Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                        let _ = events.send(MqttEvent::Info("已连接".to_owned()));
                        if let Err(error) = client.subscribe(&config.subscribe, qos).await {
                            let _ = events.send(MqttEvent::Info(format!(
                                "订阅 {} 失败：{error}",
                                config.subscribe
                            )));
                        }
                    }
                    Ok(Event::Incoming(Incoming::SubAck(_))) => {
                        let _ = events.send(MqttEvent::Info(format!(
                            "已订阅 {}",
                            config.subscribe
                        )));
                    }
                    Ok(Event::Incoming(Incoming::Publish(packet))) => {
                        let _ = events.send(MqttEvent::Data {
                            t: clock.now(),
                            bytes: packet.payload.to_vec(),
                        });
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = events.send(MqttEvent::Info(format!(
                            "MQTT 连接中断，正在重连：{error}"
                        )));
                        // The event loop reconnects on the next poll; do not spin while the
                        // broker is unreachable.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                },
            }
        }
        let _ = events.send(MqttEvent::Ended(None));
    });

    MqttLink { writes, stop }
}

fn qos_of(qos: u8) -> QoS {
    match qos {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_levels_map_to_the_wire_enum() {
        assert_eq!(qos_of(0), QoS::AtMostOnce);
        assert_eq!(qos_of(1), QoS::AtLeastOnce);
        assert_eq!(qos_of(2), QoS::ExactlyOnce);
        // Unknown levels fall back to the cheapest one.
        assert_eq!(qos_of(9), QoS::AtMostOnce);
    }

    #[test]
    fn the_default_subscription_uses_a_wildcard() {
        let config = MqttConfig::default();
        assert!(config.subscribe.ends_with("/#"));
        assert!(!config.publish.contains('#') && !config.publish.contains('+'));
    }
}
