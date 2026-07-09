# fbuild-build-engine

Platform-agnostic build engine extracted from `fbuild-build` for compile
parallelism (FastLED/fbuild#1008).

Holds every shared engine module — `pipeline`, `compiler`, `compile_many`,
`source_scanner`, `linker`, `build_fingerprint`, `compile_database`,
`symbol_analyzer`, `shrink`, `framework_libs`, `framework_core_cache`,
`script_runtime`, `flag_overlay`, `build_info`, `build_output`,
`eh_frame_policy`, `zccache`/`zccache_embedded`, `arduino_props`,
`compile_backend`, `parallel`, `perf_log`, `package_override`, `resolution`,
`mcu_config` — plus the `PlatformSupport` / `BuildOrchestrator` trait
definitions the per-platform crates implement.

The engine never references a platform module (ENGINE→PLATFORM = 0), so the
per-platform crates (`fbuild-build-esp`, `-arm`, `-mcu`) compile in parallel on
top of it. The `fbuild-build` facade re-exports everything here at its original
paths, so consumers (`fbuild-cli`, `fbuild-daemon`, …) are unchanged.
