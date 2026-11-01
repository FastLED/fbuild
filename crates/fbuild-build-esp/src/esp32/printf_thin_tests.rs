use super::*;

#[test]
fn only_explicit_printf_mode_on_esp32_applies() {
    assert!(applies(ShrinkMode::Printf, Platform::Espressif32));
    for mode in [
        ShrinkMode::Auto,
        ShrinkMode::Off,
        ShrinkMode::Safe,
        ShrinkMode::Aggressive,
    ] {
        assert!(
            !applies(mode, Platform::Espressif32),
            "{mode:?} must not shrink"
        );
    }
    assert!(!applies(ShrinkMode::Printf, Platform::AtmelAvr));
    assert!(!applies(ShrinkMode::Printf, Platform::Espressif8266));
}

#[test]
fn link_flags_select_newlib_nano_and_keep_float_formatting() {
    assert_eq!(link_flags(), vec![SPECS_FLAG, FLOAT_SUPPORT_FLAG]);
    // The spec file is what redirects -lc => -lc_nano; the -u pin is what
    // keeps `%f` from printing verbatim.
    assert_eq!(SPECS_FLAG, "-specs=nano.specs");
    assert!(FLOAT_SUPPORT_FLAG.ends_with("_printf_float"));
}

#[test]
fn probe_output_is_only_accepted_as_an_absolute_specs_path() {
    assert_eq!(
        nano_specs_from_probe("/toolchain/xtensa-esp-elf/lib/esp32s3/nano.specs\n"),
        Some("/toolchain/xtensa-esp-elf/lib/esp32s3/nano.specs")
    );
    // GCC echoes the query unchanged when the file is missing.
    assert_eq!(nano_specs_from_probe("nano.specs\n"), None);
    assert_eq!(nano_specs_from_probe(""), None);
    assert_eq!(nano_specs_from_probe("/toolchain/lib/other.specs\n"), None);
}

#[test]
fn record_reports_the_reducer_that_fired() {
    let record = record_for(ShrinkMode::Printf, true);
    assert_eq!(record.mode, ShrinkMode::Printf);
    assert_eq!(record.resolved, ShrinkMode::Printf);
    assert_eq!(record.applied, vec![REDUCER_NAME.to_string()]);
    assert_eq!(
        record.strategy,
        Some(fbuild_build_engine::shrink::record::Strategy::SpecFile)
    );
}

#[test]
fn record_degrades_to_off_when_the_toolchain_has_no_nano_variant() {
    let record = record_for(ShrinkMode::Printf, false);
    assert_eq!(record.mode, ShrinkMode::Printf);
    assert_eq!(record.resolved, ShrinkMode::Off);
    assert!(record.applied.is_empty());
    assert_eq!(record.strategy, None);
}

#[test]
fn non_printf_modes_are_recorded_as_not_applied() {
    for mode in [
        ShrinkMode::Auto,
        ShrinkMode::Off,
        ShrinkMode::Safe,
        ShrinkMode::Aggressive,
    ] {
        let record = record_for(mode, false);
        assert_eq!(record.mode, mode);
        assert_eq!(record.resolved, ShrinkMode::Off);
        assert!(record.applied.is_empty());
    }
}
