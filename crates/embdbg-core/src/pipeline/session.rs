//! One connection's data path: bytes in, frames decoded, samples stored, text logged.
//!
//! Every transport (serial, TCP, UDP, WebSocket, MQTT payloads) feeds this same session, so a
//! firmware speaking JustFloat over TCP looks exactly like one speaking it over a serial port.

use std::sync::atomic::Ordering;

use parking_lot::RwLock;

use super::decoder::{Decoded, Decoder, DecoderSpec};
use super::framing::{Framer, Framing};
use super::set::ChannelSet;
use super::terminal::{Terminal, TextEncoding};
use super::traffic::{ByteLog, Direction, LineLog, TrafficStats};

/// Per-connection settings that decide how bytes become data.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub decoder: DecoderSpec,
    /// A transport that keeps message boundaries (UDP) uses [`Framing::Datagram`]; streams
    /// leave this `None` and let the decoder choose.
    pub framing: Option<Framing>,
    /// JSON parameters for a plugin decoder; empty for the built-in protocols.
    pub options: String,
}

impl SessionConfig {
    pub fn stream(decoder: DecoderSpec) -> Self {
        Self {
            decoder,
            framing: None,
            options: String::new(),
        }
    }

    pub fn datagram(decoder: DecoderSpec) -> Self {
        Self {
            decoder,
            framing: Some(Framing::Datagram),
            options: String::new(),
        }
    }

    /// Adds the parameters a plugin connection was configured with.
    pub fn with_options(mut self, options: impl Into<String>) -> Self {
        self.options = options.into();
        self
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::stream(DecoderSpec::Line)
    }
}

/// Everything a session writes into: shared with the UI.
#[derive(Debug)]
pub struct SessionShared {
    pub channels: RwLock<ChannelSet>,
    pub lines: RwLock<LineLog>,
    pub bytes: RwLock<ByteLog>,
    /// The terminal screen: every received byte, with escape sequences applied.
    pub terminal: RwLock<Terminal>,
    /// The last plugin failure, for the connection panel. `None` while the plugin behaves.
    pub plugin_error: RwLock<Option<String>>,
    pub stats: TrafficStats,
}

impl Default for SessionShared {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionShared {
    pub fn new() -> Self {
        Self::with_capacity(super::DEFAULT_CAPACITY)
    }

    /// A session whose channels retain `capacity` samples each.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            channels: RwLock::new(ChannelSet::new(capacity)),
            lines: RwLock::new(LineLog::with_default_capacity()),
            bytes: RwLock::new(ByteLog::with_default_capacity()),
            terminal: RwLock::new(Terminal::new(40, 100)),
            plugin_error: RwLock::new(None),
            stats: TrafficStats::default(),
        }
    }

    /// Forgets samples, lines, raw bytes and counters.
    pub fn clear(&self) {
        self.channels.write().clear();
        self.lines.write().clear();
        self.bytes.write().clear();
        self.terminal.write().clear();
        *self.plugin_error.write() = None;
        self.stats.clear();
    }

    /// Switches how received bytes are shown as text. What the byte log still holds is replayed,
    /// so the screen shows its history in the new encoding instead of going blank.
    pub fn set_encoding(&self, encoding: TextEncoding) {
        let mut terminal = self.terminal.write();
        if terminal.encoding() == encoding {
            return;
        }
        terminal.set_encoding(encoding);
        let log = self.bytes.read();
        for index in 0..log.len() {
            let Some(chunk) = log.get(index) else { break };
            if chunk.direction == Direction::RemoteToHere {
                terminal.feed(&chunk.bytes);
            }
        }
    }

    pub fn encoding(&self) -> TextEncoding {
        self.terminal.read().encoding()
    }
}

/// The byte path of one connection.
#[derive(Debug)]
pub struct DataSession {
    framer: Framer,
    decoder: Decoder,
    /// The transport's framing choice, kept across protocol changes.
    framing: Option<Framing>,
    /// The plugin error last copied into the shared state, so a quiet connection does not take
    /// the write lock every chunk.
    last_plugin_error: Option<String>,
}

