//! Mirror of `retrovert/resample.h`.

use core::ffi::{c_char, c_void};

use crate::ffi::audio_format::RVAudioFormat;
use crate::ffi::service::RVService;
use crate::ffi::settings::RVSettings;

abi_enum! {
    /// Whether a settings change applies live or needs the converter reconfigured.
    RVResampleSettingsUpdate {
        Default = 0,
        RequireRestart = 1,
    }
}

/// The conversion a resample plugin is configured for.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVConvertConfig {
    pub input: RVAudioFormat,
    pub output: RVAudioFormat,
}

/// The descriptor a resample plugin publishes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVResamplePlugin {
    pub api_version: u64,
    pub name: *const c_char,
    pub version: *const c_char,
    pub library_version: *const c_char,
    pub create: Option<unsafe extern "C" fn(*const RVService) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub set_config: Option<unsafe extern "C" fn(*mut c_void, *const RVConvertConfig)>,
    pub convert: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u32) -> u32>,
    pub get_expected_output_frame_count: Option<unsafe extern "C" fn(*mut c_void, u32) -> u32>,
    pub get_required_input_frame_count: Option<unsafe extern "C" fn(*mut c_void, u32) -> u32>,
    pub static_init: Option<unsafe extern "C" fn(*const RVService)>,
    pub settings_updated: Option<unsafe extern "C" fn(*mut c_void, *const RVSettings) -> u32>,
}
