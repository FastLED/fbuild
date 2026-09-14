# Integration Tests

Tests that download real toolchains and compile real sketches. Marked `#[ignore]` so they don't run during normal `uv run test`.

Run with: `soldr cargo test -p fbuild-build -- --ignored`

`esp32s3_size_parity.rs` also needs PlatformIO (`pio`, or `FBUILD_PARITY_PIO`). It asserts that fbuild's ESP32-S3 Blink `firmware.bin` is no larger than PlatformIO's build of the same pinned platform (FastLED/fbuild#1432), and runs daily via `.github/workflows/esp32s3-size-parity.yml`. Locally, stop any running fbuild daemon first (`fbuild daemon stop`): the test starts its own compile backend, which needs the zccache cache root a live daemon holds.
