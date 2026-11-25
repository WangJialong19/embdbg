//! Cutting a byte stream into frames.
//!
//! Serial ports and sockets deliver arbitrary chunks; a frame is what the firmware sent in one
//! piece: everything up to a tail byte sequence, up to a line break, or up to the next read
//! (a datagram, which keeps its boundaries).

/// How a byte stream is cut into frames.
#[derive(Debug, Clone, PartialEq)]
pub enum Framing {
    /// Frames end with a fixed byte sequence, for example the `00 00 80 7F` of JustFloat.
    /// The sequence stays part of the frame, so the decoder can check it.
    Tail { tail: Vec<u8>, max_len: usize },
    /// Frames end at a line break, which is not part of the frame.
    Lines { max_len: usize },
    /// Every read is one frame; datagrams keep their boundaries and need no framing.
    Datagram,
}

impl Framing {
    /// The tail the JustFloat and JustInt protocols share: little-endian `+inf`.
    pub fn just_float_tail() -> Framing {
        Framing::Tail {
            tail: vec![0x00, 0x00, 0x80, 0x7F],
            max_len: 64 * 1024,
        }
    }
}

impl Default for Framing {
    fn default() -> Self {
        Framing::Lines { max_len: 8 * 1024 }
    }
}

/// What a [`Framer::push`] run produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Framed {
    /// Complete frames handed to the callback.
    pub frames: usize,
    /// Bytes thrown away because a frame ran past its maximum length.
    pub dropped_bytes: usize,
}

/// Splits a byte stream into frames, keeping the incomplete rest for the next call.
#[derive(Debug)]
pub struct Framer {
    framing: Framing,
    buffer: Vec<u8>,
}

impl Framer {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buffer: Vec::new(),
        }
    }

    /// Bytes of an incomplete frame waiting for more input.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Feeds received bytes and calls `on_frame` for every complete frame, in order.
    pub fn push(&mut self, bytes: &[u8], mut on_frame: impl FnMut(&[u8])) -> Framed {
        let mut framed = Framed::default();
        self.buffer.extend_from_slice(bytes);

        match &self.framing {
            Framing::Datagram => {
                if !self.buffer.is_empty() {
                    on_frame(&self.buffer);
                    framed.frames = 1;
                    self.buffer.clear();
                }
            }
            Framing::Lines { max_len } => {
                let max_len = *max_len;
                let mut consumed = 0;
                while let Some(offset) = self.buffer[consumed..].iter().position(|&b| b == b'\n') {
                    let end = consumed + offset;
                    let line = &self.buffer[consumed..end];
                    // A carriage return before the line feed belongs to the line ending.
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    on_frame(line);
                    framed.frames += 1;
                    consumed = end + 1;
                }
                if consumed > 0 {
                    self.buffer.drain(..consumed);
                }
                if self.buffer.len() > max_len {
                    framed.dropped_bytes = self.buffer.len();
                    self.buffer.clear();
                }
            }
            Framing::Tail { tail, max_len } => {
                let (tail, max_len) = (tail.clone(), *max_len);
                let mut consumed = 0;
                while let Some(offset) = find(&self.buffer[consumed..], &tail) {
                    let end = consumed + offset + tail.len();
                    on_frame(&self.buffer[consumed..end]);
                    framed.frames += 1;
                    consumed = end;
                }
                if consumed > 0 {
                    self.buffer.drain(..consumed);
                }
                if self.buffer.len() > max_len {
                    framed.dropped_bytes = self.buffer.len();
                    self.buffer.clear();
                }
            }
        }

        framed
    }

    /// Hands over a frame that never completed, such as a last line without a line break.
    /// Returns whether there was anything to flush.
    pub fn flush(&mut self, mut on_frame: impl FnMut(&[u8])) -> bool {
        if self.buffer.is_empty() {
            return false;
        }
        let frame = std::mem::take(&mut self.buffer);
        on_frame(&frame);
        true
    }
}

