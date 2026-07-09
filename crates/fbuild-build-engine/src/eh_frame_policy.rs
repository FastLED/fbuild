//! Decide whether to strip GCC's eh_frame unwinding tables from a build.
//!
//! See FastLED/fbuild#243 (ESP32) and FastLED/fbuild#244 (other GCC platforms).
//! GCC emits .eh_frame by default to support C++ exceptions and stack
//! unwinding. On embedded targets where neither exceptions nor panic
//! backtrace are used, this is dead metadata — ~225 KB on a stock FastLED
//! ESP32-S3 Blink. Stripping it requires compile-time cc1 flags; library-
//! header pragmas (FastLED's PR #2423) are unreliable on modern GCC.

use fbuild_config::sdkconfig::SdkConfigSummary;
use fbuild_core::BuildProfile;

/// What the build system should do with .eh_frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EhFramePolicy {
    /// Keep eh_frame (default; safe).
    #[default]
    Preserve,
    /// Strip eh_frame via `-fno-asynchronous-unwind-tables -fno-unwind-tables`.
    Strip,
}

/// Inputs to [`decide`]. All references are borrowed.
pub struct EhFrameInputs<'a> {
    /// User-supplied positive build flags (from platformio.ini `build_flags`,
    /// after debug-build overlay applied).
    pub build_flags: &'a [String],
    /// User-supplied negative build flags (from platformio.ini `build_unflags`).
    pub build_unflags: &'a [String],
    /// ESP32-only summary of sdkconfig keys we care about. None for other platforms.
    pub sdkconfig: Option<&'a SdkConfigSummary>,
    /// Build profile.
    pub profile: BuildProfile,
    /// True if `build_type = debug` was set in platformio.ini.
    pub is_debug_build: bool,
    /// `FBUILD_KEEP_EH_FRAME=1` env override.
    pub env_keep: bool,
    /// `FBUILD_STRIP_EH_FRAME=1` env override.
    pub env_strip: bool,
}

/// Append these flags to every TU's compile line when policy is [`EhFramePolicy::Strip`].
///
/// Both flags are necessary: GCC defaults to `-fasynchronous-unwind-tables`
/// (the actual emitter of .eh_frame on most non-debug builds), and
/// `-fno-unwind-tables` alone does not suppress the async variant.
pub const STRIP_FLAGS: [&str; 2] = ["-fno-asynchronous-unwind-tables", "-fno-unwind-tables"];

/// True iff `needle` appears in `flags` and is not negated by `unflags`.
///
/// The match is exact (not prefix) because real-world flags arrive as
/// discrete tokens, e.g. `["-O2", "-fexceptions", "-DFOO=1"]`.
fn flag_active(flags: &[String], unflags: &[String], needle: &str) -> bool {
    flags.iter().any(|f| f == needle) && !unflags.iter().any(|f| f == needle)
}

