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
        true,
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
        true,
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

/// RISC-V targets are excluded by measurement, not by taste: on ESP32-C6 the
/// nano link grew `firmware.bin` by 9,520 B (FastLED/fbuild#1459).
#[tokio::test]
async fn riscv_targets_refuse_printf_and_say_why() {
    let mut flags = Vec::new();
    let mut log = BuildLog::new();
    let record = resolve_shrink(
        ShrinkMode::Printf,
        Path::new("/nonexistent/riscv32-esp-elf-gcc"),
        None,
        "toolchain-riscv32-esp",
        false,
        &mut flags,
        &mut log,
    )
    .await;
    assert_eq!(record.mode, ShrinkMode::Printf);
    assert_eq!(record.resolved, ShrinkMode::Off);
    assert!(record.applied.is_empty());
    assert!(flags.is_empty(), "no nano flag on RISC-V");
    let lines = log.into_lines();
    assert_eq!(lines.len(), 1, "exactly one warning: {lines:?}");
    assert!(lines[0].starts_with("WARNING: shrink: --shrink=printf is Xtensa-only"));
    assert!(lines[0].contains("toolchain-riscv32-esp"));
    assert!(lines[0].contains("grows the image"));
}

/// The gate must not fire for `auto`: a default build stays silent on RISC-V.
#[tokio::test]
async fn riscv_auto_stays_silent() {
    let mut flags = Vec::new();
    let mut log = BuildLog::new();
    let record = resolve_shrink(
        ShrinkMode::Auto,
        Path::new("/nonexistent/riscv32-esp-elf-gcc"),
        None,
        "toolchain-riscv32-esp",
        false,
        &mut flags,
        &mut log,
    )
    .await;
    assert_eq!(record.resolved, ShrinkMode::Off);
    assert!(
        log.is_empty(),
        "auto must not warn about an unrequested mode"
    );
}
