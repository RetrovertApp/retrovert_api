//! The decode session: one plugin, one open song, one synchronous pull.
//!
//! Nothing here threads or paces: the caller names a frame budget and gets one validated
//! [`Chunk`] of f32 audio back. Decoding through a resample plugin is not wired up; the
//! loader can load the kind, and one can drop in later.

mod convert;
mod resample;

use core::ffi::c_void;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use std::ffi::CString;
use std::sync::Arc;

use crate::ffi::audio_format::{RVAudioFormat, RVAudioStreamFormat};
use crate::ffi::playback::{
    RVPlaybackPlugin, RVReadData, RVReadInfo, RVReadStatus, RVSettingsUpdate,
};
use crate::ffi::service::RVService;
use crate::loader::{LoadedPlugin, PluginKind, PluginSet};
use crate::service::{MetadataHandle, ServiceHost, TrackMetadata};
use crate::visualization::{self, VisualizationConfig, VisualizationError, VizLayout, VizSnapshot};
use resample::Resampler;

/// The most channels a target format can ask for, bounded by the resampler's state.
pub const MAX_TARGET_CHANNELS: u32 = resample::MAX_CHANNELS as u32;

/// The most channels a plugin may report. Both C hosts accept mono and stereo only.
const MAX_NATIVE_CHANNELS: u32 = 2;

/// Largest source-to-target rate ratio, covering 384 kHz decoded into an 8 kHz target.
const MAX_SAMPLE_RATE_RATIO: u32 = 48;

/// Widest sample the ABI defines, in bytes.
const MAX_SAMPLE_WIDTH: usize = 4;

/// Keeps the advertised buffer capacity inside the `u32` the ABI carries it in.
const MAX_REQUEST_FRAMES: usize =
    u32::MAX as usize / (MAX_NATIVE_CHANNELS as usize * MAX_SAMPLE_WIDTH);

/// Largest frame budget accepted by [`Player::prepare`].
pub const MAX_PREPARED_FRAMES: u32 = MAX_REQUEST_FRAMES as u32;

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
    #[error("sample-rate ratio between native {native} and target {target} exceeds {max_ratio}:1")]
    SampleRateRatio {
        native: u32,
        target: u32,
        max_ratio: u32,
    },
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
    #[error("metadata() failed with {0}")]
    MetadataFailed(i32),
    #[error("target format {} Hz / {} channels is not playable", .0.sample_rate, .0.channels)]
    InvalidTarget(StreamFormat),
    #[error("no built-in adaptation from {from} to {to} channels")]
    ChannelAdaptation { from: u32, to: u32 },
    #[error("read_data() reported an error")]
    Decode,
    #[error("could not allocate the {0} session buffer")]
    Allocation(&'static str),
    #[error("frame budget {requested} exceeds the maximum {maximum}")]
    InvalidFrameBudget { requested: u32, maximum: u32 },
    #[error("read budget {requested} exceeds the prepared budget {prepared}")]
    FrameBudgetExceeded { requested: u32, prepared: u32 },
    #[error("ABI violation: {0}")]
    Abi(#[from] AbiViolation),
}

/// A loaded playback plugin, brought up for as long as this lives.
///
/// Its exclusive [`LoadedPlugin`] borrow enforces one active `Plugin` per library, matching
/// the library-global `static_init` and `static_destroy` lifecycle.
///
/// ```compile_fail
/// use retrovert_host::loader::LoadedPlugin;
/// use retrovert_host::service::ServiceHost;
/// use retrovert_host::session::Plugin;
///
/// fn overlapping(loaded: &mut LoadedPlugin, host: &ServiceHost) {
///     let first = Plugin::new(loaded, host).unwrap();
///     let second = Plugin::new(loaded, host).unwrap();
///     drop((first, second));
/// }
/// ```
pub struct Plugin<'a> {
    name: &'a str,
    plugin: &'a RVPlaybackPlugin,
    service: &'a RVService,
    metadata: MetadataHandle,
}

impl<'a> Plugin<'a> {
    pub fn new(loaded: &'a mut LoadedPlugin, host: &'a ServiceHost) -> Result<Self, SessionError> {
        let name = loaded.name();
        let plugin = loaded.playback().ok_or(SessionError::NotPlayback)?;
        Ok(Self::from_descriptor(name, plugin, host))
    }