impl DataSession {
    pub fn new(config: SessionConfig) -> Self {
        let framing = config
            .framing
            .clone()
            .unwrap_or_else(|| config.decoder.framing());
        Self {
            framer: Framer::new(framing),
            decoder: Decoder::with_options(config.decoder, config.options),
            framing: config.framing,
            last_plugin_error: None,
        }
    }

    /// The protocol this session decodes with.
    pub fn spec(&self) -> DecoderSpec {
        self.decoder.spec()
    }

    /// The JSON parameters of a plugin decoder.
    pub fn options(&self) -> &str {
        self.decoder.options()
    }

    /// Bytes of an incomplete frame waiting for more input.
    pub fn pending(&self) -> usize {
        self.framer.pending()
    }

    /// Switches protocol or plugin parameters; a partial frame from the old setup is flushed
    /// first.
    pub fn reconfigure(&mut self, config: SessionConfig, shared: &SessionShared) {
        let decoder_changed = config.decoder != self.decoder.spec();
        let options_changed = config.options != self.decoder.options();
        if !decoder_changed && !options_changed {
            return;
        }
        self.on_idle(f64::NAN, shared);
        if decoder_changed {
            self.framer = Framer::new(
                self.framing
                    .clone()
                    .unwrap_or_else(|| config.decoder.framing()),
            );
        }
        self.decoder.set_config(config.decoder, config.options);
        self.sync_plugin_error(shared);
    }

    /// Copies a plugin failure into the shared state, where the connection panel shows it.
    fn sync_plugin_error(&mut self, shared: &SessionShared) {
        let current = self.decoder.plugin_error().map(str::to_owned);
        if current != self.last_plugin_error {
            self.last_plugin_error = current.clone();
            *shared.plugin_error.write() = current;
        }
    }

    /// Feeds bytes that arrived at time `t`.
    pub fn on_bytes(&mut self, t: f64, bytes: &[u8], shared: &SessionShared) {
        if bytes.is_empty() {
            return;
        }
        shared
            .stats
            .rx_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        shared.bytes.write().push(t, Direction::RemoteToHere, bytes);
        // The screen sees every byte, escape sequences included.
        shared.terminal.write().feed(bytes);

        let framed = {
            let decoder = &mut self.decoder;
            let channels = &shared.channels;
            let lines = &shared.lines;
            let stats = &shared.stats;
            self.framer.push(bytes, |frame| {
                stats.frames.fetch_add(1, Ordering::Relaxed);
                match decoder.decode(frame) {
                    Decoded::Samples(samples) => {
                        // One frame is one arrival: all its samples share one timestamp.
                        let mut set = channels.write();
                        set.push_frames(t, samples.len(), samples);
                        stats
                            .samples
                            .fetch_add(samples.len() as u64, Ordering::Relaxed);
                    }
                    Decoded::Line(line) => {
                        lines.write().push(
                            t,
                            Direction::RemoteToHere,
                            String::from_utf8_lossy(line),
                        );
                    }
                    Decoded::Ignored => {}
                }
            })
        };
        if framed.dropped_bytes > 0 {
            shared
                .stats
                .dropped_bytes
                .fetch_add(framed.dropped_bytes as u64, Ordering::Relaxed);
            tracing::warn!(
                target: "embdbg::session",
                "丢弃了 {} 字节：一帧超过了长度上限",
                framed.dropped_bytes
            );
        }
        self.sync_plugin_error(shared);
    }

    /// A line without a line break, or the rest of a frame, waits for this.
    pub fn on_idle(&mut self, t: f64, shared: &SessionShared) {
        let decoder = &mut self.decoder;
        let lines = &shared.lines;
        self.framer.flush(|frame| {
            if let Decoded::Line(line) = decoder.decode(frame) {
                lines
                    .write()
                    .push(t, Direction::RemoteToHere, String::from_utf8_lossy(line));
            }
        });
        self.sync_plugin_error(shared);
    }

    /// Records bytes this side sent, so the terminal and hex views show both directions.
    ///
    /// Sending happens on the UI thread, which has no session at hand, hence the freestanding form.
    pub fn log_sent(t: f64, bytes: &[u8], shared: &SessionShared) {
        if bytes.is_empty() {
            return;
        }
        shared
            .stats
            .tx_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        shared.bytes.write().push(t, Direction::HereToRemote, bytes);
        shared.lines.write().push(
            t,
            Direction::HereToRemote,
            String::from_utf8_lossy(bytes).trim_end(),
        );
    }