/// Position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(framer: &mut Framer, chunks: &[&[u8]]) -> (Vec<Vec<u8>>, Framed) {
        let mut frames = Vec::new();
        let mut framed = Framed::default();
        for chunk in chunks {
            let out = framer.push(chunk, |frame| frames.push(frame.to_vec()));
            framed.frames += out.frames;
            framed.dropped_bytes += out.dropped_bytes;
        }
        (frames, framed)
    }

    const TAIL: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

    #[test]
    fn tail_frames_survive_being_cut_anywhere() {
        let mut framer = Framer::new(Framing::just_float_tail());
        // One frame of two floats: 1.0 and 2.0.
        let mut frame = Vec::new();
        frame.extend_from_slice(&1.0f32.to_le_bytes());
        frame.extend_from_slice(&2.0f32.to_le_bytes());
        frame.extend_from_slice(&TAIL);

        // Split the frame at every possible point; it must come out whole, once.
        for split in 0..frame.len() {
            let mut framer = Framer::new(Framing::just_float_tail());
            let (frames, _) = collect(&mut framer, &[&frame[..split], &frame[split..]]);
            assert_eq!(frames, [frame.clone()], "split at {split}");
            assert_eq!(framer.pending(), 0);
        }

        // Two frames in one chunk.
        let mut two = frame.clone();
        two.extend_from_slice(&frame);
        let (frames, framed) = collect(&mut framer, &[&two]);
        assert_eq!(frames.len(), 2);
        assert_eq!(framed.frames, 2);
    }

    #[test]
    fn a_tail_that_looks_like_data_ends_the_frame_early() {
        // The tail is +inf; a frame that contains it in its payload is cut there. That is the
        // protocol's own ambiguity: +inf cannot be sent as a sample value.
        let mut framer = Framer::new(Framing::just_float_tail());
        let mut frame = Vec::new();
        frame.extend_from_slice(&f32::INFINITY.to_le_bytes());
        frame.extend_from_slice(&1.0f32.to_le_bytes());
        frame.extend_from_slice(&TAIL);
        let (frames, _) = collect(&mut framer, &[&frame]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], TAIL);
        assert_eq!(frames[1].len(), 8);
    }

    #[test]
    fn over_long_input_is_dropped_instead_of_growing_without_bound() {
        let mut framer = Framer::new(Framing::Tail {
            tail: TAIL.to_vec(),
            max_len: 16,
        });
        let (frames, framed) = collect(&mut framer, &[&[0x11; 32]]);
        assert!(frames.is_empty());
        assert_eq!(framed.dropped_bytes, 32);
        assert_eq!(framer.pending(), 0);

        // Everything before the tail counts, so a frame exactly at the limit still goes through.
        let mut framer = Framer::new(Framing::Tail {
            tail: TAIL.to_vec(),
            max_len: 8,
        });
        let mut frame = vec![0x11; 4];
        frame.extend_from_slice(&TAIL);
        let (frames, _) = collect(&mut framer, &[&frame]);
        assert_eq!(frames, [frame]);
    }

    #[test]
    fn lines_come_out_without_their_break() {
        let mut framer = Framer::new(Framing::default());
        let (frames, _) = collect(&mut framer, &[b"AT\r\nOK\r\nPART", b"IAL\r\n"]);
        assert_eq!(
            frames,
            [b"AT".to_vec(), b"OK".to_vec(), b"PARTIAL".to_vec()]
        );
        assert_eq!(framer.pending(), 0);

        // A lone carriage return without a line feed is just a character.
        let mut framer = Framer::new(Framing::default());
        let (frames, _) = collect(&mut framer, &[b"a\rb\r\n"]);
        assert_eq!(frames, [b"a\rb".to_vec()]);
    }

    #[test]
    fn an_unfinished_line_waits_for_flush() {
        let mut framer = Framer::new(Framing::default());
        let (frames, _) = collect(&mut framer, &[b"partial"]);
        assert!(frames.is_empty());
        assert_eq!(framer.pending(), 7);

        let mut flushed = Vec::new();
        assert!(framer.flush(|frame| flushed.push(frame.to_vec())));
        assert_eq!(flushed, [b"partial".to_vec()]);
        assert!(!framer.flush(|_| unreachable!()));
    }

    #[test]
    fn datagrams_pass_through_whole() {
        let mut framer = Framer::new(Framing::Datagram);
        let (frames, _) = collect(&mut framer, &[b"one", b"", b"two"]);
        assert_eq!(frames, [b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_found_tail_inside_a_chunk_is_found_again_after_a_split() {
        // The tail straddles a chunk boundary: nothing may be reported before the last byte.
        let mut framer = Framer::new(Framing::just_float_tail());
        let (frames, _) = collect(&mut framer, &[&TAIL[..3]]);
        assert!(frames.is_empty());
        let (frames, _) = collect(&mut framer, &[&TAIL[3..]]);
        assert_eq!(frames, [TAIL.to_vec()]);
    }
}
