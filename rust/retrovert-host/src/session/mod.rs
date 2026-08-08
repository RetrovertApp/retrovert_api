//! The decode session: one plugin, one open song, one synchronous pull.
//!
//! Nothing here threads or paces: the caller names a frame budget and gets one validated
//! [`Chunk`] of f32 audio back. Decoding through a resample plugin is not wired up; the
//! loader can load the kind, and one can drop in later.

mod convert;
mod resample;

use core::ffi::c_void;
use core::ptr::NonNull;
use std::ffi::CString;

use crate::ffi::audio_format::{RVAudioFormat, RVAudioStreamFormat};
use crate::ffi::playback::{
    RVPlaybackPlugin, RVReadData, RVReadInfo, RVReadStatus, RVSettingsUpdate,
};
use crate::ffi::service::RVService;
use crate::loader::LoadedPlugin;
use crate::service::{MetadataHandle, ServiceHost};
use resample::Resampler;

/// The most channels a target format can ask for, bounded by the resampler's state.
pub const MAX_TARGET_CHANNELS: u32 = resample::MAX_CHANNELS as u32;

/// The most channels a plugin may report. Both C hosts accept mono and stereo only.
const MAX_NATIVE_CHANNELS: u32 = 2;

/// Widest sample the ABI defines, in bytes.
const MAX_SAMPLE_WIDTH: usize = 4;

/// Keeps the advertised buffer capacity inside the `u32` the ABI carries it in.
const MAX_REQUEST_FRAMES: usize =
    u32::MAX as usize / (MAX_NATIVE_CHANNELS as usize * MAX_SAMPLE_WIDTH);

/// What both C hosts ask for when they have no target format of their own.
const DEFAULT_HINT: StreamFormat = StreamFormat {
    sample_rate: 48_000,
    channels: 2,
};

type ReadDataFn = unsafe extern "C" fn(*mut c_void, RVReadData) -> RVReadInfo;

/// Sample rate and channel count of a block of f32 audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u32,
}

/// The format a plugin committed to with its first chunk and must keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatLock {
    pub sample_format: RVAudioStreamFormat,
    pub sample_rate: u32,
    pub channels: u32,
}

/// One pull's worth of interleaved f32 audio.
#[derive(Debug)]
pub struct Chunk<'a> {
    pub samples: &'a [f32],
    pub format: StreamFormat,
    /// The song ended here; later reads produce nothing.
    pub finished: bool,
}

impl Chunk<'_> {
    pub fn frames(&self) -> usize {
        match self.format.channels as usize {
            0 => 0,
            channels => self.samples.len() / channels,
        }
    }
}

/// A plugin broke the contract `read_data` is written against.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AbiViolation {
    #[error("status {0} is unknown")]
    UnknownStatus(u32),
    #[error("status is a request value, not a return value")]
    RequestStatus,
    #[error("audio format {0} is unknown")]
    UnknownSampleFormat(u32),
    #[error("frame_count {returned} exceeds the {requested} requested")]
    FrameCount { returned: u32, requested: u32 },
    #[error("channel_count {0} is neither mono nor stereo")]
    ChannelCount(u32),
    #[error("sample_rate 0 cannot define a timebase")]
    ZeroSampleRate,
    #[error("format changed from {expected:?} to {found:?}")]
    FormatChanged {
        expected: FormatLock,
        found: FormatLock,
    },
    #[error("returned audio needs {needed} bytes, buffer holds {available}")]
    BufferOverrun { needed: usize, available: usize },
}

