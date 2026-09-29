# bench/fastled-examples/src

Sources for the repository's end-to-end benchmark binaries:

- `main.rs` implements `bench-fastled-examples`.
- `build_comparison.rs` implements the nightly Arduino CLI vs PlatformIO vs
  fbuild Blink build comparison and static-site renderer.
- `build_comparison_tests.rs` covers the comparison runner's cold-cache
  sequencing, output metadata, and renderer behavior.

See the parent [`README.md`](../README.md) for the FastLED harness and
[`../../blink/README.md`](../../blink/README.md) for the whole-build benchmark.