/// Decide the eh_frame policy for a build. Pure function over [`EhFrameInputs`].
///
/// Precedence (first matching rule wins):
///
/// 1. `FBUILD_STRIP_EH_FRAME=1` → Strip
/// 2. `FBUILD_KEEP_EH_FRAME=1` → Preserve
/// 3. `is_debug_build` → Preserve
/// 4. user requested unwinding via `-fexceptions`, `-funwind-tables`, or
///    `-fasynchronous-unwind-tables` in build_flags (and not negated in
///    build_unflags) → Preserve
/// 5. ESP32 sdkconfig has any of: `panic_print_backtrace`, `panic_gdbstub`,
///    `debug_ocdaware`, `optimization_debug` → Preserve
/// 6. otherwise → Strip
pub fn decide(inputs: &EhFrameInputs<'_>) -> EhFramePolicy {
    // 1. explicit strip override wins
    if inputs.env_strip {
        return EhFramePolicy::Strip;
    }
    // 2. explicit keep override
    if inputs.env_keep {
        return EhFramePolicy::Preserve;
    }
    // 3. debug build
    if inputs.is_debug_build {
        return EhFramePolicy::Preserve;
    }
    // 4. user requested unwinding via positive flags
    let unwind_flags = [
        "-fexceptions",
        "-funwind-tables",
        "-fasynchronous-unwind-tables",
    ];
    for needle in unwind_flags.iter() {
        if flag_active(inputs.build_flags, inputs.build_unflags, needle) {
            return EhFramePolicy::Preserve;
        }
    }
    // 5. sdkconfig hints (ESP32 only)
    if let Some(sdk) = inputs.sdkconfig {
        if sdk.panic_print_backtrace
            || sdk.panic_gdbstub
            || sdk.debug_ocdaware
            || sdk.optimization_debug
        {
            return EhFramePolicy::Preserve;
        }
    }
    // 6. default for release-mode embedded targets
    let _ = inputs.profile; // profile is currently informational only
    EhFramePolicy::Strip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_inputs<'a>() -> EhFrameInputs<'a> {
        EhFrameInputs {
            build_flags: &[],
            build_unflags: &[],
            sdkconfig: None,
            profile: BuildProfile::Release,
            is_debug_build: false,
            env_keep: false,
            env_strip: false,
        }
    }

    /// All-false sdkconfig for per-key isolation tests. `SdkConfigSummary::default()`
    /// returns the ESP-IDF Arduino default (`panic_print_backtrace = true`), which
    /// would otherwise short-circuit per-key Preserve assertions and break the
    /// "all keys false → Strip" assertion.
    fn empty_sdk() -> SdkConfigSummary {
        SdkConfigSummary {
            panic_print_backtrace: false,
            panic_gdbstub: false,
            debug_ocdaware: false,
            optimization_debug: false,
        }
    }

    #[test]
    fn baseline_strips() {
        let inputs = baseline_inputs();
        assert_eq!(decide(&inputs), EhFramePolicy::Strip);
    }

    #[test]
    fn env_strip_beats_everything() {
        let mut inputs = baseline_inputs();
        inputs.is_debug_build = true;
        inputs.env_keep = true;
        inputs.env_strip = true;
        assert_eq!(decide(&inputs), EhFramePolicy::Strip);
    }

    #[test]
    fn env_keep_preserves() {
        let mut inputs = baseline_inputs();
        inputs.env_keep = true;
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn debug_build_preserves() {
        let mut inputs = baseline_inputs();
        inputs.is_debug_build = true;
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn fexceptions_flag_preserves() {
        let flags = vec!["-fexceptions".to_string()];
        let mut inputs = baseline_inputs();
        inputs.build_flags = &flags;
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn fexceptions_negated_strips() {
        let flags = vec!["-fexceptions".to_string()];
        let unflags = vec!["-fexceptions".to_string()];
        let mut inputs = baseline_inputs();
        inputs.build_flags = &flags;
        inputs.build_unflags = &unflags;
        assert_eq!(decide(&inputs), EhFramePolicy::Strip);
    }

    #[test]
    fn funwind_tables_flag_preserves() {
        let flags = vec!["-funwind-tables".to_string()];
        let mut inputs = baseline_inputs();
        inputs.build_flags = &flags;
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn fasync_unwind_tables_flag_preserves() {
        let flags = vec!["-fasynchronous-unwind-tables".to_string()];
        let mut inputs = baseline_inputs();
        inputs.build_flags = &flags;
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn sdkconfig_panic_print_backtrace_preserves() {
        let sdk = SdkConfigSummary {
            panic_print_backtrace: true,
            ..empty_sdk()
        };
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn sdkconfig_panic_gdbstub_preserves() {
        let sdk = SdkConfigSummary {
            panic_gdbstub: true,
            ..empty_sdk()
        };
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn sdkconfig_debug_ocdaware_preserves() {
        let sdk = SdkConfigSummary {
            debug_ocdaware: true,
            ..empty_sdk()
        };
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn sdkconfig_optimization_debug_preserves() {
        let sdk = SdkConfigSummary {
            optimization_debug: true,
            ..empty_sdk()
        };
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn sdkconfig_all_false_strips() {
        let sdk = empty_sdk();
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Strip);
    }

    #[test]
    fn sdkconfig_arduino_default_preserves() {
        // ESP-IDF Arduino default has panic_print_backtrace=true, which is the
        // most common case in the wild. Must Preserve to match user expectation
        // of readable crash backtraces.
        let sdk = SdkConfigSummary::default();
        let mut inputs = baseline_inputs();
        inputs.sdkconfig = Some(&sdk);
        assert_eq!(decide(&inputs), EhFramePolicy::Preserve);
    }

    #[test]
    fn strip_flags_regression_guard() {
        assert_eq!(STRIP_FLAGS.len(), 2);
        assert!(STRIP_FLAGS.contains(&"-fno-asynchronous-unwind-tables"));
        assert!(STRIP_FLAGS.contains(&"-fno-unwind-tables"));
    }

    #[test]
    fn default_policy_is_preserve() {
        assert_eq!(EhFramePolicy::default(), EhFramePolicy::Preserve);
    }
}