/// Why a session could not start or could not continue.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("not a playback plugin")]
    NotPlayback,
    #[error("plugin has no {0}()")]
    MissingCallback(&'static str),
    #[error("url contains an interior NUL")]
    InvalidUrl,
    #[error("create() returned null")]
    CreateFailed,
    #[error("open() failed with {0}")]
    OpenFailed(i32),
    #[error("target format {} Hz / {} channels is not playable", .0.sample_rate, .0.channels)]
    InvalidTarget(StreamFormat),
    #[error("no built-in adaptation from {from} to {to} channels")]
    ChannelAdaptation { from: u32, to: u32 },
    #[error("read_data() reported an error")]
    Decode,
    #[error("ABI violation: {0}")]
    Abi(#[from] AbiViolation),
}

/// A loaded playback plugin, brought up for as long as this lives.
///
/// `static_init` runs here and `static_destroy` on drop, both of which are global to the
/// library — so at most one `Plugin` may exist per [`LoadedPlugin`] at a time.
pub struct Plugin<'a> {
    plugin: &'a RVPlaybackPlugin,
    service: &'a RVService,
    metadata: MetadataHandle,
}

impl<'a> Plugin<'a> {
    pub fn new(loaded: &'a LoadedPlugin, host: &'a ServiceHost) -> Result<Self, SessionError> {
        let plugin = loaded.playback().ok_or(SessionError::NotPlayback)?;
        Ok(Self::from_descriptor(plugin, host))
    }

    fn from_descriptor(plugin: &'a RVPlaybackPlugin, host: &'a ServiceHost) -> Self {
        let service = host.service();
        if let Some(static_init) = plugin.static_init {
            // SAFETY: the plugin published this callback, and the service locator stays
            // live for as long as the host that owns it.
            unsafe { static_init(service) };
        }
        Self {
            plugin,
            service,
            metadata: host.metadata(),
        }
    }

    /// Opens a song, leaving conversion to the caller: chunks arrive at the plugin's own
    /// rate and channel count.
    pub fn open(&self, url: &str, subsong: u32) -> Result<Player<'_>, SessionError> {
        self.open_inner(url, subsong, None)
    }

    /// Opens a song for mixing at `target`: chunks arrive resampled and channel-adapted.
    pub fn open_with_target(
        &self,
        url: &str,
        subsong: u32,
        target: StreamFormat,
    ) -> Result<Player<'_>, SessionError> {
        self.open_inner(url, subsong, Some(target))
    }

    fn open_inner(
        &self,
        url: &str,
        subsong: u32,
        target: Option<StreamFormat>,
    ) -> Result<Player<'_>, SessionError> {
        if let Some(target) = target {
            if target.sample_rate == 0
                || target.channels == 0
                || target.channels > MAX_TARGET_CHANNELS
            {
                return Err(SessionError::InvalidTarget(target));
            }
        }
        let create = self
            .plugin
            .create
            .ok_or(SessionError::MissingCallback("create"))?;
        let open = self
            .plugin
            .open
            .ok_or(SessionError::MissingCallback("open"))?;
        let read_data = self
            .plugin
            .read_data
            .ok_or(SessionError::MissingCallback("read_data"))?;
        let url = CString::new(url).map_err(|_| SessionError::InvalidUrl)?;

        // SAFETY: the service locator stays live for the whole instance lifetime.
        let instance = unsafe { create(self.service) };
        let instance = NonNull::new(instance).ok_or(SessionError::CreateFailed)?;

        // Owned from here on, so a failed open still destroys the instance.
        let mut player = Player {
            plugin: self.plugin,
            service: self.service,
            instance,
            read_data,
            opened: false,
            target,
            lock: None,
            resampler: None,
            finished: false,
            carry: 0,
            carry_at: 0,
            buffers: Buffers::default(),
        };

        // SAFETY: the instance came from this plugin's `create`, and both the url and
        // the service outlive the call.
        let result = unsafe { open(instance.as_ptr(), url.as_ptr(), subsong, self.service) };
        if result != 0 {
            // A failed open still pushed whatever it got to; the next song must not see it.
            self.metadata.discard();
            return Err(SessionError::OpenFailed(result));
        }
        player.opened = true;
        Ok(player)
    }
}

impl Drop for Plugin<'_> {
    fn drop(&mut self) {
        if let Some(static_destroy) = self.plugin.static_destroy {
            // SAFETY: every player made from this plugin borrows it, so all of them are
            // gone by now.
            unsafe { static_destroy() };
        }
    }
}

