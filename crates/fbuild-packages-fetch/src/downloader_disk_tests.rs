//! Disk-space translation at the downloader boundary (FastLED/fbuild#1750 item 4).
//!
//! Split out of `downloader_tests.rs`, which is at the workspace's 1000-LOC limit.

use super::*;

#[test]
fn a_full_disk_while_writing_the_part_file_is_a_disk_error_not_a_download_error() {
    let error = DownloadAttemptError::PartFile {
        path: "/cache/part/pkg.tar.gz.part".to_string(),
        error: "No space left on device (os error 28)".to_string(),
    }
    .into_fbuild_error("https://example.invalid/pkg.tar.gz")
    .to_string();
    assert!(error.contains("insufficient disk space"), "{error}");
    assert!(error.contains("/cache/part/pkg.tar.gz.part"), "{error}");
    assert!(
        error.contains("FBUILD_MIN_DISK_BYTES=0"),
        "the way out must be named: {error}"
    );
}

#[test]
fn an_ordinary_part_file_failure_keeps_its_download_error_text() {
    let error = DownloadAttemptError::PartFile {
        path: "/cache/part/pkg.tar.gz.part".to_string(),
        error: "Permission denied (os error 13)".to_string(),
    }
    .into_fbuild_error("https://example.invalid/pkg.tar.gz")
    .to_string();
    assert!(
        error.contains("failed to write partial download"),
        "{error}"
    );
    assert!(!error.contains("insufficient disk space"), "{error}");
}
