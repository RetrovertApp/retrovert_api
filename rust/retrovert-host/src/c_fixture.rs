//! Builds throwaway C shared libraries for tests, against the real headers.

use std::path::{Path, PathBuf};

/// `include/` beside the crate, so fixtures build against the real headers.
fn include_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../include")
        .canonicalize()
        .expect("include/ not found beside the crate")
}

pub(crate) fn compile(dir: &Path, stem: &str, source: &str) -> PathBuf {
    let c_file = dir.join(format!("{stem}.c"));
    let object = dir.join(format!("{stem}.so"));
    std::fs::write(&c_file, source).expect("write fixture source");
    let status = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&object)
        .arg(&c_file)
        .arg(format!("-I{}", include_dir().display()))
        .status()
        .expect("run cc");
    assert!(status.success(), "compiling {stem}");
    object
}