    /// Samples per channel per second, from the timestamps that came with the data.
    pub fn sample_rate(&self, shared: &SessionShared) -> Option<f64> {
        let channels = shared.channels.read();
        channels.axis().frame_rate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::decoder::JUST_FLOAT_TAIL;

    fn just_float(values: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&JUST_FLOAT_TAIL);
        bytes
    }

    #[test]
    fn a_just_float_stream_split_anywhere_lands_in_the_channels() {
        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::JustFloat));

        // Three frames, fed one byte at a time: chunk boundaries must not matter.
        let mut stream = Vec::new();
        stream.extend(just_float(&[1.0, 2.0]));
        stream.extend(just_float(&[3.0, 4.0]));
        stream.extend(just_float(&[5.0, 6.0]));
        for chunk in stream.chunks(3) {
            session.on_bytes(0.1, chunk, &shared);
        }

        let channels = shared.channels.read();
        assert_eq!(channels.channels().len(), 2);
        assert_eq!(channels.frames(), 3);
        assert_eq!(channels.channel(0).unwrap().value_at(2), Some(5.0));
        assert_eq!(channels.channel(1).unwrap().value_at(0), Some(2.0));
        assert_eq!(shared.stats.summary().2, 3, "three frames decoded");
        assert_eq!(shared.stats.summary().3, 6, "six samples stored");
        assert_eq!(shared.bytes.read().bytes(), stream.len());
        assert_eq!(shared.lines.read().len(), 0);
    }

    #[test]
    fn a_line_stream_reaches_the_terminal_and_a_partial_line_waits_for_idle() {
        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::Line));

        session.on_bytes(1.0, b"AT\r\nOK\r", &shared);
        session.on_bytes(1.1, b"\n+CMS ERROR: 3\r\nTAIL", &shared);
        {
            let lines = shared.lines.read();
            let texts: Vec<&str> = lines.lines().map(|line| line.text.as_str()).collect();
            assert_eq!(texts, ["AT", "OK", "+CMS ERROR: 3"]);
        }

        // The unfinished last line is shown once the stream goes quiet.
        session.on_idle(2.0, &shared);
        let lines = shared.lines.read();
        let texts: Vec<&str> = lines.lines().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["AT", "OK", "+CMS ERROR: 3", "TAIL"]);
    }

    #[test]
    fn switching_the_encoding_replays_what_the_byte_log_holds() {
        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::Line));
        // "中文" in GBK, with the character split across two reads.
        session.on_bytes(1.0, &[0xd6], &shared);
        session.on_bytes(1.1, &[0xd0, 0xce, 0xc4], &shared);
        assert!(!shared.terminal.read().contents().contains('中'));

        shared.set_encoding(TextEncoding::Gbk);
        assert!(shared.terminal.read().contents().contains("中文"));
    }

    #[test]
    fn what_we_send_is_logged_in_both_views() {
        let shared = SessionShared::new();
        DataSession::log_sent(3.0, b"AT+CSQ\r\n", &shared);

        let lines = shared.lines.read();
        let line = lines.lines().next().unwrap();
        assert_eq!(line.direction, Direction::HereToRemote);
        assert_eq!(line.text, "AT+CSQ");
        assert_eq!(shared.bytes.read().bytes(), 8);
        assert_eq!(shared.stats.summary().1, 8, "tx bytes counted");
    }

    #[test]
    fn garbage_frames_are_counted_but_harmless() {
        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::JustFloat));
        // A stream of random bytes without a tail stays pending, then is dropped past the limit.
        session.on_bytes(0.0, &[0x42; 100], &shared);
        assert_eq!(shared.channels.read().frames(), 0);
        assert_eq!(shared.stats.summary().2, 0);
    }

    #[test]
    fn sample_rate_comes_from_the_timestamps() {
        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::JustFloat));
        let frame = just_float(&[1.0]);
        // Ten frames, one every 10 ms: 100 samples per second.
        for i in 0..10 {
            session.on_bytes(i as f64 * 0.01, &frame, &shared);
        }
        let rate = session.sample_rate(&shared).unwrap();
        assert!((rate - 100.0).abs() < 1e-6, "rate {rate}");
    }
}
