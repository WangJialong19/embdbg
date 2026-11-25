//! Turning frames into samples or text.

/// The tail the JustFloat and JustInt protocols share: little-endian `+inf`.
pub const JUST_FLOAT_TAIL: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

/// How the bytes of a frame become data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoderSpec {
    /// Little-endian `f32` samples, then the tail `00 00 80 7F`: VOFA+'s JustFloat engine and the
    /// protocol several vendor tutorials use.
    JustFloat,
    /// The same layout with little-endian `i32` samples, from the 山外/逐飞 assistant library.
    JustInt,
    /// A text protocol: the frame is one line of text, such as an AT command answer.
    #[default]
    Line,
}

impl DecoderSpec {
    /// The name the MCP tools use.
    pub fn id(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::JustFloat => "justfloat",
            Self::JustInt => "justint",
        }
    }

    /// What the protocol needs from the framer.
    pub fn framing(self) -> super::framing::Framing {
        match self {
            Self::JustFloat | Self::JustInt => super::framing::Framing::just_float_tail(),
            Self::Line => super::framing::Framing::default(),
        }
    }
}

/// What one frame turned into.
#[derive(Debug, PartialEq)]
pub enum Decoded<'a> {
    /// One sample per channel, in frame order.
    Samples(&'a [f32]),
    /// A line of text.
    Line(&'a [u8]),
    /// Nothing usable: the wrong length, a missing tail, or an empty frame.
    Ignored,
}

/// Decodes frames according to its spec, reusing its sample buffer.
#[derive(Debug)]
pub struct Decoder {
    spec: DecoderSpec,
    samples: Vec<f32>,
}

impl Decoder {
    pub fn new(spec: DecoderSpec) -> Self {
        Self {
            spec,
            samples: Vec::new(),
        }
    }

    pub fn spec(&self) -> DecoderSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: DecoderSpec) {
        self.spec = spec;
    }

    /// Decodes one frame. The result borrows from either `frame` or the decoder.
    pub fn decode<'a>(&'a mut self, frame: &'a [u8]) -> Decoded<'a> {
        match self.spec {
            DecoderSpec::JustFloat => {
                let Some(payload) = strip_tail(frame) else {
                    return Decoded::Ignored;
                };
                if payload.is_empty() || !payload.len().is_multiple_of(4) {
                    return Decoded::Ignored;
                }
                self.samples.clear();
                self.samples.extend(
                    payload
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|bytes| f32::from_le_bytes(*bytes)),
                );
                Decoded::Samples(&self.samples)
            }
            DecoderSpec::JustInt => {
                let Some(payload) = strip_tail(frame) else {
                    return Decoded::Ignored;
                };
                if payload.is_empty() || !payload.len().is_multiple_of(4) {
                    return Decoded::Ignored;
                }
                self.samples.clear();
                self.samples.extend(
                    payload
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|bytes| i32::from_le_bytes(*bytes) as f32),
                );
                Decoded::Samples(&self.samples)
            }
            DecoderSpec::Line => {
                if frame.is_empty() {
                    Decoded::Ignored
                } else {
                    Decoded::Line(frame)
                }
            }
        }
    }
}

/// The payload of a JustFloat/JustInt frame, or `None` if the tail is missing.
fn strip_tail(frame: &[u8]) -> Option<&[u8]> {
    frame.strip_suffix(&JUST_FLOAT_TAIL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One JustFloat frame of four floats.
    fn just_float(values: &[f32]) -> Vec<u8> {
        let mut frame = Vec::new();
        for value in values {
            frame.extend_from_slice(&value.to_le_bytes());
        }
        frame.extend_from_slice(&JUST_FLOAT_TAIL);
        frame
    }

    #[test]
    fn just_float_reads_little_endian_floats() {
        let mut decoder = Decoder::new(DecoderSpec::JustFloat);
        let frame = just_float(&[1.0, -2.5, 3.25]);
        match decoder.decode(&frame) {
            Decoded::Samples(samples) => assert_eq!(samples, [1.0, -2.5, 3.25]),
            other => panic!("unexpected {other:?}"),
        }

        // One value per frame is a valid stream too; the channel count is whatever the frame holds.
        let frame = just_float(&[42.0]);
        assert_eq!(decoder.decode(&frame), Decoded::Samples(&[42.0]));
    }

    #[test]
    fn just_float_ignores_frames_without_a_tail_or_with_a_partial_sample() {
        let mut decoder = Decoder::new(DecoderSpec::JustFloat);
        assert_eq!(decoder.decode(&[0x00, 0x00, 0x80, 0x3F]), Decoded::Ignored);
        assert_eq!(
            decoder.decode(&[0x01, 0x02, 0x03]),
            Decoded::Ignored,
            "three bytes are not a whole float"
        );
        assert_eq!(
            decoder.decode(&JUST_FLOAT_TAIL),
            Decoded::Ignored,
            "no samples"
        );
    }

    #[test]
    fn just_int_reads_little_endian_integers() {
        let mut decoder = Decoder::new(DecoderSpec::JustInt);
        let mut frame = Vec::new();
        for value in [1000i32, -5, 42] {
            frame.extend_from_slice(&value.to_le_bytes());
        }
        frame.extend_from_slice(&JUST_FLOAT_TAIL);
        match decoder.decode(&frame) {
            Decoded::Samples(samples) => assert_eq!(samples, [1000.0, -5.0, 42.0]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn lines_are_text_and_empty_frames_are_ignored() {
        let mut decoder = Decoder::new(DecoderSpec::Line);
        assert_eq!(decoder.decode(b"OK"), Decoded::Line(b"OK"));
        assert_eq!(decoder.decode(b""), Decoded::Ignored);
        decoder.set_spec(DecoderSpec::JustFloat);
        assert_eq!(decoder.spec(), DecoderSpec::JustFloat);
    }

    #[test]
    fn the_protocol_chooses_its_framing() {
        assert_eq!(
            DecoderSpec::JustFloat.framing(),
            super::super::framing::Framing::Tail {
                tail: JUST_FLOAT_TAIL.to_vec(),
                max_len: 64 * 1024
            }
        );
        assert!(matches!(
            DecoderSpec::Line.framing(),
            super::super::framing::Framing::Lines { .. }
        ));
    }
}