    fn from_descriptor(name: &'a str, plugin: &'a RVPlaybackPlugin, host: &'a ServiceHost) -> Self {
        let service = host.service();
        if let Some(static_init) = plugin.static_init {
            // SAFETY: the plugin published this callback, and the service locator stays
            // live for as long as the host that owns it.
            unsafe { static_init(service) };
        }
        Self {
            name,
            plugin,
            service,
            metadata: host.metadata(),
        }
    }

    pub fn name(&self) -> &str {
        self.name
    }

    /// Opens a song, leaving conversion to the caller: chunks arrive at the plugin's own
    /// rate and channel count.
    pub fn open(&self, url: &str, subsong: u32) -> Result<Player<'_>, SessionError> {
        self.open_inner(url, subsong, None)
    }

    /// Runs the plugin's static metadata pass. A plugin without one reports no record.
    pub fn metadata(&self, url: &str) -> Result<Option<TrackMetadata>, SessionError> {
        let Some(metadata) = self.plugin.metadata else {
            return Ok(None);
        };
        let url = CString::new(url).map_err(|_| SessionError::InvalidUrl)?;
        self.metadata.discard();

        // SAFETY: the URL is live and NUL-terminated for the call, and the service
        // locator stays live for this plugin's whole lifetime.
        let result = unsafe { metadata(url.as_ptr(), self.service) };
        if result != 0 {
            self.metadata.discard();
            return Err(SessionError::MetadataFailed(result));
        }
        Ok(Some(self.metadata.take()))
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
            pipeline: None,
            resampler: None,
            finished: false,
            carry: 0,
            carry_at: 0,
            buffers: Buffers::default(),
            visualization: VisualizationState::Unprepared,
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

/// Playback plugins initialized against one host and kept live through selection and use.
pub struct PlaybackPlugins<'a> {
    plugins: Vec<Plugin<'a>>,
}

impl<'a> PlaybackPlugins<'a> {
    pub fn new(loaded: &'a mut PluginSet, host: &'a ServiceHost) -> Result<Self, SessionError> {
        let plugins = loaded
            .of_kind_mut(PluginKind::Playback)
            .map(|loaded| Plugin::new(loaded, host))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { plugins })
    }

    /// The first supported plugin wins; the first unsure plugin is the fallback.
    pub fn select(&self, data: &[u8], filename: &str, total_size: u64) -> Option<&Plugin<'a>> {
        crate::loader::select_playback_candidate(
            &self.plugins,
            |plugin| Some(plugin.plugin),
            data,
            filename,
            total_size,
        )
    }

    #[cfg(test)]
    fn from_descriptors(
        descriptors: &'a [(&'a str, &'a RVPlaybackPlugin)],
        host: &'a ServiceHost,
    ) -> Self {
        Self {
            plugins: descriptors
                .iter()
                .map(|(name, descriptor)| Plugin::from_descriptor(name, descriptor, host))
                .collect(),
        }
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
    pipeline: Option<Pipeline>,
    resampler: Option<Resampler>,
    finished: bool,
    /// Frames the last read produced past its budget, waiting at `carry_at` in `out`.
    carry: usize,
    carry_at: usize,
    buffers: Buffers,
    visualization: VisualizationState,
}

enum VisualizationState {
    Unprepared,
    Absent(VisualizationConfig),
    Ready(Arc<VizLayout>),
}

#[derive(Clone, Copy)]
struct Pipeline {
    convert: bool,
    adapt: bool,
    resample: bool,
}

/// One open song with all host storage reserved for a fixed real-time frame budget.
///
/// Construction through [`Player::prepare`] is the allocation seam. Calls to [`Self::read`]
/// at or below [`Self::max_frames`] do not allocate in host code, including the first call.
pub struct PreparedPlayer<'a> {
    player: Player<'a>,
    max_frames: u32,
}

impl<'a> Deref for PreparedPlayer<'a> {
    type Target = Player<'a>;

    fn deref(&self) -> &Self::Target {
        &self.player
    }
}

impl DerefMut for PreparedPlayer<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.player
    }
}

impl PreparedPlayer<'_> {
    pub const fn max_frames(&self) -> u32 {
        self.max_frames
    }

    /// Pulls up to `frames` frames without allocating host storage.
    pub fn read(&mut self, frames: u32) -> Result<Chunk<'_>, SessionError> {
        if frames > self.max_frames {
            return Err(SessionError::FrameBudgetExceeded {
                requested: frames,
                prepared: self.max_frames,
            });
        }
        self.player.read_prepared(frames as usize)
    }
}

