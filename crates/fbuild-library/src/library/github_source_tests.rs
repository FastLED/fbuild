//! Tests for GitHub `lib_deps` sources (FastLED/fbuild#2018).

use super::*;
use crate::library::LibrarySpec;

const AUDIO: &str = "https://github.com/schreibfaul1/ESP32-audioI2S.git#3.0.12";

fn source(spec: &str) -> GitHubSource {
    GitHubSource::parse(spec).unwrap_or_else(|| panic!("{spec} should parse"))
}

#[test]
fn tag_ref_downloads_that_tags_archive() {
    let s = source(AUDIO);
    assert_eq!(s.owner, "schreibfaul1");
    assert_eq!(s.repo, "ESP32-audioI2S");
    assert_eq!(s.reference.as_deref(), Some("3.0.12"));
    assert_eq!(
        s.archive_url(),
        "https://github.com/schreibfaul1/ESP32-audioI2S/archive/3.0.12.zip"
    );
    assert_eq!(
        s.repo_url(),
        "https://github.com/schreibfaul1/ESP32-audioI2S"
    );
}

#[test]
fn every_url_form_resolves_to_the_same_repository() {
    for raw in [
        "https://github.com/O/R.git#v1",
        "https://github.com/O/R#v1",
        "git+https://github.com/O/R.git#v1",
        "https://github.com/O/R/#v1",
        "http://github.com/O/R.git#v1",
        "https://www.github.com/O/R#v1",
    ] {
        let s = source(raw);
        assert_eq!((s.owner.as_str(), s.repo.as_str()), ("O", "R"), "{raw}");
        assert_eq!(
            s.archive_url(),
            "https://github.com/O/R/archive/v1.zip",
            "{raw}"
        );
    }
}

#[test]
fn no_ref_uses_the_default_branch_not_a_guessed_name() {
    for raw in [
        "https://github.com/O/R",
        "https://github.com/O/R.git",
        "git+https://github.com/O/R.git",
    ] {
        let s = source(raw);
        assert_eq!(s.reference, None, "{raw}");
        assert_eq!(
            s.archive_url(),
            "https://github.com/O/R/archive/HEAD.zip",
            "{raw}"
        );
        assert_eq!(s.dir_suffix(), None, "{raw}");
    }
}

#[test]
fn branch_and_commit_refs_use_the_same_endpoint() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        source(&format!("https://github.com/O/R.git#{sha}")).archive_url(),
        format!("https://github.com/O/R/archive/{sha}.zip")
    );
    assert_eq!(
        source("https://github.com/O/R.git#feature/x").archive_url(),
        "https://github.com/O/R/archive/feature/x.zip"
    );
}

#[test]
fn unsafe_ref_characters_are_percent_encoded_in_the_url() {
    assert_eq!(
        source("https://github.com/O/R.git#v1%2").archive_url(),
        "https://github.com/O/R/archive/v1%252.zip"
    );
}

#[test]
fn dir_suffix_is_filesystem_safe_and_distinguishes_refs() {
    assert_eq!(
        source(AUDIO).dir_suffix().as_deref(),
        Some("3.0.12"),
        "a plain tag is used as-is"
    );
    let slash = source("https://github.com/O/R#a/b").dir_suffix().unwrap();
    let under = source("https://github.com/O/R#a_b").dir_suffix().unwrap();
    assert_ne!(slash, under);
    assert!(
        slash
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
        "{slash}"
    );
}

#[test]
fn rejects_non_github_archives_and_bad_refs() {
    for raw in [
        "https://gitlab.com/O/R.git#v1",
        "https://github.com/O/R/archive/v1.zip",
        "https://github.com/O/R/tree/main",
        "https://github.com/O/R.git#a/../b",
        "https://github.com/O/R.git#",
        "fastled/FastLED",
    ] {
        assert_eq!(GitHubSource::parse(raw), None, "{raw}");
    }
}

#[test]
fn library_spec_keeps_the_ref_out_of_name_and_install_dir() {
    let spec = LibrarySpec::parse(AUDIO).unwrap();
    assert_eq!(spec.owner, "schreibfaul1");
    assert_eq!(spec.name, "ESP32-audioI2S");
    assert_eq!(spec.sanitized_name(), "esp32-audioi2s");
    assert_eq!(spec.install_dir_name(), "esp32-audioi2s-3.0.12");
    assert_eq!(spec.to_string(), "schreibfaul1/ESP32-audioI2S#3.0.12");
    let github = spec.github.expect("GitHub source");
    assert_eq!(github.reference.as_deref(), Some("3.0.12"));

    let other =
        LibrarySpec::parse("https://github.com/schreibfaul1/ESP32-audioI2S.git#3.0.11").unwrap();
    assert_ne!(other.install_dir_name(), "esp32-audioi2s-3.0.12");

    let head = LibrarySpec::parse("https://github.com/schreibfaul1/ESP32-audioI2S").unwrap();
    assert_eq!(head.install_dir_name(), "esp32-audioi2s");
}

#[test]
fn library_spec_custom_name_form() {
    // PlatformIO's `<name>=<source>` sets the library name explicitly.
    let spec =
        LibrarySpec::parse("AudioI2S=https://github.com/schreibfaul1/ESP32-audioI2S.git#3.0.12")
            .unwrap();
    assert_eq!(spec.name, "AudioI2S");
    assert_eq!(spec.install_dir_name(), "audioi2s-3.0.12");
    assert_eq!(
        spec.github.unwrap().archive_url(),
        "https://github.com/schreibfaul1/ESP32-audioI2S/archive/3.0.12.zip"
    );
}

#[test]
fn html_body_is_rejected_with_a_clear_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("3.0.12.zip");
    std::fs::write(
        &path,
        b"\n\n<!DOCTYPE html>\n<html lang=\"en\"><head></head></html>",
    )
    .unwrap();
    let err = ensure_zip_archive(&path, "https://github.com/O/R/archive/3.0.12.zip")
        .unwrap_err()
        .to_string();
    assert!(err.contains("returned an HTML page"), "{err}");
    assert!(
        err.contains("https://github.com/O/R/archive/3.0.12.zip"),
        "{err}"
    );
    assert!(!err.contains("EOCD"), "{err}");
}

#[test]
fn zip_body_is_accepted() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("a.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    writer
        .start_file("R-1/x.h", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();
    ensure_zip_archive(&path, "u").unwrap();

    std::fs::write(&path, b"").unwrap();
    let err = ensure_zip_archive(&path, "u").unwrap_err().to_string();
    assert!(err.contains("an empty response"), "{err}");
}

#[tokio::test]
#[ignore = "network: downloads from github.com"]
async fn downloads_the_tagged_archive_from_github() {
    let tmp = tempfile::TempDir::new().unwrap();
    let spec = LibrarySpec::parse(AUDIO).unwrap();
    let dir = crate::library::library_downloader::download_library(&spec, tmp.path())
        .await
        .unwrap();
    assert!(dir.ends_with("esp32-audioi2s-3.0.12"));
    // Upstream never bumps its manifest version, so identify the tag by
    // content: Audio.cpp is 273879 bytes in the 3.0.12 archive.
    let audio = dir.join("src").join("src").join("Audio.cpp");
    assert_eq!(std::fs::metadata(&audio).unwrap().len(), 273_879);
    let meta = std::fs::read_to_string(dir.join("library.json")).unwrap();
    assert!(meta.contains("\"3.0.12\""), "{meta}");
}
