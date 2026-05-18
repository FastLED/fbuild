# esp32_native

Native ESP32 `verify-flash` and `write-flash` implementations backed by
the `espflash` crate, compiled in only when the `espflash-native` cargo
feature is enabled. Alternative to the default
`crate::esp32::Esp32Deployer` path that shells out to Python `esptool`.

See the module docstring in [`mod.rs`](mod.rs) for the rationale (issue
#66), scope, serial-port lease behavior, and opt-in env vars.

## Layout

| File           | Responsibility                                          |
| -------------- | ------------------------------------------------------- |
| `mod.rs`       | Module docstring; re-exports the public API             |
| `types.rs`     | `NativeVerifyRegion`, `NativeWriteRegion`               |
| `verify.rs`    | `try_verify_deployment_native`, `collect_standard_regions` |
| `write.rs`     | `try_write_deployment_native`, write-region collectors  |
| `transport.rs` | Chip / reset-string parsing, MD5, port discovery, stdout renderer |
| `progress.rs`  | `LoggingProgressBridge` — espflash → `tracing` adapter  |
| `tests.rs`     | Unit tests for all pure helpers                         |

Split from a single 1042-LOC `esp32_native.rs` to satisfy the workspace
LOC gate (1000 LOC max per `.rs` file). Public surface is preserved
exactly — `crate::esp32_native::*` paths still resolve.
