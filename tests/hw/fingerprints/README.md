# tests/hw/fingerprints/

USB VID:PID fingerprint files consumed by the
[`hw-ci.yml`](../../../.github/workflows/hw-ci.yml) workflow's
"Detect attached hardware" step.

One file per board family — `<board>.txt`. One VID:PID per line, hex
without `0x`, lowercase or uppercase. Lines starting with `#` are
comments. The runner is considered to have the board attached when
**any** listed VID:PID is present in `fbuild serial probe list`.

See [`../README.md`](../README.md) for the broader fixture layout
and FastLED/fbuild#696 for the meta tracker.
