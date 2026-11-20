//! Build script for `fbuild`.
//!
//! On `*-pc-windows-msvc` we ask the linker to reserve an 8 MiB stack for the
//! `fbuild.exe` binary (default is 1 MiB). The clap-generated argument-parser
//! state machine has grown deep enough that debug builds overflow the default
//! Windows stack when rendering `--help` (FastLED/fbuild#242). Release builds
//! are unaffected; this only changes the PE header's reserved stack size.

#[path = "../fbuild-core/commit_stamp.rs"]
mod commit_stamp;

fn main() {
    // Report this checkout's commit in `--version` / `daemon status`
    // (FastLED/fbuild#1742). Stamped here, at the top of the crate graph, so
    // a new commit does not recompile the workspace below; `main` installs it
    // into `fbuild_core::build_id::STAMP`.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    commit_stamp::emit(std::path::Path::new(&manifest_dir));
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("pc-windows-msvc") {
        println!("cargo:rustc-link-arg-bin=fbuild=/STACK:8388608");
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../fbuild-core/src/platform/windows/long-paths.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bin=fbuild=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=fbuild=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
