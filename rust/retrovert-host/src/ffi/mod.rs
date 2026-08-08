//! Raw `#[repr(C)]` mirror of the headers in `include/retrovert/`, one module per header.
//!
//! Enum-typed ABI fields and function-pointer results are carried as the raw `u32`
//! the C enum lowers to. A plugin can hand back a discriminant this ABI version does
//! not define, and holding that in a Rust enum is undefined behaviour; `from_raw`
//! does the checked conversion at the point the value is consumed.
//!
//! `rv_types.h` contributes no types — it is macros plus standard-header includes.

/// Declares a `#[repr(u32)]` mirror of a C enum together with its checked conversion.
macro_rules! abi_enum {
    (
        $(#[$attr:meta])*
        $name:ident { $($variant:ident = $value:expr),* $(,)? }
    ) => {
        $(#[$attr])*
        #[repr(u32)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant = $value),*
        }

        impl $name {
            /// Returns `None` for a discriminant this ABI version does not define.
            pub const fn from_raw(raw: u32) -> Option<Self> {
                match raw {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

pub mod audio_format;
pub mod io;
pub mod log;
pub mod metadata;
pub mod output;
pub mod playback;
pub mod resample;
pub mod service;
pub mod settings;
