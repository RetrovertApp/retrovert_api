//! The linear-interpolation resampler, ported from the C music player's `audio_convert.c`.
//!
//! Kept step for step with that code: the C build is the parity oracle for playback, so
//! the arithmetic, the break conditions and the cross-block state all match it.

/// How many channels of cross-block state the C resampler carries.
pub(crate) const MAX_CHANNELS: usize = 8;

pub(crate) struct Resampler {
    src_rate: u32,
    dst_rate: u32,
    channels: usize,
    /// Read position in the source signal, relative to the start of the current block.
    position: f64,
    /// Last frame of the previous block, interpolated from when `position` is negative.
    prev_frame: [f32; MAX_CHANNELS],
    has_prev: bool,
}

impl Resampler {
    pub(crate) fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Self {
        Self {
            src_rate,
            dst_rate,
            channels,
            position: 0.0,
            prev_frame: [0.0; MAX_CHANNELS],
            has_prev: false,
        }
    }

    /// Source frames to ask for to produce `wanted` output frames.
    ///
    /// The scaling and its `+ 2` are the C player's, kept so a request tracks the rate
    /// ratio instead of over-reading by the whole of it every block.
    pub(crate) fn source_frames_for(&self, wanted: usize) -> usize {
        (wanted as f64 * f64::from(self.src_rate) / f64::from(self.dst_rate)) as usize + 2
    }

    /// Everything `src_frames` can yield, from any position a run can leave behind.
    ///
    /// The destination must hold all of it, as the C player's doubled resample buffer
    /// does: a run that stops on a full destination instead of on source exhaustion
    /// rebases the position past frames it never read.
    ///
    /// A run stops no earlier than `src_frames - 1` and rebases by `src_frames`, so it
    /// leaves the next one starting at `-1` at worst: a block spans at most `src_frames`
    /// positions. The spare frame absorbs the rounding.
    pub(crate) fn max_output_frames(&self, src_frames: usize) -> usize {
        (src_frames as f64 * f64::from(self.dst_rate) / f64::from(self.src_rate)).ceil() as usize
            + 1
    }

