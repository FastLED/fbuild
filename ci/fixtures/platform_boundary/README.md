# Platform-boundary fixtures

`research_red_pass.rs` preserves phase-1 evidence for FastLED/fbuild#1307: the
current workspace has no host-platform boundary lint, so representative private,
inactive, native-import, compile-host-fact, and `cfg!` constructs compile on each
supported host. Phase 2 converts these constructs into negative Dylint fixtures.
