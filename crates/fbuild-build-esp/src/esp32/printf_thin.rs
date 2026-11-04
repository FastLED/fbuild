//! `--shrink=printf` reducer for ESP32/Xtensa + newlib (FastLED/fbuild#1459).
//!
//! Every newlib formatting entry point (`printf`, `fprintf`, `vprintf`,
//! `sprintf`, `snprintf`, `vsnprintf`, …) funnels through one of three
//! reentrant cores. On the Xtensa toolchain those cores live in `libc.a`
//! (`_vfprintf_r` 11.4 KB + `_svfprintf_r` 11.2 KB) and pull in
//! `_dtoa_r` (3.2 KB) for floating point. The vendor ships a second,
//! purpose-built copy of the same libc — newlib-nano — in the very same
//! toolchain: `libc_nano.a` plus `nano.specs`, which rewrites `-lc` to
//! `-lc_nano` (and `-lm` to `-lm_nano`) at link time.
//!
//! Linking with `-specs=nano.specs` therefore replaces the whole cluster
//! with newlib's own thin formatter instead of a hand-written printf:
//! no formatting semantics are invented here, and the substitution is the
//! one Espressif documents for size-constrained images.
//!
//! `-Wl,-u,_printf_float` keeps `%f`/`%e`/`%g` working (nano gates its
//! float formatter behind that symbol); without it nano prints the
//! conversion specifier literally. We always link it so a build that
//! shrinks still formats the same values.
//!
//! Divergences that remain are the vendor's, and they are measured rather than
//! assumed: the self-checking battery in
//! `crates/fbuild-build/tests/it/esp32_shrink_printf.rs` was flashed on an
//! ESP32-S3 (Xtensa LX7, xtensa-esp32s3-elf 8.4.0, espressif32 6.13.0) and
//! reported `BATTERY total=36 failed=0` unshrunk versus
//! `BATTERY total=36 failed=6` shrunk. Every failure is a length modifier
//! printed verbatim, because `libc_nano.a` exports `_printf_float` (so `%f`
//! survives) and has no equivalent for 64-bit integers: see
//! [`UNSUPPORTED_CONVERSIONS`]. Wide-character and long-double formatting are
//! absent for the same reason. A shrunk build therefore prints a `WARNING:`
//! naming those conversions, and `--shrink=auto` still never selects this mode.
//!
//! Availability is not the same as a win: RISC-V ESP32 targets ship a much
//! smaller full formatter, and linking nano there *grows* the image (measured
//! +9.5 KB on ESP32-C6). The caller gates on `McuConfig::is_xtensa()`.

use std::path::Path;

use fbuild_core::subprocess::run_command;
use fbuild_core::{Platform, Result};

use fbuild_build_engine::shrink::ShrinkMode;
use fbuild_build_engine::shrink::record::{ShrinkRecord, Strategy};

/// The GCC spec file that redirects `-lc`/`-lm` to newlib-nano.
pub const SPECS_FLAG: &str = "-specs=nano.specs";

/// Forces newlib-nano's floating-point formatter into the image.
///
/// Without it nano prints `%f` verbatim, so a shrunk build would silently
/// change program output. The float formatter is a few hundred bytes plus
/// `_dtoa_r`, i.e. still well under the full newlib cluster.
pub const FLOAT_SUPPORT_FLAG: &str = "-Wl,-u,_printf_float";

/// GCC query used to prove the spec file exists for the *selected* target.
pub const PROBE_ARG: &str = "-print-file-name=nano.specs";

/// Whether the reducer applies to this resolved mode + platform.
///
/// Only `--shrink=printf` opts in, and only on ESP32. The architecture gate
/// (`is_xtensa`) lives at the call site because it is a *measurement*, not a
/// platform property: see `shrink/README.md`.
#[must_use]
pub fn applies(mode: ShrinkMode, platform: Platform) -> bool {
    mode == ShrinkMode::Printf && platform == Platform::Espressif32
}

/// Link arguments that switch the image onto newlib-nano.
#[must_use]
pub fn link_flags() -> Vec<String> {
    vec![SPECS_FLAG.to_string(), FLOAT_SUPPORT_FLAG.to_string()]
}

