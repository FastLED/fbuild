# fbuild-broker

v1 [running-process](https://github.com/zackees/running-process) broker
adoption for fbuild (zackees/running-process#437 / FastLED/fbuild#510).

This crate adopts the **frozen v1 broker API** (running-process #433) so fbuild
discovers and version-negotiates its daemon through the broker instead of
hard-coding a transport endpoint, while keeping the legacy direct
loopback-HTTP path behind the `RUNNING_PROCESS_DISABLE=1` escape hatch.

## Inventory + encoding lane

The current fbuild wire/cache layout is recorded in
[`docs/running-process/inventory.md`](../../docs/running-process/inventory.md).
`fbuild-daemon` speaks **JSON over loopback HTTP**, so per the adoption guide's
encoding decision table fbuild takes the **JSON lane**: keep the JSON direct
path, add a prost broker path, and assert golden-message parity for both
encodings.

## Modules

- **`protocol`** — pins fbuild's registered payload-protocol ID
  (`FBUILD_PAYLOAD_PROTOCOL = 0x7EB1`) and defines the single internal
  request/response model (`BrokerRequest` / `BrokerResponse`) used by **both**
  the direct and broker paths, plus the prost wire types (`FbuildRequest` /
  `FbuildResponse`). The prost envelope carries the operation discriminator plus
  the verbatim JSON body the daemon already parses, so the broker path is a pure
  framing change — not a schema fork.
- **`service`** — builds + installs the fbuild `ServiceDefinition`
  (`SHARED_BROKER` for per-user local builds, `EXPLICIT_INSTANCE "ci-trusted"`
  for CI trust groups) and publishes the `CacheManifest` recording the seven
  cache roots (artifact / index / temp / log / lock / runtime / config) resolved
  from `fbuild-paths`.
- **`session`** — adopts the async broker session
  (`AsyncBrokerSession::adopt`) with typed `Refused` handling (`RefusalKind`)
  and the `RUNNING_PROCESS_DISABLE=1` direct-path escape hatch.

## Rollback

`RUNNING_PROCESS_DISABLE=1` makes `FbuildBrokerSession::adopt` return
`AdoptOutcome::UseDirectPath`, so callers fall back to the existing
`DaemonClient` loopback-HTTP path without dialing the broker.

## Payload protocol registration

`FBUILD_PAYLOAD_PROTOCOL = 0x7EB1` is authoritatively registered in
running-process (`broker::protocol::registry`, zackees/running-process#440) and
re-pinned here with `register_payload_protocol!` so the two sides cannot drift.
