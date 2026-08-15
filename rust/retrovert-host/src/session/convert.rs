//! Sample-format normalization and channel adaptation.
//!
//! Byte order is the host's, as in the C hosts this ports from — the ABI has none of
//! its own.

use crate::ffi::audio_format::RVAudioStreamFormat;

#[cfg(any(test, target_endian = "little"))]
fn s24_from_le_bytes(bytes: [u8; 3]) -> i32 {
    let sign = if bytes[2] & 0x80 == 0 { 0 } else { 0xff };
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], sign])
}

#[cfg(any(test, target_endian = "big"))]
fn s24_from_be_bytes(bytes: [u8; 3]) -> i32 {
    let sign = if bytes[0] & 0x80 == 0 { 0 } else { 0xff };
    i32::from_be_bytes([sign, bytes[0], bytes[1], bytes[2]])
}

#[cfg(target_endian = "little")]
fn s24_from_ne_bytes(bytes: [u8; 3]) -> i32 {
    s24_from_le_bytes(bytes)
}

#[cfg(target_endian = "big")]
fn s24_from_ne_bytes(bytes: [u8; 3]) -> i32 {
    s24_from_be_bytes(bytes)
}

/// Bytes one sample of `format` occupies in the plugin's buffer.
pub(crate) fn sample_width(format: RVAudioStreamFormat) -> usize {
    match format {
        RVAudioStreamFormat::U8 => 1,
        RVAudioStreamFormat::S16 => 2,
        RVAudioStreamFormat::S24 => 3,
        RVAudioStreamFormat::S32 | RVAudioStreamFormat::F32 => 4,
    }
}

/// Normalizes interleaved samples of any stream format into f32.
///
/// `input` must hold `output.len()` samples of `format`.
pub(crate) fn to_f32(format: RVAudioStreamFormat, input: &[u8], output: &mut [f32]) {
    const U8_SCALE: f32 = 1.0 / 128.0;
    const S16_SCALE: f32 = 1.0 / 32_768.0;
    const S24_SCALE: f32 = 1.0 / 8_388_608.0;
    const S32_SCALE: f32 = 1.0 / 2_147_483_648.0;

    let width = sample_width(format);
    assert_eq!(input.len(), output.len() * width);

    match format {
        RVAudioStreamFormat::U8 => {
            for (&byte, sample) in input.iter().zip(output.iter_mut()) {
                *sample = (f32::from(byte) - 128.0) * U8_SCALE;
            }
        }
        RVAudioStreamFormat::S16 => {
            for (bytes, sample) in input.chunks_exact(2).zip(output.iter_mut()) {
                *sample = f32::from(i16::from_ne_bytes([bytes[0], bytes[1]])) * S16_SCALE;
            }
        }
        RVAudioStreamFormat::S24 => {
            for (bytes, sample) in input.chunks_exact(3).zip(output.iter_mut()) {
                *sample = s24_from_ne_bytes([bytes[0], bytes[1], bytes[2]]) as f32 * S24_SCALE;
            }
        }
        RVAudioStreamFormat::S32 => {
            for (bytes, sample) in input.chunks_exact(4).zip(output.iter_mut()) {
                *sample =
                    i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 * S32_SCALE;
            }
        }
        RVAudioStreamFormat::F32 => {
            for (bytes, sample) in input.chunks_exact(4).zip(output.iter_mut()) {
                *sample = f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            }
        }
    }
}

/// Whether the built-in adaptation covers this pair: mono fans out, stereo folds down.
/// Anything else would need a channel matrix, which the session deliberately lacks.
pub(crate) fn adaptation_supported(from: u32, to: u32) -> bool {
    from == to || from == 1 || (from == 2 && to == 1)
}

