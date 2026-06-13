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
