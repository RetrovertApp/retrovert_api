//! Mirror of `retrovert/playback.h`.
//! Field semantics and ABI contracts are canonical in `include/retrovert/playback.h`.

#![allow(missing_docs)]

use core::ffi::{c_char, c_void};

use crate::ffi::audio_format::RVAudioFormat;
use crate::ffi::service::RVService;

pub const RV_PLAYBACK_PLUGIN_API_VERSION: u64 = 2;

abi_enum! {
    /// Result of probing a candidate file to decide whether a plugin can play it.
    RVProbeResult {
        Supported = 0,
        Unsupported = 1,
        Unsure = 2,
    }
}

abi_enum! {
    /// Status of a single `read_data` call.
    RVReadStatus {
        DecodingRequest = 0,
        Ok = 1,
        Finished = 2,
        Error = 3,
    }
}

abi_enum! {
    /// Whether a settings change applies live or needs the song reopened.
    RVSettingsUpdate {
        Default = 0,
        RequireRestart = 1,
    }
}

abi_enum! {
    /// How pattern channels advance through rows.
    RVScrollMode {
        Synchronized = 0,
        PerChannel = 1,
    }
}

abi_enum! {
    /// What a tracker pattern column holds.
    RVColumnKind {
        Note = 0,
        Instrument = 1,
        Volume = 2,
        Effect = 3,
        Param = 4,
        Custom = 5,
    }
}

/// Bits of [`RVVizInfo::caps`]. A bitset rather than a discriminant, so the header's
/// `RVVizCaps` enumerators mirror as constants.
pub struct RVVizCaps;

impl RVVizCaps {
    pub const PATTERN_CELLS: u32 = 1;
    pub const SCOPE: u32 = 2;
    pub const VU: u32 = 4;
    pub const WHOLE_SONG_KNOWN: u32 = 8;
    pub const SEEKABLE_PREVIEW: u32 = 16;
    pub const FUTURE_KNOWN: u32 = 32;
}

/// Format and outcome of a block of decoded audio.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVReadInfo {
    pub format: RVAudioFormat,
    pub frame_count: u32,
    pub status: u32,
}

/// Destination buffer and input request for a `read_data` call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVReadData {
    pub channels_output: *mut c_void,
    pub channels_output_max_bytes_size: u32,
    /// Caller-supplied format hint and maximum requested frame count; status is ignored.
    /// The plugin returns a separate [`RVReadInfo`] describing what it produced.
    pub info: RVReadInfo,
}

/// One-time description of a plugin's visualization surface. The counts here size the
/// buffers the host passes to the column, channel and cell queries.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVVizInfo {
    pub caps: u32,
    pub scroll_mode: u32,
    pub pattern_channel_count: u32,
    pub scope_channel_count: u32,
    pub column_count: u32,
}

/// Description of one tracker pattern column.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVColumnDesc {
    pub label: [u8; 16],
    pub char_width: u8,
    pub kind: u32,
}

/// Description of one pattern or scope channel.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVChannelDesc {
    pub name: [u8; 24],
    pub scope_width: u8,
}

/// Live musical position of the playhead.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVTrackerPosition {
    pub order: u32,
    pub pattern: u32,
    pub row: u32,
    pub window_lo: u32,
    pub window_hi: u32,
}

/// One tracker pattern cell: the plugin's raw value plus its own rendered text.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVPatternCell {
    pub raw: u32,
    pub text: [u8; 16],
}

/// The descriptor a playback plugin publishes. `user_data` comes from `create` and is
/// passed back to every per-instance call; the visualization slots may be null.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVPlaybackPlugin {
    pub api_version: u64,
    pub name: *const c_char,
    pub version: *const c_char,
    pub library_version: *const c_char,
    pub probe_can_play: Option<unsafe extern "C" fn(*mut u8, u64, *const c_char, u64) -> u32>,
    pub supported_extensions: Option<unsafe extern "C" fn() -> *const c_char>,
    pub create: Option<unsafe extern "C" fn(*const RVService) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub event: Option<unsafe extern "C" fn(*mut c_void, *mut u8, u64)>,
    pub open:
        Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32, *const RVService) -> i32>,
    pub close: Option<unsafe extern "C" fn(*mut c_void)>,
    pub read_data: Option<unsafe extern "C" fn(*mut c_void, RVReadData) -> RVReadInfo>,
    pub seek: Option<unsafe extern "C" fn(*mut c_void, i64) -> i64>,
    pub metadata: Option<unsafe extern "C" fn(*const c_char, *const RVService) -> i32>,
    pub static_init: Option<unsafe extern "C" fn(*const RVService)>,
    pub settings_updated: Option<unsafe extern "C" fn(*mut c_void, *const RVService) -> u32>,
    pub static_destroy: Option<unsafe extern "C" fn()>,
    pub viz_info: Option<unsafe extern "C" fn(*mut c_void, *mut RVVizInfo) -> bool>,
    pub tracker_columns: Option<unsafe extern "C" fn(*mut c_void, *mut RVColumnDesc, u32) -> u32>,
    pub tracker_channels: Option<unsafe extern "C" fn(*mut c_void, *mut RVChannelDesc, u32) -> u32>,
    pub scope_channels: Option<unsafe extern "C" fn(*mut c_void, *mut RVChannelDesc, u32) -> u32>,
    pub tracker_position: Option<unsafe extern "C" fn(*mut c_void, *mut RVTrackerPosition) -> bool>,
    pub tracker_channel_rows: Option<unsafe extern "C" fn(*mut c_void, *mut u32, u32) -> u32>,
    pub tracker_cells:
        Option<unsafe extern "C" fn(*mut c_void, i32, u32, u32, *mut RVPatternCell, u32) -> u32>,
    pub scope_enable: Option<unsafe extern "C" fn(*mut c_void, bool)>,
    pub scope_samples: Option<unsafe extern "C" fn(*mut c_void, i32, *mut f32, u32) -> u32>,
    pub vu_levels: Option<unsafe extern "C" fn(*mut c_void, *mut f32, u32) -> u32>,
}
