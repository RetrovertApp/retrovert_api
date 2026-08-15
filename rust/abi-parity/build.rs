//! Generates bindings from the canonical C headers for layout parity tests.

use std::path::PathBuf;

/// Every header of the v2 ABI, in dependency order.
const HEADERS: &[&str] = &[
    "rv_types.h",
    "audio_format.h",
    "io.h",
    "log.h",
    "metadata.h",
    "settings.h",
    "service.h",
    "playback.h",
    "output.h",
    "resample.h",
];

fn main() {
    let include = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../include")
        .canonicalize()
        .expect("include/ not found beside the crate");

    let mut source = String::new();
    for header in HEADERS {
        let path = include.join("retrovert").join(header);
        assert!(path.is_file(), "missing header {}", path.display());
        println!("cargo:rerun-if-changed={}", path.display());
        source.push_str(&format!("#include <retrovert/{header}>\n"));
    }

    let bindings = bindgen::Builder::default()
        .header_contents("rv_abi.h", &source)
        .clang_arg(format!("-I{}", include.display()))
        .allowlist_type("RV.*")
        .allowlist_var("RV.*")
        .default_enum_style(bindgen::EnumVariation::Consts)
        // The C enumerators already carry their enum's name.
        .prepend_enum_name(false)
        .derive_debug(false)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed on the retrovert headers");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    bindings
        .write_to_file(out.join("rv_abi.rs"))
        .expect("writing generated bindings");
}
