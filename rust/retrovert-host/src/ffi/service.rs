//! Mirror of `retrovert/service.h`.

use core::ffi::{c_int, c_void};

use crate::ffi::io::RVIo;
use crate::ffi::log::RVLog;
use crate::ffi::metadata::RVMetadata;
use crate::ffi::settings::RVSettings;

/// Service locator handed to a plugin at every entry point that can need one.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVService {
    pub private_data: *mut c_void,
    pub get_io: Option<unsafe extern "C" fn(*mut c_void, c_int) -> *const RVIo>,
    pub get_log: Option<unsafe extern "C" fn(*mut c_void, c_int) -> *const RVLog>,
    pub get_metadata: Option<unsafe extern "C" fn(*mut c_void, c_int) -> *const RVMetadata>,
    pub get_settings: Option<unsafe extern "C" fn(*mut c_void, c_int) -> *const RVSettings>,
}
