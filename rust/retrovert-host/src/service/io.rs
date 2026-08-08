//! The io service: url existence and whole-file loads.

use core::ffi::{c_char, c_void};
use core::ptr;
use std::path::Path;

use crate::ffi::io::RVIoReadUrlResult;
use crate::service::{context, guard, plugin_str};

/// File access a host lends to plugins.
///
/// A url is whatever the host's backends understand — a plain path, an entry inside an
/// archive, a remote address. Buffers are returned owned; the crate hands them across the
/// ABI and takes them back, so no implementor ever frees plugin memory.
pub trait Io: Send + Sync {
    fn exists(&self, url: &str) -> bool;

    /// `None` when the url cannot be read.
    fn read_to_memory(&self, url: &str) -> Option<Vec<u8>>;
}

/// The default [`Io`]: local files through `std::fs`.
pub struct StdFsIo;

impl Io for StdFsIo {
    fn exists(&self, url: &str) -> bool {
        Path::new(url).exists()
    }

    fn read_to_memory(&self, url: &str) -> Option<Vec<u8>> {
        std::fs::read(url).ok()
    }
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `url`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn exists(private_data: *mut c_void, url: *const c_char) -> bool {
    guard(false, || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, url) = unsafe { (context(private_data), plugin_str(url)) };
        match url {
            Some(url) => ctx.io.exists(&url),
            None => false,
        }
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `url`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn read_url_to_memory(
    private_data: *mut c_void,
    url: *const c_char,
) -> RVIoReadUrlResult {
    let empty = RVIoReadUrlResult {
        data: ptr::null_mut(),
        data_size: 0,
    };

    guard(empty, || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, url) = unsafe { (context(private_data), plugin_str(url)) };
        let Some(bytes) = url.and_then(|url| ctx.io.read_to_memory(&url)) else {
            return empty;
        };
        if bytes.is_empty() {
            return empty;
        }

        // Plugins reach the buffer through `free_url_to_memory`, but hosts have always
        // handed out malloc memory and a plugin that calls `free` itself gets away with it.
        // SAFETY: a fresh allocation of the right size, written before it is read.
        let data = unsafe { libc::malloc(bytes.len()).cast::<u8>() };
        if data.is_null() {
            return empty;
        }
        // SAFETY: `data` owns `bytes.len()` bytes and cannot overlap the source vector.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };

        RVIoReadUrlResult {
            data,
            data_size: bytes.len() as u64,
        }
    })
}

/// # Safety
///
/// `memory` must be null, or a buffer returned by [`read_url_to_memory`] that has not
/// already been freed.
pub(super) unsafe extern "C" fn free_url_to_memory(
    _private_data: *mut c_void,
    memory: *mut c_void,
) {
    guard((), || {
        if !memory.is_null() {
            // SAFETY: the caller guarantees a live allocation from `libc::malloc`.
            unsafe { libc::free(memory) };
        }
    })
}
