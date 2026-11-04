//! Tests for [`crate::esp32::orchestrator::prefix_map`].

use std::path::Path;

use super::sketch_prefix_map_args;

/// A modern toolchain maps all three channels, so two project directories
/// normalize to one cache key (FastLED/fbuild#1414).
#[test]
fn portable_toolchains_map_every_channel() {
    let args = sketch_prefix_map_args(true, Path::new("/work/blink"), Path::new("/work/blink"));
    assert_eq!(
        args,
        vec![
            "-ffile-prefix-map=/work/blink=.".to_string(),
            "-fmacro-prefix-map=/work/blink=.".to_string(),
            "-fdebug-prefix-map=/work/blink=.".to_string(),
        ]
    );
}

/// A legacy toolchain (GCC 5/7) has no `-ffile-prefix-map`; emitting it would
/// fail the compile, so only the debug channel is mapped.
#[test]
fn legacy_toolchains_map_the_debug_channel_only() {
    let args = sketch_prefix_map_args(false, Path::new("/work/blink"), Path::new("/work/out"));
    assert_eq!(
        args,
        vec![
            "-fdebug-prefix-map=/work/blink=.".to_string(),
            "-fdebug-prefix-map=/work/out=fbuild-build".to_string(),
        ]
    );
}
