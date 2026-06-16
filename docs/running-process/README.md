# docs/running-process

Documentation for fbuild's adoption of the
[running-process](https://github.com/zackees/running-process) v1 broker.

- [`inventory.md`](inventory.md) — the required wire & cache inventory (daemon
  model, request/response wire, cache roots, CI trust grouping, rollback path)
  and the encoding-lane decision that drives the broker migration. Recorded
  before any runtime behavior change, per the running-process fbuild adoption
  guide.

The implementation lives in the
[`broker`](../../crates/fbuild-daemon/src/broker) module of `fbuild-daemon`
(folded in from the former standalone `fbuild-broker` crate so fbuild stays close
to a monocrate — FastLED/fbuild#560). Tracker: zackees/running-process#437 ·
FastLED/fbuild#510.

## Current Runtime Slice

- `fbuild-daemon` binds the broker-provided
  `RUNNING_PROCESS_BROKER_V1_BACKEND_PIPE` endpoint when launched by
  running-process, answers broker identity probes, and serves broker-framed
  health/daemon-info diagnostics.
- CLI and PyO3 daemon acquisition prefer running-process negotiation when
  `RUNNING_PROCESS_DISABLE` is unset, then continue to use the existing HTTP
  endpoint for the operation surface.
- `RUNNING_PROCESS_DISABLE=1` and broker-unavailable cases keep the legacy
  direct HTTP spawn path.

Build/deploy/monitor requests, including streaming NDJSON builds, intentionally
remain on the HTTP path in this slice. Moving those long-running operation
payloads onto broker frames needs a separate streaming/cancellation design.