    /// Resamples `src_frames` interleaved frames into `dst`, returning frames written.
    pub(crate) fn process(
        &mut self,
        src: &[f32],
        src_frames: usize,
        dst: &mut [f32],
        dst_max_frames: usize,
    ) -> usize {
        if src_frames == 0 || dst_max_frames == 0 {
            return 0;
        }
        let channels = self.channels;

        if self.src_rate == self.dst_rate {
            let frames = src_frames.min(dst_max_frames);
            dst[..frames * channels].copy_from_slice(&src[..frames * channels]);
            return frames;
        }

        let ratio = f64::from(self.src_rate) / f64::from(self.dst_rate);
        let mut out_frames = 0;
        let mut position = self.position;

        while out_frames < dst_max_frames {
            let index = position.floor() as i64;
            let fraction = position - index as f64;

            // Both the frame at `index` and the one after it are needed.
            if index + 1 >= src_frames as i64 && index >= 0 {
                break;
            }
            if index >= src_frames as i64 {
                break;
            }

            let (first, second) = if index < 0 {
                // Carried over from the previous block.
                let first = if self.has_prev {
                    &self.prev_frame[..channels]
                } else {
                    &src[..channels]
                };
                (first, &src[..channels])
            } else {
                let base = index as usize * channels;
                (
                    &src[base..base + channels],
                    &src[base + channels..][..channels],
                )
            };

            let out = &mut dst[out_frames * channels..][..channels];
            for channel in 0..channels {
                let delta = second[channel] - first[channel];
                out[channel] = (f64::from(first[channel]) + f64::from(delta) * fraction) as f32;
            }

            out_frames += 1;
            position += ratio;
        }

        self.prev_frame[..channels]
            .copy_from_slice(&src[(src_frames - 1) * channels..][..channels]);
        self.has_prev = true;
        self.position = position - src_frames as f64;

        out_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one block through a fresh resampler and returns what it produced.
    fn resample(src: &[f32], channels: usize, src_rate: u32, dst_rate: u32) -> Vec<f32> {
        let frames = src.len() / channels;
        let mut resampler = Resampler::new(src_rate, dst_rate, channels);
        let mut dst = vec![0.0; frames * 4 * channels];
        let produced = resampler.process(src, frames, &mut dst, frames * 4);
        dst.truncate(produced * channels);
        dst
    }

    #[test]
    fn equal_rates_copy_the_block() {
        let src = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(resample(&src, 1, 48_000, 48_000), src);
    }

    #[test]
    fn doubling_the_rate_lands_on_the_midpoints() {
        let out = resample(&[0.0, 1.0, 2.0, 3.0], 1, 24_000, 48_000);
        assert_eq!(out, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
    }

    #[test]
    fn channels_are_interpolated_independently() {
        let out = resample(&[0.0, 10.0, 1.0, 20.0], 2, 24_000, 48_000);
        assert_eq!(out, [0.0, 10.0, 0.5, 15.0]);
    }

    #[test]
    fn output_stops_when_the_source_runs_out() {
        // Downsampling 4 frames by 2 leaves the last frame without a partner.
        let out = resample(&[0.0, 1.0, 2.0, 3.0], 1, 48_000, 24_000);
        assert_eq!(out, [0.0, 2.0]);
    }

    #[test]
    fn the_last_frame_carries_into_the_next_block() {
        let mut resampler = Resampler::new(24_000, 48_000, 1);
        let mut dst = [0.0; 8];

        let produced = resampler.process(&[0.0, 1.0], 2, &mut dst, 8);
        assert_eq!(&dst[..produced], [0.0, 0.5]);
        // The block starts at position -1: 1.0 is the previous block's last frame,
        // 3.0 the new block's first.
        let produced = resampler.process(&[3.0, 4.0], 2, &mut dst, 8);
        assert_eq!(&dst[..produced], [1.0, 2.0, 3.0, 3.5]);
    }

    #[test]
    fn a_full_destination_stops_the_run() {
        let mut resampler = Resampler::new(24_000, 48_000, 1);
        let mut dst = [0.0; 2];
        assert_eq!(resampler.process(&[0.0, 1.0, 2.0, 3.0], 4, &mut dst, 2), 2);
        assert_eq!(dst, [0.0, 0.5]);
    }

    #[test]
    fn an_empty_block_produces_nothing() {
        let mut resampler = Resampler::new(24_000, 48_000, 1);
        let mut dst = [0.0; 4];
        assert_eq!(resampler.process(&[], 0, &mut dst, 4), 0);
        assert_eq!(resampler.process(&[1.0], 1, &mut dst, 0), 0);
    }

    #[test]
    fn source_frames_scale_with_the_rate_ratio() {
        let resampler = Resampler::new(44_100, 48_000, 2);
        assert_eq!(resampler.source_frames_for(480), 443);
    }

    #[test]
    fn the_output_bound_scales_with_the_rate_ratio() {
        let resampler = Resampler::new(44_100, 48_000, 2);
        assert_eq!(resampler.max_output_frames(441), 481);
        // 440 frames do not land on a whole output frame, so this one pins the rounding
        // as well as the ratio.
        assert_eq!(resampler.max_output_frames(440), 480);
    }

    #[test]
    fn a_destination_sized_by_the_bound_runs_to_source_exhaustion() {
        let src: Vec<f32> = (0..64).map(|value| value as f32).collect();
        for (src_rate, dst_rate) in [
            (24_000, 48_000),
            (44_100, 48_000),
            (48_000, 24_000),
            (8_000, 48_000),
            (48_000, 44_100),
        ] {
            let mut resampler = Resampler::new(src_rate, dst_rate, 1);
            // Four blocks: only the first starts from a fresh position.
            for block in 0..4 {
                let bound = resampler.max_output_frames(src.len());
                let mut dst = vec![0.0; bound];
                let produced = resampler.process(&src, src.len(), &mut dst, bound);
                assert!(
                    produced < bound,
                    "{src_rate} -> {dst_rate} block {block} filled the destination \
                     ({produced} of {bound}), so the source may not have run out"
                );
            }
        }
    }
}