/// Category recorded in `build_info_<env>.json` when the reducer fired.
pub const REDUCER_NAME: &str = "newlib-nano";

/// Conversions whose formatted **output** changes when the image links
/// newlib-nano (FastLED/fbuild#1459).
///
/// Measured, not assumed: the battery above was flashed on an ESP32-S3 and
/// `--shrink=printf` turned 6 of 36 identical cases into these five
/// modifiers printed verbatim (`%lld` of `-9223372036854775807` yields
/// `"ld"`, `%zu` of `sizeof(size_t)` yields `"zu"`). nano's formatter has no
/// 64-bit integer, `size_t` or `hh` support — `libc_nano.a` exports
/// `_printf_float`, which is why `%f` still works, and no symbol that would
/// restore the rest (there is no `_printf_long_long`).
///
/// This list is the single source of truth for the build-time warning, so a
/// change in measured behaviour has to change the disclosure with it.
pub const UNSUPPORTED_CONVERSIONS: &[&str] = &["%lld", "%llu", "%llx", "%zu", "%hh*"];

/// The always-visible line a shrunk build prints after claiming the reducer.
///
/// A reducer that silently changes what a sketch prints is worse than no
/// reducer, so the gap is disclosed in the build log, not only in docs. Kept
/// pure (no compiler, no I/O) so the exact text is testable.
#[must_use]
pub fn divergence_warning() -> String {
    format!(
        "WARNING: shrink: newlib-nano has no 64-bit integer, %zu or %hh formatting, so {} \
         print their length modifier instead of the value (measured on ESP32-S3, \
         FastLED/fbuild#1459); build with --no-shrink if the sketch formats those",
        UNSUPPORTED_CONVERSIONS.join(", ")
    )
}

/// The shrink decision written into `build_info_<env>.json`.
///
/// `mode` is the user-level request (already collapsed by
/// `shrink::resolve_explicit`); `resolved` is the mode that actually
/// reached the link. A toolchain without a nano variant builds unshrunk,
/// so `resolved` drops back to [`ShrinkMode::Off`] and `applied` stays
/// empty — the JSON then agrees with the terminal warning instead of
/// claiming a reduction that never happened.
///
/// `nano_applied` is the only input because the probe, not this module,
/// decides availability; keeping the mapping pure makes the kebab-case
/// JSON contract testable without spawning a compiler.
#[must_use]
pub fn record_for(mode: ShrinkMode, nano_applied: bool) -> ShrinkRecord {
    ShrinkRecord {
        mode,
        resolved: if nano_applied {
            ShrinkMode::Printf
        } else {
            ShrinkMode::Off
        },
        applied: if nano_applied {
            vec![REDUCER_NAME.to_string()]
        } else {
            Vec::new()
        },
        strategy: nano_applied.then_some(Strategy::SpecFile),
    }
}

/// Parse `gcc -print-file-name=nano.specs` output.
///
/// GCC echoes the query back verbatim when it cannot resolve the file, so
/// only an absolute path naming a `nano.specs` counts as a hit.
#[must_use]
pub fn nano_specs_from_probe(stdout: &str) -> Option<&str> {
    let path = stdout.trim();
    if path.is_empty() || !path.ends_with("nano.specs") {
        return None;
    }
    Path::new(path).is_absolute().then_some(path)
}

/// Ask `gcc` where its `nano.specs` is and confirm the file is really there.
///
/// Returns `false` for toolchains that carry no nano variant (or when the
/// compiler cannot be spawned) so the caller can build unshrunk instead of
/// failing the build on a link-time "cannot read spec file".
pub async fn probe_nano_specs(compiler: &Path, caller_path: Option<&str>) -> Result<bool> {
    let compiler_str = compiler.to_string_lossy().into_owned();
    let argv = [compiler_str.as_str(), PROBE_ARG];
    let env = caller_path.map(|path| vec![("PATH", path)]);
    let output = run_command(&argv, None, env.as_deref(), None).await?;
    if !output.success() {
        return Ok(false);
    }
    Ok(nano_specs_from_probe(&output.stdout).is_some_and(|path| Path::new(path).is_file()))
}

#[cfg(test)]
#[path = "printf_thin_tests.rs"]
mod tests;
