//! Builds the C shim used to forward variadic plugin log calls into Rust.

fn main() {
    println!("cargo:rerun-if-changed=log_shim.c");
    cc::Build::new()
        .file("log_shim.c")
        .compile("retrovert_host_log_shim");
}
