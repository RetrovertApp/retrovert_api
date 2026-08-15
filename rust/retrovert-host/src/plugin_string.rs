use core::ffi::c_char;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StringLimitExceeded;

pub(crate) enum BoundedBytes<'a> {
    Null,
    Bytes(&'a [u8]),
    LimitExceeded(&'a [u8]),
}

/// Reads a plugin string through at most `limit + 1` bytes.
///
/// The extra byte admits a value with exactly `limit` non-NUL bytes. A non-null
/// input without a terminator in that window is reported as over-limit.
///
/// # Safety
///
/// `value` must be null, NUL-terminated within `limit + 1` readable bytes, or
/// point to at least `limit + 1` readable bytes. Non-null memory must remain live
/// and unchanged for the returned borrow's lifetime.
pub(crate) unsafe fn bounded_bytes<'a>(value: *const c_char, limit: usize) -> BoundedBytes<'a> {
    if value.is_null() {
        return BoundedBytes::Null;
    }

    for length in 0..=limit {
        // SAFETY: the caller guarantees each byte through `limit` is readable unless an
        // earlier byte terminates the string.
        if unsafe { value.add(length).read() } == 0 {
            // SAFETY: the loop established that the preceding `length` bytes are readable.
            let bytes = unsafe { core::slice::from_raw_parts(value.cast::<u8>(), length) };
            return BoundedBytes::Bytes(bytes);
        }
    }

    // SAFETY: reaching this point established that the first `limit` bytes are readable.
    let prefix = unsafe { core::slice::from_raw_parts(value.cast::<u8>(), limit) };
    BoundedBytes::LimitExceeded(prefix)
}

/// Decodes a bounded plugin string. Invalid UTF-8 uses replacement characters.
///
/// # Safety
///
/// `value` must satisfy [`bounded_bytes`] for `limit`.
pub(crate) unsafe fn plugin_str<'a>(
    value: *const c_char,
    limit: usize,
) -> Result<Option<Cow<'a, str>>, StringLimitExceeded> {
    // SAFETY: the caller upholds the same pointer contract.
    match unsafe { bounded_bytes(value, limit) } {
        BoundedBytes::Null => Ok(None),
        BoundedBytes::Bytes(bytes) => Ok(Some(lossy_with_limit(bytes, limit))),
        BoundedBytes::LimitExceeded(_) => Err(StringLimitExceeded),
    }
}

pub(crate) fn lossy_with_limit(bytes: &[u8], limit: usize) -> Cow<'_, str> {
    if let Ok(text) = core::str::from_utf8(bytes) {
        return Cow::Borrowed(text);
    }

    let mut text = String::with_capacity(limit.min(bytes.len().saturating_mul(3)));
    let mut rest = bytes;
    while !rest.is_empty() && text.len() < limit {
        match core::str::from_utf8(rest) {
            Ok(valid) => {
                push_valid(&mut text, valid, limit);
                break;
            }
            Err(error) => {
                let valid =
                    core::str::from_utf8(&rest[..error.valid_up_to()]).expect("valid UTF-8 prefix");
                if push_valid(&mut text, valid, limit) < valid.len() {
                    break;
                }
                if text.len() + '\u{fffd}'.len_utf8() <= limit {
                    text.push('\u{fffd}');
                } else {
                    break;
                }
                let invalid = error
                    .error_len()
                    .unwrap_or(rest.len() - error.valid_up_to());
                rest = &rest[error.valid_up_to() + invalid..];
            }
        }
    }
    Cow::Owned(text)
}

