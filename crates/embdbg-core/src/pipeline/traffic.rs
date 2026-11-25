//! What a connection shows besides samples: text lines, raw bytes, and the counters for both.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    RemoteToHere,
    HereToRemote,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::RemoteToHere => "收",
            Self::HereToRemote => "发",
        }
    }
}

/// One line of text on a connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Seconds on the engine's clock.
    pub t: f64,
    pub direction: Direction,
    pub text: String,
}

/// A bounded log of text lines: the terminal view.
#[derive(Debug)]
pub struct LineLog {
    lines: VecDeque<Line>,
    capacity: usize,
}

impl LineLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(4096)),
            capacity: capacity.max(1),
        }
    }

    /// Default capacity: enough scrollback for a session, about a megabyte of text.
    pub fn with_default_capacity() -> Self {
        Self::new(20_000)
    }

    pub fn push(&mut self, t: f64, direction: Direction, text: impl Into<String>) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            t,
            direction,
            text: text.into(),
        });
    }

    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    /// The line at `index`, counting from the oldest retained one.
    pub fn get(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// A chunk of raw bytes: one read from the wire, or one send.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub t: f64,
    pub direction: Direction,
    pub bytes: Vec<u8>,
}

/// The hex view's data: raw chunks, bounded by their total size rather than their count.
#[derive(Debug)]
pub struct ByteLog {
    chunks: VecDeque<Chunk>,
    bytes: usize,
    max_bytes: usize,
}

impl ByteLog {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            max_bytes: max_bytes.max(8),
        }
    }

    /// Default capacity: a few megabytes, enough to scroll back through a burst.
    pub fn with_default_capacity() -> Self {
        Self::new(4 << 20)
    }

    pub fn push(&mut self, t: f64, direction: Direction, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.bytes += bytes.len();
        self.chunks.push_back(Chunk {
            t,
            direction,
            bytes: bytes.to_vec(),
        });
        // Drop whole chunks from the front until the log fits again. The newest chunk stays even
        // when it alone is bigger than the budget: showing it beats showing nothing.
        while self.bytes > self.max_bytes && self.chunks.len() > 1 {
            match self.chunks.pop_front() {
                Some(chunk) => self.bytes -= chunk.bytes.len(),
                None => break,
            }
        }
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.iter()
    }

    /// How many chunks are retained.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The chunk at `index`, counting from the oldest retained one.
    pub fn get(&self, index: usize) -> Option<&Chunk> {
        self.chunks.get(index)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
    }
}

/// Counters shared with the UI without a lock.
#[derive(Debug, Default)]
pub struct TrafficStats {
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
    /// Frames handed to a decoder.
    pub frames: AtomicU64,
    /// Samples stored in the channel set.
    pub samples: AtomicU64,
    /// Bytes discarded because a frame overran its maximum length.
    pub dropped_bytes: AtomicU64,
}

impl TrafficStats {
    pub fn get(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    pub fn summary(&self) -> (u64, u64, u64, u64, u64) {
        (
            self.rx_bytes.load(Ordering::Relaxed),
            self.tx_bytes.load(Ordering::Relaxed),
            self.frames.load(Ordering::Relaxed),
            self.samples.load(Ordering::Relaxed),
            self.dropped_bytes.load(Ordering::Relaxed),
        )
    }

    pub fn clear(&self) {
        for counter in [
            &self.rx_bytes,
            &self.tx_bytes,
            &self.frames,
            &self.samples,
            &self.dropped_bytes,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_log_drops_the_oldest_lines() {
        let mut log = LineLog::new(3);
        for i in 0..5 {
            log.push(i as f64, Direction::RemoteToHere, format!("line {i}"));
        }
        let texts: Vec<&str> = log.lines().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["line 2", "line 3", "line 4"]);
        assert_eq!(log.len(), 3);
        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn the_byte_log_is_bounded_by_total_size() {
        let mut log = ByteLog::new(10);
        log.push(0.0, Direction::RemoteToHere, &[1, 2, 3, 4]);
        log.push(1.0, Direction::HereToRemote, &[5, 6, 7, 8]);
        assert_eq!(log.bytes(), 8);
        log.push(2.0, Direction::RemoteToHere, &[9, 10, 11, 12]);
        // The oldest chunk goes as a whole: 4 + 4 + 4 exceeds 10 once the third arrives.
        assert_eq!(log.bytes(), 8);
        let chunks: Vec<&[u8]> = log.chunks().map(|chunk| chunk.bytes.as_slice()).collect();
        assert_eq!(chunks, [&[5, 6, 7, 8][..], &[9, 10, 11, 12][..]]);

        // A chunk larger than the whole budget is still kept, alone.
        log.push(3.0, Direction::RemoteToHere, &[0; 32]);
        assert_eq!(log.bytes(), 32);
        assert_eq!(log.chunks().count(), 1);
    }

    #[test]
    fn empty_chunks_are_not_stored() {
        let mut log = ByteLog::new(16);
        log.push(0.0, Direction::RemoteToHere, &[]);
        assert_eq!(log.chunks().count(), 0);
    }

    #[test]
    fn counters_add_up_and_reset() {
        let stats = TrafficStats::default();
        stats.rx_bytes.fetch_add(10, Ordering::Relaxed);
        stats.tx_bytes.fetch_add(3, Ordering::Relaxed);
        stats.frames.fetch_add(2, Ordering::Relaxed);
        assert_eq!(stats.summary(), (10, 3, 2, 0, 0));
        assert_eq!(stats.get(&stats.frames), 2);
        stats.clear();
        assert_eq!(stats.summary(), (0, 0, 0, 0, 0));
    }
}
