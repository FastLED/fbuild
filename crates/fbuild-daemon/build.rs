//! Declare long-path support without elevating the daemon (#1673).

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
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../fbuild-core/src/platform/windows/long-paths.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bin=fbuild-daemon=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=fbuild-daemon=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
