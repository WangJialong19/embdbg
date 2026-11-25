//! All channels of one data stream, sharing one frame index and one timestamp log.

use std::ops::Range;

use super::batch::{BatchLog, TimeAxis};
use super::channel::{ChannelRing, normalize_capacity};

/// One timestamp entry per this many samples at the very least.
const BATCH_LOG_DIVISOR: usize = 16;

/// Channels of one data stream.
///
/// A *frame* is one decoded data unit holding one sample per channel. All channels are
/// indexed by frame, so the X-Y plot (and everything else) can pair samples of different
/// channels without aligning them first.
#[derive(Debug)]
pub struct ChannelSet {
    capacity: usize,
    batches: BatchLog,
    /// Frames pushed so far.
    frames: u64,
    channels: Vec<ChannelRing>,
}

impl ChannelSet {
    /// Creates an empty set that retains `capacity` samples per channel.
    pub fn new(capacity: usize) -> Self {
        let capacity = normalize_capacity(capacity);
        Self {
            capacity,
            batches: BatchLog::with_capacity((capacity / BATCH_LOG_DIVISOR).max(16)),
            frames: 0,
            channels: Vec::new(),
        }
    }

    /// Appends one frame that arrived at time `t`: `values[i]` is the sample of channel `i`.
    pub fn push_batch(&mut self, t: f64, values: &[f32]) {
        self.push_frames(t, values.len(), values);
    }

    /// Appends frames that all arrived together at time `t`, laid out frame by frame:
    /// `flat[frame * channels + channel]`. One arrival is one batch, so it gets one timestamp.
    ///
    /// Channels that do not exist yet are created on the fly, so later frames may carry more channels
    /// than earlier ones. Channels a frame does not reach get NaN, which keeps every channel
    /// aligned to the same frames.
    pub fn push_frames(&mut self, t: f64, channels: usize, flat: &[f32]) {
        if channels == 0 || flat.len() < channels {
            return;
        }
        self.begin_batch(t, channels, (flat.len() / channels) as u64);
        self.push_chunk(channels, flat);
    }

    /// Starts a batch of `frames` frames that arrived at time `t`, without storing any sample yet.
    ///
    /// The samples follow in any number of [`Self::push_chunk`] calls, in order. A reader that
    /// looks in between sees a batch whose later frames are not stored yet; that is safe, because
    /// each channel only reports the frames it actually holds.
    pub fn begin_batch(&mut self, t: f64, channels: usize, frames: u64) {
        if channels == 0 || frames == 0 {
            return;
        }
        let start = self.frames;
        self.batches.push(t, start);
        self.frames += frames;
        if self.channels.len() < channels {
            self.channels.reserve(channels - self.channels.len());
            while self.channels.len() < channels {
                let index = self.channels.len();
                self.channels.push(ChannelRing::new(
                    format!("通道 {}", index + 1),
                    self.capacity,
                    start,
                ));
            }
        }
    }

    /// Stores the next frames of the batch started with [`Self::begin_batch`], laid out as in
    /// [`Self::push_frames`].
    pub fn push_chunk(&mut self, channels: usize, flat: &[f32]) {
        if channels == 0 {
            return;
        }
        for values in flat.chunks_exact(channels) {
            for (i, channel) in self.channels.iter_mut().enumerate() {
                channel.push(values.get(i).copied().unwrap_or(f32::NAN));
            }
        }
    }

    pub fn clear(&mut self) {
        self.batches.clear();
        self.frames = 0;
        self.channels.clear();
    }

    pub fn channels(&self) -> &[ChannelRing] {
        &self.channels
    }

    pub fn channel(&self, index: usize) -> Option<&ChannelRing> {
        self.channels.get(index)
    }

    pub fn channel_mut(&mut self, index: usize) -> Option<&mut ChannelRing> {
        self.channels.get_mut(index)
    }

