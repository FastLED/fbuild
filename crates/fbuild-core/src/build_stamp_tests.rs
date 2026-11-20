//! Tests for the `build.rs` stamp helpers (FastLED/fbuild#2042).

use std::path::Path;

use super::*;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A crates dir with configs, a board asset, Markdown and Rust source.
fn crates_tree(root: &Path) {
    write(&root.join("a/src/lib.rs"), "");
    write(&root.join("a/src/README.md"), "docs");
    write(
        &root.join("a/src/configs/avr.json"),
        r#"{"linker_flags":["-Wl,--gc-sections"]}"#,
    );
    write(&root.join("a/src/mixed/mod.rs"), "");
    write(&root.join("a/src/mixed/table.json"), "[]");
    write(&root.join("b/assets/boards/json/uno.json"), "{}");
    write(&root.join("b/data/fixture.bin"), "test only");
}

fn names(assets: &[Asset]) -> Vec<&str> {
    assets.iter().map(|asset| asset.name.as_str()).collect()
}

#[test]
fn embedded_assets_are_the_non_rust_files_under_src_and_assets() {
    let tmp = tempfile::TempDir::new().unwrap();
    crates_tree(tmp.path());
    let assets = embedded_assets(tmp.path());
    assert_eq!(
        names(&assets.files),
        [
            "a/src/configs/avr.json",
            "a/src/mixed/table.json",
            "b/assets/boards/json/uno.json"
        ]
    );
    // `src/mixed` holds Rust source, so watching it would rerun the build
    // script on every `.rs` edit; its asset is watched as a file instead.
    assert_eq!(names(&assets.asset_dirs), ["a/src/configs", "b/assets"]);
}

#[test]
fn the_assets_digest_follows_asset_bytes_and_names() {
    let tmp = tempfile::TempDir::new().unwrap();
    crates_tree(tmp.path());
    let digest = || assets_digest(&embedded_assets(tmp.path()).files);
    let base = digest();
    assert_eq!(base, digest(), "unchanged assets keep the digest");

    let config = tmp.path().join("a/src/configs/avr.json");
    write(&config, r#"{"linker_flags":["-Wl,--relax"]}"#);
    let edited = digest();
    assert_ne!(base, edited, "an edited config JSON must change the digest");

    std::fs::rename(&config, tmp.path().join("a/src/configs/avr2.json")).unwrap();
    assert_ne!(edited, digest(), "a renamed asset must change the digest");

    write(&tmp.path().join("a/src/lib.rs"), "fn edited() {}");
    write(&tmp.path().join("a/src/README.md"), "edited docs");
    let after_source_edit = digest();
    write(&tmp.path().join("a/src/lib.rs"), "");
    assert_eq!(
        after_source_edit,
        digest(),
        "Rust and Markdown are not assets"
    );
}
