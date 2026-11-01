use fbuild_build_engine::shrink::ShrinkMode;
use fbuild_core::BuildLog;

use super::*;

#[tokio::test]
async fn inapplicable_modes_never_probe_and_never_touch_link_flags() {
    // A bogus compiler path proves the probe is skipped entirely: if it
    // ran, `probe_nano_specs` would still return Ok(false), but the flags
    // and record would be indistinguishable — so also assert the record
    // keeps the user's mode.
    let mut flags = vec!["-DCORE_DEBUG_LEVEL=0".to_string()];
    let mut log = BuildLog::new();
    let record = resolve_shrink(
        ShrinkMode::Auto,
        Path::new("/nonexistent/xtensa-esp32-elf-gcc"),
        None,
        "toolchain-xtensa-esp32",
        &mut flags,
        &mut log,
    )
    .await;
    assert_eq!(record.mode, ShrinkMode::Auto);
    assert_eq!(record.resolved, ShrinkMode::Off);
    assert!(record.applied.is_empty());
    assert_eq!(flags, vec!["-DCORE_DEBUG_LEVEL=0".to_string()]);
    assert!(log.is_empty(), "a default build must stay silent");
}

#[tokio::test]
async fn missing_toolchain_builds_unshrunk_and_says_so_in_the_build_log() {
    let mut flags = Vec::new();
    let mut log = BuildLog::new();
    let record = resolve_shrink(
        ShrinkMode::Printf,
        Path::new("/nonexistent/xtensa-esp32-elf-gcc"),
        None,
        "toolchain-xtensa-esp32",
        &mut flags,
        &mut log,
    )
    .await;
    // Agrees with the JSON: nothing applied, resolved off.
    assert_eq!(record.mode, ShrinkMode::Printf);
    assert_eq!(record.resolved, ShrinkMode::Off);
    assert!(record.applied.is_empty());
    assert!(flags.is_empty(), "no spec flag without a probed nano.specs");
    let lines = log.into_lines();
    assert_eq!(lines.len(), 1, "expected exactly one warning: {lines:?}");
    assert!(lines[0].contains("found no nano.specs"));
    assert!(lines[0].contains("toolchain-xtensa-esp32"));
}
