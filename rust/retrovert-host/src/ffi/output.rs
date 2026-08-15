//! Mirror of `retrovert/output.h`.
//! Field semantics and ABI contracts are canonical in `include/retrovert/output.h`.

#![allow(missing_docs)]

use core::ffi::{c_char, c_void};

use crate::ffi::audio_format::RVAudioFormat;
use crate::ffi::service::RVService;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVWriteInfo {
    pub sample_rate: u32,
    pub sample_count: u16,
    pub channel_count: u8,
    pub output_format: u8,
}

/// Pull callback an output plugin drives to fetch frames.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVPlaybackCallback {
    pub user_data: *mut c_void,
    pub callback: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, RVAudioFormat, u32) -> u32>,
}

/// The output devices a plugin can open, as a `char**` of `names_size` entries.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVOutputTargets {
    pub names: *mut c_void,
    pub names_size: u64,
}

/// The descriptor an output plugin publishes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVOutputPlugin {
    pub api_version: u64,
    pub name: *const c_char,
    pub version: *const c_char,
    pub library_version: *const c_char,
    pub create: Option<unsafe extern "C" fn(*const RVService) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub output_targets_info: Option<unsafe extern "C" fn(*mut c_void) -> RVOutputTargets>,
    pub start: Option<unsafe extern "C" fn(*mut c_void, *mut RVPlaybackCallback)>,
    pub stop: Option<unsafe extern "C" fn(*mut c_void)>,
    pub static_init: Option<unsafe extern "C" fn(*const RVService)>,
}
