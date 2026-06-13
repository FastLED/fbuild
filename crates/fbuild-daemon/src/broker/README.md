# fbuild-daemon broker module

v1 running-process broker adoption for fbuild, folded in from the former
standalone `fbuild-broker` crate (FastLED/fbuild#560 — fbuild stays as close to a
monocrate as possible).

## Modules

- `protocol.rs` — fbuild's registered payload-protocol pin
  (`register_payload_protocol! { FBUILD_PAYLOAD_PROTOCOL = 0x7EB1 }`) and the
  single internal `BrokerRequest`/`BrokerResponse` model shared by the legacy
  direct loopback-HTTP path (JSON) and the broker path (prost over the v1
  `Frame` envelope). Golden-message parity tests keep the two encodings in
  lock-step.
- `service.rs` — fbuild `ServiceDefinition` (`SHARED_BROKER` local /
  `EXPLICIT_INSTANCE "ci-trusted"` CI) and `CacheManifest` construction over the
  frozen builders. The seven cache roots come from
  [`fbuild_paths::running_process::CacheRoots`].
- `session.rs` — `FbuildBrokerSession::adopt` / `request` with typed
  `RefusalKind` handling and the `RUNNING_PROCESS_DISABLE=1` direct-path escape
  hatch.

## Where the shared bits live

The dependency-free pieces the CLI `daemon running-process` diagnostic also
prints — the cache roots and the display constants (`SERVICE_NAME`,
`CI_TRUSTED_INSTANCE`, `MIN_VERSION`, `FBUILD_PAYLOAD_PROTOCOL`,
`FBUILD_PROTOCOL_VERSION`) — live in `fbuild-paths::running_process` so the CLI
does not need to depend on `fbuild-daemon` or pull in `running-process`. The
authoritative compile-time payload-protocol pin stays here (the real broker
consumer); a drift test asserts it equals the `fbuild-paths` copy.
