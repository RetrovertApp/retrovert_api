//! The io service: url existence and whole-file loads.

use core::ffi::{c_char, c_void};
use core::ptr;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::ffi::io::RVIoReadUrlResult;
use crate::plugin_string::plugin_str;
use crate::service::{context, guard};

/// Longest URL or path accepted from an I/O callback, in bytes.
pub const IO_URL_LIMIT: usize = 64 * 1024;
/// Largest whole-file buffer the I/O service returns to a plugin.
pub const IO_DATA_LIMIT: usize = 256 * 1024 * 1024;

/// File access a host lends to plugins.
///
/// A url is whatever the host's backends understand — a plain path, an entry inside an
/// archive, a remote address. Buffers are returned owned; the crate hands them across the
/// ABI and takes them back, so no implementor ever frees plugin memory.
pub trait Io: Send + Sync {
    /// Returns whether `url` can be read by this backend.
    fn exists(&self, url: &str) -> bool;

    /// `None` when the url cannot be read within `max_bytes`.
    ///
    /// Implementations must enforce `max_bytes` before allocating storage controlled by the
    /// resource size.
    fn read_to_memory(&self, url: &str, max_bytes: usize) -> Option<Vec<u8>>;
}

/// The default [`Io`]: local files through `std::fs`.
pub struct StdFsIo;

impl Io for StdFsIo {
    fn exists(&self, url: &str) -> bool {
        Path::new(url).exists()
    }

    fn read_to_memory(&self, url: &str, max_bytes: usize) -> Option<Vec<u8>> {
        let mut file = File::open(url).ok()?;
        let byte_count = usize::try_from(file.metadata().ok()?.len()).ok()?;
        if byte_count > max_bytes {
            return None;
        }

        let mut bytes = Vec::new();
        bytes.try_reserve_exact(byte_count).ok()?;
        bytes.resize(byte_count, 0);
        file.read_exact(&mut bytes).ok()?;

        let mut trailing = [0];
        if file.read(&mut trailing).ok()? != 0 {
            return None;
        }
        Some(bytes)
    }
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `url`
/// must satisfy the bounded string contract for [`IO_URL_LIMIT`].
pub(super) unsafe extern "C" fn exists(private_data: *mut c_void, url: *const c_char) -> bool {
    guard(false, || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, url) = unsafe { (context(private_data), plugin_str(url, IO_URL_LIMIT)) };
        match url {
            Ok(Some(url)) => ctx.io.exists(&url),
            Ok(None) | Err(_) => false,
        }
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `url`
/// must satisfy the bounded string contract for [`IO_URL_LIMIT`].
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
        let (ctx, url) = unsafe { (context(private_data), plugin_str(url, IO_URL_LIMIT)) };
        let Some(bytes) = url
            .ok()
            .flatten()
            .and_then(|url| ctx.io.read_to_memory(&url, IO_DATA_LIMIT))
        else {
            return empty;
        };
        if bytes.is_empty() || bytes.len() > IO_DATA_LIMIT {
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
