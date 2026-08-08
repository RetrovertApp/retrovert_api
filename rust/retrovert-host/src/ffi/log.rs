//! Mirror of `retrovert/log.h`.

use core::ffi::{c_char, c_int, c_void};

pub const RV_LOG_API_VERSION: c_int = 1;

abi_enum! {
    /// Severity a log record is written at.
    RVLogLevel {
        Trace = 0,
        Debug = 1,
        Info = 2,
        Warn = 3,
        Error = 4,
        Fatal = 5,
    }
}

/// `(private_data, level, file, line, fmt, ...)`. Rust can call a C-variadic function
/// but cannot define one, so a host fills this slot from a C shim.
pub type RVLogFn = unsafe extern "C" fn(*mut c_void, u32, *const c_char, c_int, *const c_char, ...);

/// Logging the host exposes to plugins.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVLog {
    pub private_data: *mut c_void,
    pub log: Option<RVLogFn>,
}