    /// Suggests a name for each channel, in order. Channels the user already named keep their name.
    pub fn set_default_names(&mut self, names: &[&str]) {
        for (channel, name) in self.channels.iter_mut().zip(names) {
            channel.set_default_name(*name);
        }
    }

    /// Frames pushed so far, which is also the frame index of the next sample.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The shared index ↔ time mapping.
    pub fn axis(&self) -> TimeAxis<'_> {
        TimeAxis::new(&self.batches, self.frames)
    }

    /// Frames that can be plotted: retained by every channel and covered by the timestamp log.
    /// Returns `None` while the set is empty.
    pub fn plottable_range(&self) -> Option<Range<u64>> {
        let start = self.batches.first_frame()?;
        let end = self.frames;
        let start = self
            .channels
            .iter()
            .map(|c| c.oldest())
            .max()
            .unwrap_or(start)
            .max(start);
        (start < end).then_some(start..end)
    }

    /// Appends up to `max_points` `(x, y)` sample pairs of two channels to `out`, evenly spread over `range`.
    ///
    /// Frames where either channel is NaN are left out.
    pub fn xy_points(
        &self,
        x_channel: usize,
        y_channel: usize,
        range: Range<u64>,
        max_points: usize,
        out: &mut Vec<[f32; 2]>,
    ) {
        let Some((x, y)) = self.channel(x_channel).zip(self.channel(y_channel)) else {
            return;
        };
        let start = range.start.max(x.oldest()).max(y.oldest());
        let end = range.end.min(x.newest()).min(y.newest());
        if start >= end || max_points == 0 {
            return;
        }
        let step = ((end - start).div_ceil(max_points as u64)).max(1);
        for frame in (start..end).step_by(step as usize) {
            if let (Some(xv), Some(yv)) = (x.value_at(frame), y.value_at(frame))
                && !xv.is_nan()
                && !yv.is_nan()
            {
                out.push([xv, yv]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_created_as_frames_grow() {
        let mut set = ChannelSet::new(1024);
        assert_eq!(set.plottable_range(), None);
        set.push_batch(0.0, &[1.0, 2.0]);
        set.push_batch(0.1, &[3.0, 4.0, 5.0]);
        set.push_batch(0.2, &[6.0]);

        assert_eq!(set.channels().len(), 3);
        assert_eq!(set.frames(), 3);
        let names: Vec<&str> = set.channels().iter().map(|c| c.name()).collect();
        assert_eq!(names, ["通道 1", "通道 2", "通道 3"]);

        let v = |ch: usize, frame: u64| set.channel(ch).unwrap().value_at(frame);
        assert_eq!(v(0, 2), Some(6.0));
        // Channel 2 missed the third frame.
        assert!(v(1, 2).unwrap().is_nan());
        // Channel 3 only exists from the second frame on: before that there is nothing to read.
        assert_eq!(v(2, 0), None);
        assert_eq!(v(2, 1), Some(5.0));
        assert!(v(2, 2).unwrap().is_nan());
    }

    #[test]
    fn a_batch_may_be_filled_in_pieces() {
        let mut set = ChannelSet::new(1024);
        set.begin_batch(1.0, 2, 4);
        set.push_chunk(2, &[1.0, 2.0, 3.0, 4.0]);
        // Declared whole, half stored: readers see four frames, of which each channel holds two.
        assert_eq!(set.frames(), 4);
        assert_eq!(set.channel(0).unwrap().newest(), 2);
        set.push_chunk(2, &[5.0, 6.0]);
        set.push_chunk(2, &[7.0, 8.0]);
        assert_eq!(set.channel(1).unwrap().stats().last, Some(8.0));
        assert_eq!(set.channel(0).unwrap().retained(), 0..4);
        // Whatever the piecing, it is one arrival with one timestamp.
        assert_eq!(set.axis().frame_rate(), None);

        // Nothing to store: no batch, no channel.
        set.begin_batch(2.0, 0, 4);
        set.begin_batch(2.0, 2, 0);
        set.push_chunk(0, &[1.0]);
        assert_eq!(set.frames(), 4);
    }

    #[test]
    fn new_channels_start_at_the_start_of_their_batch() {
        let mut set = ChannelSet::new(1024);
        set.push_frames(1.0, 2, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(set.channel(1).unwrap().retained(), 0..2);
        assert_eq!(set.channel(1).unwrap().value_at(0), Some(2.0));
    }

    #[test]
    fn suggested_names_do_not_override_the_users() {
        let mut set = ChannelSet::new(1024);
        set.push_batch(0.0, &[1.0, 2.0]);
        set.set_default_names(&["正弦", "方波"]);
        set.channel_mut(0).unwrap().set_name("我的通道");
        // A restarted source suggests its names again.
        set.set_default_names(&["噪声", "电源"]);
        let names: Vec<&str> = set.channels().iter().map(|c| c.name()).collect();
        assert_eq!(names, ["我的通道", "电源"]);
    }

    #[test]
    fn frames_arriving_together_share_one_timestamp() {
        let mut set = ChannelSet::new(1024);
        // Two frames of two channels, arrived at t = 1.
        set.push_frames(1.0, 2, &[10.0, 11.0, 12.0, 13.0]);
        assert_eq!(set.channels().len(), 2);
        assert_eq!(set.frames(), 2);
        assert_eq!(set.channel(0).unwrap().stats().last, Some(12.0));
        assert_eq!(set.channel(1).unwrap().stats().last, Some(13.0));
        assert_eq!(set.axis().time_at(0), Some(1.0));
        assert_eq!(set.axis().time_at(1), Some(1.0));
        assert_eq!(set.axis().frame_rate(), None);

        set.push_frames(2.0, 2, &[14.0, 15.0]);
        // The first batch still has no predecessor, so its frames keep sharing t = 1.
        assert_eq!(set.axis().time_at(1), Some(1.0));
        assert_eq!(set.axis().time_at(2), Some(2.0));
        assert!((set.axis().frame_rate().unwrap() - 2.0).abs() < 1e-9);

        // Too short to hold a whole frame, or no channels at all: ignored.
        set.push_frames(3.0, 2, &[1.0]);
        set.push_frames(3.0, 0, &[1.0]);
        assert_eq!(set.frames(), 3);
    }

    #[test]
    fn plottable_range_is_bounded_by_the_timestamp_log() {
        let mut set = ChannelSet::new(1024);
        for i in 0..40u64 {
            set.push_batch(i as f64, &[i as f32]);
        }
        assert_eq!(set.plottable_range(), Some(0..40));
        assert_eq!(set.axis().time_at(10), Some(10.0));
        // Eleven one-frame batches arrived by t = 10: frames 0..11.
        assert_eq!(set.axis().index_at_time(10.0), Some(11));

        set.clear();
        assert_eq!(set.plottable_range(), None);
        assert_eq!(set.channels().len(), 0);
        assert_eq!(set.frames(), 0);
    }

    #[test]
    fn xy_pairs_align_on_frames_and_skip_gaps() {
        let mut set = ChannelSet::new(1024);
        for i in 0..10u64 {
            set.push_batch(i as f64, &[i as f32, (i * i) as f32]);
        }
        set.channel_mut(1).unwrap().set_name("平方");
        let out = &mut Vec::new();
        set.xy_points(0, 1, 0..10, 4, out);
        // Ten frames, at most four points → every third frame.
        assert_eq!(*out, [[0.0, 0.0], [3.0, 9.0], [6.0, 36.0], [9.0, 81.0]]);

        out.clear();
        set.xy_points(0, 1, 4..8, 100, out);
        assert_eq!(out.len(), 4);

        out.clear();
        set.xy_points(0, 9, 0..10, 100, out);
        assert!(out.is_empty());
    }
}