fn push_valid(text: &mut String, valid: &str, limit: usize) -> usize {
    let mut length = valid.len().min(limit - text.len());
    while !valid.is_char_boundary(length) {
        length -= 1;
    }
    text.push_str(&valid[..length]);
    length
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{PLUGIN_LIBRARY_VERSION_LIMIT, PLUGIN_NAME_LIMIT, PLUGIN_VERSION_LIMIT};
    use crate::service::{
        IO_URL_LIMIT, LOG_FILE_LIMIT, LOG_MESSAGE_LIMIT, SETTING_DESCRIPTION_LIMIT,
        SETTING_EXTENSION_LIMIT, SETTING_ID_LIMIT, SETTING_LABEL_LIMIT, SETTING_REG_ID_LIMIT,
        SETTING_VALUE_LIMIT,
    };

    #[test]
    fn null_valid_at_limit_and_missing_terminator_are_distinct() {
        let at_limit = *b"abcd\0";
        let over_limit = *b"abcde\0";

        // SAFETY: both arrays are readable through the requested windows.
        unsafe {
            assert!(matches!(
                bounded_bytes(core::ptr::null(), 4),
                BoundedBytes::Null
            ));
            assert!(matches!(
                bounded_bytes(at_limit.as_ptr().cast(), 4),
                BoundedBytes::Bytes(b"abcd")
            ));
            assert!(matches!(
                bounded_bytes(over_limit.as_ptr().cast(), 4),
                BoundedBytes::LimitExceeded(b"abcd")
            ));
        }
    }

    #[test]
    fn invalid_utf8_remains_lossy() {
        let value = [b'a', 0xff, 0, 0, 0];

        // SAFETY: the array is terminated inside the five-byte window.
        let decoded = unsafe { plugin_str(value.as_ptr().cast(), 4) }
            .expect("within limit")
            .expect("non-null");
        assert_eq!(decoded, "a\u{fffd}");
    }

    #[test]
    fn lossy_output_never_exceeds_the_input_limit() {
        let value = [0xff, 0xff, 0xff, 0xff, 0];

        // SAFETY: the array terminates at the four-byte limit.
        let decoded = unsafe { plugin_str(value.as_ptr().cast(), 4) }
            .expect("within limit")
            .expect("non-null");
        assert_eq!(decoded, "\u{fffd}");
        assert!(decoded.len() <= 4);

        let value = [0xff, b'a', b'b', b'c', 0];
        // SAFETY: the array terminates at the four-byte limit.
        let decoded = unsafe { plugin_str(value.as_ptr().cast(), 4) }
            .expect("within limit")
            .expect("non-null");
        assert_eq!(decoded, "\u{fffd}a");
        assert!(decoded.len() <= 4);
    }

    #[test]
    fn every_rejecting_domain_accepts_its_limit_and_rejects_the_next_byte() {
        for limit in [
            PLUGIN_NAME_LIMIT,
            PLUGIN_VERSION_LIMIT,
            PLUGIN_LIBRARY_VERSION_LIMIT,
            SETTING_REG_ID_LIMIT,
            SETTING_ID_LIMIT,
            SETTING_LABEL_LIMIT,
            SETTING_DESCRIPTION_LIMIT,
            SETTING_VALUE_LIMIT,
            SETTING_EXTENSION_LIMIT,
            IO_URL_LIMIT,
            LOG_FILE_LIMIT,
            LOG_MESSAGE_LIMIT,
        ] {
            let mut value = vec![b'x'; limit + 2];
            value[limit] = 0;
            // SAFETY: the allocation covers every byte the decoder may inspect.
            assert!(unsafe { plugin_str(value.as_ptr().cast(), limit) }.is_ok());

            value[limit] = b'x';
            value[limit + 1] = 0;
            // SAFETY: the allocation covers every byte the decoder may inspect.
            let over_limit = unsafe { plugin_str(value.as_ptr().cast(), limit) };
            assert_eq!(over_limit, Err(StringLimitExceeded));

            value[limit + 1] = b'x';
            // SAFETY: the allocation covers every byte the decoder may inspect.
            let unterminated = unsafe { plugin_str(value.as_ptr().cast(), limit) };
            assert_eq!(unterminated, Err(StringLimitExceeded));
        }
    }
}
