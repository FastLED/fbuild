//! Pilot: fetch a real per-chip ESP libs slice through a served manifest.
//!
//! Integration tests compile the library without `cfg(test)`, so the manifest
//! path is live. The manifest URL is served locally; the slices it names are
//! absolute URLs on the fbuild Spaces bucket. Run with the manifest server up:
//!
//!     FBUILD_MIRROR_MANIFEST_URL=http://127.0.0.1:8899/manifest.json \
//!         cargo test -p fbuild-packages-fetch --test slice_pilot -- --ignored

const VENDOR: &str =
    "https://github.com/espressif/arduino-esp32/releases/download/3.3.5/esp32-3.3.5-libs.tar.xz";
const SLICE_BYTES: u64 = 39_790_521;
const WHOLE_BYTES: u64 = 271_802_544;

#[tokio::test]
#[ignore = "network: downloads a real slice from the fbuild Spaces bucket"]
async fn pilot_fetches_the_chip_slice_not_the_whole_bundle() {
    let manifest = std::env::var("FBUILD_MIRROR_MANIFEST_URL").unwrap_or_default();
    assert!(
        !manifest.is_empty(),
        "set FBUILD_MIRROR_MANIFEST_URL to the served manifest"
    );
    let dir = tempfile::tempdir().unwrap();
    let request = fbuild_packages_fetch::FetchRequest {
        name: "framework-arduinoespressif32-libs",
        version: "3.3.5",
        url: VENDOR,
        variant: Some("ESP32-S3"),
    };
    let path = request.fetch(dir.path()).await.expect("slice fetch");
    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        size,
        SLICE_BYTES,
        "expected the esp32s3 slice ({SLICE_BYTES} B), got {size} B at {}",
        path.display()
    );
    assert_ne!(size, WHOLE_BYTES, "downloaded the whole bundle");
    assert!(
        path.to_string_lossy().contains("esp32s3"),
        "slice cache path is not chip-specific: {}",
        path.display()
    );
}

/// The real pioarduino 3.3.12 URL and the board's `build.mcu`, exactly as
/// `ensure_libs` passes them. Proves the production path selects the chip
/// slice for the version a current platform manifest resolves.
#[tokio::test]
#[ignore = "network: downloads a real slice from the fbuild Spaces bucket"]
async fn pilot_pioarduino_3_3_12_selects_the_chip_slice() {
    const VENDOR_3_3_12: &str = "https://github.com/espressif/arduino-esp32/releases/download/3.3.12/esp32-core-3.3.12-libs.tar.xz";
    const SLICE_3_3_12_S3: u64 = 72_335_469;
    let manifest = std::env::var("FBUILD_MIRROR_MANIFEST_URL").unwrap_or_default();
    assert!(!manifest.is_empty(), "set FBUILD_MIRROR_MANIFEST_URL");
    let dir = tempfile::tempdir().unwrap();
    let path = fbuild_packages_fetch::FetchRequest {
        name: "framework-arduinoespressif32-libs",
        version: "core-3.3.12",
        url: VENDOR_3_3_12,
        variant: Some("esp32s3"),
    }
    .fetch(dir.path())
    .await
    .expect("slice fetch");
    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        size, SLICE_3_3_12_S3,
        "expected the 3.3.12 esp32s3 slice, got {size}"
    );
    assert!(
        path.to_string_lossy().contains("+esp32s3"),
        "cache path is not chip-specific: {}",
        path.display()
    );
}
