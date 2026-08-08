//! Mirror of `retrovert/io.h`.

use core::ffi::{c_char, c_int, c_void};

pub const RV_IO_API_VERSION: c_int = 1;

/// Buffer handed back by [`RVIo::read_url_to_memory`], released with `free_url_to_memory`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVIoReadUrlResult {
    pub data: *mut u8,
    pub data_size: u64,
}

/// File I/O the host exposes to plugins.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVIo {
    pub private_data: *mut c_void,
    pub exists: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> bool>,
    pub read_url_to_memory:
        Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> RVIoReadUrlResult>,
    pub free_url_to_memory: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}
