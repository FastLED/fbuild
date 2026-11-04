//! Prefix-map flags for the ESP32 sketch/framework compile stages.
//!
//! Split out of `build.rs` to keep that file under the 1000-LOC gate.

use std::path::Path;

/// Path-remapping flags for a sketch-stage compile.
///
/// Every prefix-map spelling makes the object independent of where the
/// project lives, but a shared compiler cache only reuses an object across
/// project directories once the argv proves that the *file*, *macro* and
/// *debug* channels all cover the checkout root (zccache's
/// `cc_prefix_maps_cover_root`). With only `-fdebug-prefix-map`, the cache
/// entry stays scoped to one worktree, so a brand-new project directory
/// recompiles the sketch even though the object it would reuse is
/// byte-identical (FastLED/fbuild#1414).
///
/// `portable` is whether the toolchain accepts `-ffile-prefix-map` and
/// `-fmacro-prefix-map`: both arrived in GCC 8, so one capability line covers
/// the pair. Older toolchains (avr-gcc 7.3, the legacy ESP32 GCC 5) only have
/// `-fdebug-prefix-map` (GCC 4.3+) and keep the worktree-scoped behaviour.
pub(crate) fn sketch_prefix_map_args(
    portable: bool,
    project_dir: &Path,
    build_dir: &Path,
) -> Vec<String> {
    if portable {
        fbuild_paths::portable_prefix_map_args(project_dir, build_dir)
    } else {
        fbuild_paths::debug_prefix_map_args(project_dir, build_dir)
    }
}

#[cfg(test)]
#[path = "prefix_map_tests.rs"]
mod tests;