/// One open song. Dropping it closes the song and destroys the instance.
pub struct Player<'a> {
    plugin: &'a RVPlaybackPlugin,
    service: &'a RVService,
    instance: NonNull<c_void>,
    read_data: ReadDataFn,
    opened: bool,
    target: Option<StreamFormat>,
    lock: Option<FormatLock>,
    resampler: Option<Resampler>,
    finished: bool,
    /// Frames the last read produced past its budget, waiting at `carry_at` in `out`.
    carry: usize,
    carry_at: usize,
    buffers: Buffers,
}

impl Player<'_> {
    /// What the plugin locked to, once it has produced a chunk.
    pub fn native_format(&self) -> Option<FormatLock> {
        self.lock
    }

    /// Rate and channel count of delivered samples. Zeroed until the first chunk when no
    /// target was set — the plugin has not said what it plays yet.
    pub fn format(&self) -> StreamFormat {
        self.target.unwrap_or(match self.lock {
            Some(lock) => StreamFormat {
                sample_rate: lock.sample_rate,
                channels: lock.channels,
            },
            None => StreamFormat {
                sample_rate: 0,
                channels: 0,
            },
        })
    }

    /// Tells the plugin its settings changed. Anything but `RequireRestart` — including a
    /// plugin without the callback — means the change took effect live.
    pub fn settings_updated(&mut self) -> RVSettingsUpdate {
        let Some(settings_updated) = self.plugin.settings_updated else {
            return RVSettingsUpdate::Default;
        };
        // SAFETY: the instance is live and the service outlives the call.
        let result = unsafe { settings_updated(self.instance.as_ptr(), self.service) };
        match RVSettingsUpdate::from_raw(result) {
            Some(RVSettingsUpdate::RequireRestart) => RVSettingsUpdate::RequireRestart,
            _ => RVSettingsUpdate::Default,
        }
    }

    /// Pulls up to `frames` frames, reading the plugin as many times as that takes.
    ///
    /// A short chunk means the song ended, the plugin ran dry, or the resampler is still
    /// gathering source frames.
    pub fn read(&mut self, frames: u32) -> Result<Chunk<'_>, SessionError> {
        let budget = (frames as usize).min(MAX_REQUEST_FRAMES);
        let mut produced = self.take_carry();

        while produced < budget && !self.finished {
            let request = self.source_frames(budget - produced);
            self.reserve(request, budget);

            let info = self.pull(request);
            let (returned, format, finished) = self.validate(info, request)?;
            if self.lock.is_none() {
                self.start(format)?;
                // A resampler may have just appeared, and it needs its slack.
                self.reserve(request, budget);
            }
            self.finished |= finished;
            if returned == 0 {
                break;
            }
            produced += self.consume(returned, produced);
        }

        // The resampler consumed the source behind any frames past the budget, so they
        // ride to the next read rather than being dropped.
        if produced > budget {
            self.carry = produced - budget;
            self.carry_at = budget;
            produced = budget;
        }

        let format = self.format();
        Ok(Chunk {
            samples: &self.buffers.out[..produced * format.channels as usize],
            format,
            finished: self.finished && self.carry == 0,
        })
    }

    /// Moves the last read's surplus to the front of the output buffer.
    fn take_carry(&mut self) -> usize {
        let carry = core::mem::take(&mut self.carry);
        if carry > 0 {
            let channels = self.out_channels();
            let start = self.carry_at * channels;
            self.buffers
                .out
                .copy_within(start..start + carry * channels, 0);
        }
        carry
    }

    /// Sizes the buffers for one pull. The output buffer holds the budget plus everything
    /// the resampler could produce from this request, so the run always ends at source
    /// exhaustion; whatever lands past the budget rides to the next read.
    fn reserve(&mut self, request: usize, budget: usize) {
        let slack = self
            .resampler
            .as_ref()
            .map_or(0, |resampler| resampler.max_output_frames(request));
        let out_channels = self.out_channels();
        self.buffers.reserve(request, budget + slack, out_channels);
    }

    /// Source frames to ask for to fill `wanted` output frames.
    fn source_frames(&self, wanted: usize) -> usize {
        match &self.resampler {
            Some(resampler) => resampler.source_frames_for(wanted).min(MAX_REQUEST_FRAMES),
            None => wanted,
        }
    }

    /// Channels the delivered samples carry; the widest a plugin may report until the
    /// first chunk says otherwise.
    fn out_channels(&self) -> usize {
        match self.format().channels as usize {
            0 => MAX_NATIVE_CHANNELS as usize,
            channels => channels,
        }
    }

    /// Asks the plugin for `request` frames, hinting at the format we would rather have.
    /// The hint is not binding: whatever comes back is normalized.
    fn pull(&mut self, request: usize) -> RVReadInfo {
        let hint = self.target.unwrap_or(DEFAULT_HINT);
        let data = RVReadData {
            channels_output: self.buffers.raw.as_mut_ptr().cast(),
            channels_output_max_bytes_size: advertised_bytes(request) as u32,
            info: RVReadInfo {
                format: RVAudioFormat {
                    audio_format: RVAudioStreamFormat::F32 as u32,
                    channel_count: hint.channels,
                    sample_rate: hint.sample_rate,
                },
                frame_count: request as u32,
                status: RVReadStatus::DecodingRequest as u32,
            },
        };
        // SAFETY: the buffer is live and writable for the capacity advertised, and the
        // instance belongs to this plugin.
        unsafe { (self.read_data)(self.instance.as_ptr(), data) }
    }

    /// The one checked boundary for what `read_data` returns.
    fn validate(
        &self,
        info: RVReadInfo,
        requested: usize,
    ) -> Result<(usize, FormatLock, bool), SessionError> {
        let finished = match RVReadStatus::from_raw(info.status) {
            Some(RVReadStatus::Ok) => false,
            Some(RVReadStatus::Finished) => true,
            Some(RVReadStatus::Error) => return Err(SessionError::Decode),
            Some(RVReadStatus::DecodingRequest) => return Err(AbiViolation::RequestStatus.into()),
            None => return Err(AbiViolation::UnknownStatus(info.status).into()),
        };
        let sample_format = RVAudioStreamFormat::from_raw(info.format.audio_format)
            .ok_or(AbiViolation::UnknownSampleFormat(info.format.audio_format))?;

        // Judged first, and against the bytes the plugin was actually handed: whether the
        // frame count fits the request is a narrower question than whether the audio it
        // claims fits the buffer.
        let needed = (info.frame_count as usize)
            .saturating_mul(info.format.channel_count as usize)
            .saturating_mul(convert::sample_width(sample_format));
        let available = advertised_bytes(requested);
        if needed > available {
            return Err(AbiViolation::BufferOverrun { needed, available }.into());
        }

        if info.frame_count as usize > requested {
            return Err(AbiViolation::FrameCount {
                returned: info.frame_count,
                requested: requested as u32,
            }
            .into());
        }
        if !matches!(info.format.channel_count, 1..=MAX_NATIVE_CHANNELS) {
            return Err(AbiViolation::ChannelCount(info.format.channel_count).into());
        }
        if info.format.sample_rate == 0 {
            return Err(AbiViolation::ZeroSampleRate.into());
        }

        let format = FormatLock {
            sample_format,
            sample_rate: info.format.sample_rate,
            channels: info.format.channel_count,
        };
        if let Some(expected) = self.lock {
            if format != expected {
                return Err(AbiViolation::FormatChanged {
                    expected,
                    found: format,
                }
                .into());
            }
        }

        Ok((info.frame_count as usize, format, finished))
    }

    /// Locks the format the first chunk reported and sets up the conversion it needs.
    fn start(&mut self, format: FormatLock) -> Result<(), SessionError> {
        if let Some(target) = self.target {
            if !convert::adaptation_supported(format.channels, target.channels) {
                return Err(SessionError::ChannelAdaptation {
                    from: format.channels,
                    to: target.channels,
                });
            }
            if format.sample_rate != target.sample_rate {
                self.resampler = Some(Resampler::new(
                    format.sample_rate,
                    target.sample_rate,
                    target.channels as usize,
                ));
            }
        }
        self.lock = Some(format);
        Ok(())
    }

    /// Normalizes, adapts and resamples one plugin chunk into the output buffer,
    /// returning the frames appended. Mirrors the C player's stage order.
    fn consume(&mut self, returned: usize, produced: usize) -> usize {
        let native = self
            .lock
            .expect("locked before the first chunk is consumed");
        let native_channels = native.channels as usize;
        let out_channels = self.out_channels();
        let samples = returned * native_channels;

        let Buffers {
            raw,
            decoded,
            adapted,
            out,
        } = &mut self.buffers;

        let width = convert::sample_width(native.sample_format);
        convert::to_f32(
            native.sample_format,
            &raw_bytes(raw)[..samples * width],
            &mut decoded[..samples],
        );

        let source: &[f32] = if native_channels == out_channels {
            &decoded[..samples]
        } else {
            let adapted = &mut adapted[..returned * out_channels];
            convert::adapt(&decoded[..samples], native_channels, adapted, out_channels);
            adapted
        };

        let destination = &mut out[produced * out_channels..];
        match &mut self.resampler {
            // The whole destination, so the run ends at source exhaustion rather than on
            // a full buffer, which would rebase the position past frames it never read.
            Some(resampler) => {
                let capacity = destination.len() / out_channels;
                resampler.process(source, returned, destination, capacity)
            }
            None => {
                destination[..returned * out_channels]
                    .copy_from_slice(&source[..returned * out_channels]);
                returned
            }
        }
    }
}

