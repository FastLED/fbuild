# operations

Submodules for the build, deploy, monitor, reset, and install-deps HTTP
handlers. Originally `handlers/operations.rs`; split here to keep every
`.rs` under the 1000-LOC CI gate.

- **`mod.rs`** -- Re-exports the public handlers and the `pub(crate)` items
  shared with `handlers::emulator`. External paths
  (`crate::handlers::operations::build`, etc.) are unchanged.
- **`common.rs`** -- `OperationGuard`, env-var feature switches
  (`trust_device_hash_enabled`, `native_verify_enabled`,
  `native_write_enabled`), `compute_esp32_image_hash`,
  `qemu_extra_build_flags`, deploy-route parsing, client-path resolution,
  and the artifact bundle exporter.
- **`build.rs`** -- `POST /api/build` handler (streaming + buffered paths).
- **`deploy.rs`** -- `POST /api/deploy` handler with the ESP32 trust-hash /
  verify-flash fast paths and AVR fallback.
- **`monitor.rs`** -- `POST /api/monitor` handler, `MonitorState`,
  `MonitorOutcome`, and the shared `run_monitor_loop`.
- **`reset.rs`** -- `POST /api/reset` handler (DTR/RTS toggling).
- **`install_deps.rs`** -- `POST /api/install-deps` handler.
- **`tests.rs`** -- Unit tests previously inlined at the bottom of
  `operations.rs` (deploy message formatting, espflash env switches,
  image-hash memo cache).
