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
    // "Absolute" is host-specific (GCC prints a drive-letter path on
    // Windows), so derive the fixture from an absolute host directory.
    let specs = std::env::temp_dir().join("esp32s3").join("nano.specs");
    let specs = specs.to_str().expect("utf-8 temp dir");
    assert_eq!(nano_specs_from_probe(&format!("{specs}\n")), Some(specs));
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

/// The measured gap is what a shrunk build discloses, so both halves are
/// pinned: the exact conversion set (a new divergence, or a fixed one, has to
/// edit this test) and the shape of the one-line warning that carries it.
#[test]
fn divergence_warning_names_every_measured_gap() {
    assert_eq!(
        UNSUPPORTED_CONVERSIONS,
        ["%lld", "%llu", "%llx", "%zu", "%hh*"],
        "the list is the source of truth for the build-time warning"
    );
    let warning = divergence_warning();
    for conversion in UNSUPPORTED_CONVERSIONS {
        assert!(
            warning.contains(conversion),
            "{conversion} missing from {warning}"
        );
    }
    assert!(
        warning.starts_with("WARNING: shrink: newlib-nano"),
        "must be greppable as a shrink warning: {warning}"
    );
    assert!(
        warning.contains("--no-shrink"),
        "the warning must name the escape hatch: {warning}"
    );
    assert!(
        warning.contains("FastLED/fbuild#1459"),
        "the warning must cite the measurement: {warning}"
    );
    assert!(
        !warning.contains('\n'),
        "build-log timestamps prefix one line per entry: {warning}"
    );
}