/// Rewrites `frames` interleaved frames from `src_channels` to `dst_channels`.
///
/// Only the pairs [`adaptation_supported`] accepts arrive here, and never an
/// unchanged channel count — that path copies nothing.
pub(crate) fn adapt(src: &[f32], src_channels: usize, dst: &mut [f32], dst_channels: usize) {
    if src_channels == 1 {
        for (frame, sample) in src.iter().enumerate() {
            dst[frame * dst_channels..][..dst_channels].fill(*sample);
        }
    } else {
        for (frame, target) in dst.iter_mut().enumerate() {
            *target = (src[frame * 2] + src[frame * 2 + 1]) * 0.5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(format: RVAudioStreamFormat, input: &[u8]) -> Vec<f32> {
        let mut output = vec![0.0; input.len() / sample_width(format)];
        to_f32(format, input, &mut output);
        output
    }

    #[test]
    fn unsigned_eight_bit_centres_on_128() {
        assert_eq!(
            convert(RVAudioStreamFormat::U8, &[0, 128, 255]),
            [-1.0, 0.0, 127.0 / 128.0]
        );
    }

    #[test]
    fn signed_sixteen_bit_scales_by_the_negative_bound() {
        let input: Vec<u8> = [0_i16, i16::MAX, i16::MIN, -16384]
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        assert_eq!(
            convert(RVAudioStreamFormat::S16, &input),
            [0.0, 32767.0 / 32768.0, -1.0, -0.5]
        );
    }

    #[test]
    fn signed_twenty_four_bit_little_endian_values_decode() {
        let input = [
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0x00, 0x00,
            0x80,
        ];
        assert_eq!(
            input
                .chunks_exact(3)
                .map(|bytes| s24_from_le_bytes([bytes[0], bytes[1], bytes[2]]))
                .collect::<Vec<_>>(),
            [0, 1, 8_388_607, -1, -8_388_608]
        );
    }

    #[test]
    fn signed_twenty_four_bit_big_endian_values_decode() {
        let input = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0x80, 0x00,
            0x00,
        ];
        assert_eq!(
            input
                .chunks_exact(3)
                .map(|bytes| s24_from_be_bytes([bytes[0], bytes[1], bytes[2]]))
                .collect::<Vec<_>>(),
            [0, 1, 8_388_607, -1, -8_388_608]
        );
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn signed_twenty_four_bit_native_conversion_is_little_endian() {
        assert_eq!(
            convert(
                RVAudioStreamFormat::S24,
                &[
                    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0x00,
                    0x00, 0x80,
                ]
            ),
            [
                0.0,
                1.0 / 8_388_608.0,
                8_388_607.0 / 8_388_608.0,
                -1.0 / 8_388_608.0,
                -1.0
            ]
        );
    }

    #[cfg(target_endian = "big")]
    #[test]
    fn signed_twenty_four_bit_native_conversion_is_big_endian() {
        assert_eq!(
            convert(
                RVAudioStreamFormat::S24,
                &[
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0x80,
                    0x00, 0x00,
                ]
            ),
            [
                0.0,
                1.0 / 8_388_608.0,
                8_388_607.0 / 8_388_608.0,
                -1.0 / 8_388_608.0,
                -1.0
            ]
        );
    }

    #[test]
    fn signed_thirty_two_bit_scales_by_the_negative_bound() {
        let input: Vec<u8> = [0_i32, i32::MIN, i32::MIN / 2]
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        assert_eq!(convert(RVAudioStreamFormat::S32, &input), [0.0, -1.0, -0.5]);
    }

    #[test]
    fn floats_pass_through() {
        let input: Vec<u8> = [0.25_f32, -0.75]
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        assert_eq!(convert(RVAudioStreamFormat::F32, &input), [0.25, -0.75]);
    }

    #[test]
    fn adaptation_covers_fan_out_and_fold_down_only() {
        assert!(adaptation_supported(1, 1));
        assert!(adaptation_supported(2, 2));
        assert!(adaptation_supported(1, 8));
        assert!(adaptation_supported(2, 1));
        assert!(!adaptation_supported(2, 4));
    }

    #[test]
    fn mono_is_duplicated_across_every_target_channel() {
        let mut dst = [0.0; 6];
        adapt(&[1.0, -1.0], 1, &mut dst, 3);
        assert_eq!(dst, [1.0, 1.0, 1.0, -1.0, -1.0, -1.0]);
    }

    #[test]
    fn stereo_folds_to_the_average() {
        let mut dst = [0.0; 2];
        adapt(&[1.0, 0.0, -0.5, 0.5], 2, &mut dst, 1);
        assert_eq!(dst, [0.5, 0.0]);
    }
}
