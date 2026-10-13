//! Declare long-path support without elevating the daemon (#1673).

fn main() {
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
