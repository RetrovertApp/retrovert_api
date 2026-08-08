//! Mirror of `retrovert/audio_format.h`.

abi_enum! {
    /// Sample format of an audio stream.
    RVAudioStreamFormat {
        U8 = 1,
        S16 = 2,
        S24 = 3,
        S32 = 4,
        F32 = 5,
    }
}

/// Sample format, channel count and sample rate of a block of audio.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVAudioFormat {
    pub audio_format: u32,
    pub channel_count: u32,
    pub sample_rate: u32,
}
