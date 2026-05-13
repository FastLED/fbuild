# fbuild-cli integration tests

Integration tests that spawn the `fbuild` binary (`CARGO_BIN_EXE_fbuild`)
and drive it against a stand-in daemon or environment so the CLI exit-code
and message contracts are covered end-to-end.

- **`test_emu_exit_code.rs`** -- regression for issue #130. Spawns a mock
  HTTP daemon on an ephemeral loopback port, points the CLI at it via
  `FBUILD_DEV_MODE=1` + `FBUILD_DAEMON_PORT`, and asserts the CLI exits
  non-zero when the daemon returns a structured failure response.
- **`ci_command.rs`** -- regression for FastLED/fbuild#242. Asserts the
  `fbuild ci` clap surface: `--help` documents the PlatformIO-compatible
  flags (`--board`, `--lib`, `--project-conf`, `--keep-build-dir`,
  `--build-dir`) and missing required args produce usage errors. No
  toolchain or daemon is invoked.
- **`lib_select.rs`** -- regression for FastLED/fbuild#202 / #204.
  Argument-parse + early-exit coverage for `fbuild lib-select` without a
  populated framework cache.
