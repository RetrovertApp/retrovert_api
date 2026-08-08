//! Host-side Rust bindings for the Retrovert plugin ABI, version 2.

#[cfg(all(test, unix))]
mod c_fixture;
pub mod ffi;
pub mod loader;
pub mod service;
pub mod session;
