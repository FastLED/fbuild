# docs/running-process

Documentation for fbuild's adoption of the
[running-process](https://github.com/zackees/running-process) v1 broker.

- [`inventory.md`](inventory.md) — the required wire & cache inventory (daemon
  model, request/response wire, cache roots, CI trust grouping, rollback path)
  and the encoding-lane decision that drives the broker migration. Recorded
  before any runtime behavior change, per the running-process fbuild adoption
  guide.

The implementation lives in the [`fbuild-broker`](../../crates/fbuild-broker)
crate. Tracker: zackees/running-process#437 · FastLED/fbuild#510.
