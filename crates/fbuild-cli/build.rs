//! Build script for `fbuild`.
//!
//! On `*-pc-windows-msvc` we ask the linker to reserve an 8 MiB stack for the
//! `fbuild.exe` binary (default is 1 MiB). The clap-generated argument-parser
//! state machine has grown deep enough that debug builds overflow the default
//! Windows stack when rendering `--help` (FastLED/fbuild#242). Release builds
//! are unaffected; this only changes the PE header's reserved stack size.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("pc-windows-msvc") {
        println!("cargo:rustc-link-arg-bin=fbuild=/STACK:8388608");
    }
}
