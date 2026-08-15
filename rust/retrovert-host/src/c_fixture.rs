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
    let object = dir.join(format!("{stem}.{}", std::env::consts::DLL_EXTENSION));
    std::fs::write(&c_file, source).expect("write fixture source");

    let rustc = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("query rustc host");
    let rustc = String::from_utf8(rustc.stdout).expect("rustc version is UTF-8");
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc host triple");
    let compiler = cc::Build::new()
        .cargo_metadata(false)
        .opt_level(0)
        .host(host)
        .target(host)
        .get_compiler();
    let mut command = compiler.to_command();
    if compiler.is_like_msvc() {
        command
            .current_dir(dir)
            .args(["/nologo", "/LD", "/std:c11"])
            .arg(format!("/I{}", include_dir().display()))
            .arg(c_file.file_name().expect("fixture source name"))
            .arg(format!(
                "/Fe{}",
                object
                    .file_name()
                    .expect("fixture library name")
                    .to_string_lossy()
            ))
            .arg("/link");
        for symbol in [
            "rv_playback_plugin",
            "rv_output_plugin",
            "rv_resample_plugin",
            "probe_calls",
            "fixture_inits",
        ] {
            if source.contains(symbol) {
                command.arg(format!("/EXPORT:{symbol}"));
            }
        }
    } else {
        command
            .args(["-shared", "-fPIC", "-o"])
            .arg(&object)
            .arg(&c_file)
            .arg(format!("-I{}", include_dir().display()));
    }
    let status = command.status().expect("run C compiler");
    assert!(status.success(), "compiling {stem}");
    object
}