impl<'a> Player<'a> {
    /// Reserves every host buffer needed by reads up to `max_frames` and consumes the
    /// unprepared player so the real-time interface cannot be entered accidentally.
    pub fn prepare(mut self, max_frames: u32) -> Result<PreparedPlayer<'a>, SessionError> {
        if max_frames > MAX_PREPARED_FRAMES {
            return Err(SessionError::InvalidFrameBudget {
                requested: max_frames,
                maximum: MAX_PREPARED_FRAMES,
            });
        }
        self.buffers.prepare(max_frames as usize, self.target)?;
        Ok(PreparedPlayer {
            player: self,
            max_frames,
        })
    }

    /// Queries immutable visualization structure and allocates no per-frame storage.
    ///
    /// Repeating this with the same configuration returns the cached layout without
    /// calling the plugin again. Frame slots are allocated explicitly through
    /// [`VizLayout::new_snapshot`] before entering the capture loop.
    pub fn prepare_visualization(
        &mut self,
        config: VisualizationConfig,
    ) -> Result<Option<Arc<VizLayout>>, VisualizationError> {
        match &self.visualization {
            VisualizationState::Ready(layout) if layout.config() == config => {
                return Ok(Some(Arc::clone(layout)));
            }
            VisualizationState::Absent(prepared) if *prepared == config => return Ok(None),
            VisualizationState::Ready(_) | VisualizationState::Absent(_) => {
                return Err(VisualizationError::ConfigurationChanged);
            }
            VisualizationState::Unprepared => {}
        }

        let layout = visualization::prepare_layout(self.plugin, self.instance.as_ptr(), config)?;
        self.visualization = match &layout {
            Some(layout) => VisualizationState::Ready(Arc::clone(layout)),
            None => VisualizationState::Absent(config),
        };
        Ok(layout)
    }

    /// Refreshes one preallocated frame slot without allocating.
    pub fn capture_visualization(
        &mut self,
        output_frame: u64,
        snapshot: &mut VizSnapshot,
    ) -> Result<(), VisualizationError> {
        let VisualizationState::Ready(layout) = &self.visualization else {
            return Err(VisualizationError::NotPrepared);
        };
        visualization::capture_snapshot(
            self.plugin,
            self.instance.as_ptr(),
            layout,
            output_frame,
            snapshot,
        )
    }

    /// Enables or disables the plugin's scope capture when it advertises that callback.
    pub fn set_scope_enabled(&mut self, enabled: bool) {
        if let Some(scope_enable) = self.plugin.scope_enable {
            // SAFETY: the instance belongs to this plugin and the call is serialized with reads.
            unsafe { scope_enable(self.instance.as_ptr(), enabled) };
        }
    }

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
    fn read_prepared(&mut self, budget: usize) -> Result<Chunk<'_>, SessionError> {
        let mut produced = self.take_carry();

        while produced < budget && !self.finished {
            let request = self.source_frames(budget - produced);

            let info = self.pull(request);
            let (returned, format, finished) = self.validate(info, request)?;
            if self.lock.is_none() {
                self.start(format)?;
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
        let mut adapt = false;
        let mut resample = false;
        if let Some(target) = self.target {
            let native_rate = u64::from(format.sample_rate);
            let target_rate = u64::from(target.sample_rate);
            let ratio = u64::from(MAX_SAMPLE_RATE_RATIO);
            if native_rate > target_rate * ratio || target_rate > native_rate * ratio {
                return Err(AbiViolation::SampleRateRatio {
                    native: format.sample_rate,
                    target: target.sample_rate,
                    max_ratio: MAX_SAMPLE_RATE_RATIO,
                }
                .into());
            }
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
                resample = true;
            }
            adapt = format.channels != target.channels;
        }
        self.pipeline = Some(Pipeline {
            convert: format.sample_format != RVAudioStreamFormat::F32,
            adapt,
            resample,
        });
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

        let pipeline = self.pipeline.expect("selected with the format lock");
        let Buffers {
            raw,
            scratch,
            decoded_capacity,
            out,
        } = &mut self.buffers;
        let (decoded, adapted) = scratch.split_at_mut(*decoded_capacity);
        let destination = &mut out[produced * out_channels..];

        if !pipeline.resample {
            if pipeline.adapt {
                let source = if pipeline.convert {
                    let width = convert::sample_width(native.sample_format);
                    convert::to_f32(
                        native.sample_format,
                        &raw_bytes(raw)[..samples * width],
                        &mut decoded[..samples],
                    );
                    &decoded[..samples]
                } else {
                    &raw_f32(raw)[..samples]
                };
                convert::adapt(
                    source,
                    native_channels,
                    &mut destination[..returned * out_channels],
                    out_channels,
                );
            } else if pipeline.convert {
                let width = convert::sample_width(native.sample_format);
                convert::to_f32(
                    native.sample_format,
                    &raw_bytes(raw)[..samples * width],
                    &mut destination[..samples],
                );
            } else {
                destination[..samples].copy_from_slice(&raw_f32(raw)[..samples]);
            }
            return returned;
        }

        let normalized = if pipeline.convert {
            let width = convert::sample_width(native.sample_format);
            convert::to_f32(
                native.sample_format,
                &raw_bytes(raw)[..samples * width],
                &mut decoded[..samples],
            );
            &decoded[..samples]
        } else {
            &raw_f32(raw)[..samples]
        };
        let source = if pipeline.adapt {
            let adapted = &mut adapted[..returned * out_channels];
            convert::adapt(normalized, native_channels, adapted, out_channels);
            &*adapted
        } else {
            normalized
        };
        let capacity = destination.len() / out_channels;
        self.resampler
            .as_mut()
            .expect("selected resampling pipeline")
            .process(source, returned, destination, capacity)
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

/// Storage reserved once at the preparation seam.
#[derive(Default)]
struct Buffers {
    /// What the plugin writes into. `u32` slots give every sample type the alignment it
    /// may assume while holding exactly four bytes each.
    raw: Vec<u32>,
    /// One allocation partitioned into normalized and channel-adapted work areas.
    scratch: Vec<f32>,
    decoded_capacity: usize,
    out: Vec<f32>,
}

impl Buffers {
    fn prepare(
        &mut self,
        max_frames: usize,
        target: Option<StreamFormat>,
    ) -> Result<(), SessionError> {
        let native = MAX_NATIVE_CHANNELS as usize;
        let ratio = MAX_SAMPLE_RATE_RATIO as usize;
        let source_frames = if target.is_some() {
            checked_len(max_frames, ratio, 2, "raw")?.min(MAX_REQUEST_FRAMES)
        } else {
            max_frames
        };
        let raw_samples = checked_len(source_frames, native, 0, "raw")?;
        // Native delivery converts directly into `out`. A target may require both
        // normalization and adaptation before resampling, but the ABI reveals that only
        // on the first read, so its scratch storage is reserved conservatively.
        let decoded_samples = target.map_or(0, |_| raw_samples);
        let adapted_samples = match target {
            Some(target) => checked_len(source_frames, target.channels as usize, 0, "adapted")?,
            None => 0,
        };
        let scratch_samples = decoded_samples
            .checked_add(adapted_samples)
            .ok_or(SessionError::Allocation("scratch"))?;
        // The unlocked first pull can expand by the full ratio. Later pulls include two
        // source frames of resampler slack, each of which can also expand by the ratio.
        let output_frames = if target.is_some() {
            checked_len(max_frames, ratio + 1, ratio * 2 + 1, "output")?
        } else {
            max_frames
        };
        let out_channels = target.map_or(native, |target| target.channels as usize);
        let output_samples = checked_len(output_frames, out_channels, 0, "output")?;

        grow(&mut self.raw, raw_samples, "raw")?;
        grow(&mut self.scratch, scratch_samples, "scratch")?;
        self.decoded_capacity = decoded_samples;
        grow(&mut self.out, output_samples, "output")
    }
}

fn checked_len(
    count: usize,
    multiplier: usize,
    extra: usize,
    name: &'static str,
) -> Result<usize, SessionError> {
    count
        .checked_mul(multiplier)
        .and_then(|length| length.checked_add(extra))
        .ok_or(SessionError::Allocation(name))
}

fn grow<T: Copy + Default>(
    buffer: &mut Vec<T>,
    len: usize,
    name: &'static str,
) -> Result<(), SessionError> {
    if buffer.len() < len {
        buffer
            .try_reserve_exact(len - buffer.len())
            .map_err(|_| SessionError::Allocation(name))?;
        buffer.resize(len, T::default());
    }
    Ok(())
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

fn raw_f32(raw: &[u32]) -> &[f32] {
    // SAFETY: `u32` and `f32` have identical size and alignment, every bit pattern is a
    // valid `f32`, and the plugin initialized the prefix read by the caller.
    unsafe { core::slice::from_raw_parts(raw.as_ptr().cast::<f32>(), raw.len()) }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod c_tests;
