//! The log service: plugin diagnostics, formatted by a C shim.

use core::ffi::{c_char, c_int, c_void};

use crate::ffi::log::RVLogLevel;
use crate::plugin_string::plugin_str;
use crate::service::{context, guard};

/// Longest source-file name accepted with a log record, in bytes.
pub const LOG_FILE_LIMIT: usize = 1024;
/// Longest formatted log message accepted from the C shim, in bytes.
pub const LOG_MESSAGE_LIMIT: usize = 2047;

/// Severity a plugin logged at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LogLevel {
    /// A level this ABI version does not define arrives as [`LogLevel::Info`].
    fn from_raw(raw: u32) -> Self {
        match RVLogLevel::from_raw(raw) {
            Some(RVLogLevel::Trace) => Self::Trace,
            Some(RVLogLevel::Debug) => Self::Debug,
            Some(RVLogLevel::Warn) => Self::Warn,
            Some(RVLogLevel::Error) => Self::Error,
            Some(RVLogLevel::Fatal) => Self::Fatal,
            Some(RVLogLevel::Info) | None => Self::Info,
        }
    }
}

/// Where a plugin's diagnostics go.
///
/// `file` and `line` are filled only when the plugin used the `rvfl_*` macros.
pub trait Log: Send + Sync {
    fn log(&self, level: LogLevel, file: Option<&str>, line: u32, message: &str);
}

/// The default [`Log`]: forwards to the `log` crate under the `retrovert` target.
pub struct LogCrate;

impl Log for LogCrate {
    fn log(&self, level: LogLevel, file: Option<&str>, line: u32, message: &str) {
        let level = match level {
            LogLevel::Trace => ::log::Level::Trace,
            LogLevel::Debug => ::log::Level::Debug,
            LogLevel::Info => ::log::Level::Info,
            LogLevel::Warn => ::log::Level::Warn,
            LogLevel::Error | LogLevel::Fatal => ::log::Level::Error,
        };
        match file {
            Some(file) => ::log::log!(target: "retrovert", level, "{file}:{line}: {message}"),
            None => ::log::log!(target: "retrovert", level, "{message}"),
        }
    }
}

extern "C" {
    /// Formats the variadic arguments, then calls [`retrovert_host_log_write`].
    pub(super) fn retrovert_host_log_shim(
        private_data: *mut c_void,
        level: u32,
        file: *const c_char,
        line: c_int,
        fmt: *const c_char,
        ...
    );
}

/// Receives an already-formatted log line from the C shim.
///
/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`. `file` and
/// `text` must satisfy the bounded string contracts for
/// [`LOG_FILE_LIMIT`] and [`LOG_MESSAGE_LIMIT`].
#[no_mangle]
pub unsafe extern "C" fn retrovert_host_log_write(
    private_data: *mut c_void,
    level: u32,
    file: *const c_char,
    line: c_int,
    text: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller guarantees the context pointer and both strings.
        let (ctx, file, text) = unsafe {
            (
                context(private_data),
                plugin_str(file, LOG_FILE_LIMIT),
                plugin_str(text, LOG_MESSAGE_LIMIT),
            )
        };
        let (Ok(file), Ok(Some(text))) = (file, text) else {
            return;
        };
        ctx.log.log(
            LogLevel::from_raw(level),
            file.as_deref(),
            line.max(0) as u32,
            &text,
        );
    })
}