impl Drop for Player<'_> {
    fn drop(&mut self) {
        if self.opened {
            if let Some(close) = self.plugin.close {
                // SAFETY: the instance is live and was opened exactly once.
                unsafe { close(self.instance.as_ptr()) };
            }
        }
        if let Some(destroy) = self.plugin.destroy {
            // SAFETY: the instance came from this plugin's `create` and is destroyed once.
            unsafe { destroy(self.instance.as_ptr()) };
        }
    }
}

/// The buffers one read passes audio through. They grow to the largest budget seen and
/// stay there, so a steady stream allocates once.
#[derive(Default)]
struct Buffers {
    /// What the plugin writes into. `u32` slots give every sample type the alignment it
    /// may assume while holding exactly four bytes each.
    raw: Vec<u32>,
    decoded: Vec<f32>,
    adapted: Vec<f32>,
    out: Vec<f32>,
}

impl Buffers {
    fn reserve(&mut self, source_frames: usize, out_frames: usize, out_channels: usize) {
        let native = MAX_NATIVE_CHANNELS as usize;
        grow(&mut self.raw, source_frames * native);
        grow(&mut self.decoded, source_frames * native);
        grow(&mut self.adapted, source_frames * out_channels);
        grow(&mut self.out, out_frames * out_channels);
    }
}

fn grow<T: Copy + Default>(buffer: &mut Vec<T>, len: usize) {
    if buffer.len() < len {
        buffer.resize(len, T::default());
    }
}

/// Bytes the plugin is told it may write for `frames` frames: the widest it may answer
/// with. Both the promise and the check that holds the plugin to it read it from here.
fn advertised_bytes(frames: usize) -> usize {
    frames * MAX_NATIVE_CHANNELS as usize * MAX_SAMPLE_WIDTH
}

fn raw_bytes(raw: &[u32]) -> &[u8] {
    // SAFETY: `u32` holds no padding and no invalid bit patterns, and `u8` needs weaker
    // alignment, so the whole allocation reads as initialized bytes.
    unsafe { core::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), core::mem::size_of_val(raw)) }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod c_tests;
