# Test helper binaries

Auxiliary binaries used only by the fbuild-daemon integration test suite.
These are never shipped — they exist only so the test driver in
`tests/` can spawn real OS processes and exercise platform-level
behaviour (process containment, Job Objects, process groups) rather
than mocking it.

## Binaries

- `containment_harness.rs` — drives the process-containment integration
  test (FastLED/fbuild#32). Acts as a parent, child, or grandchild
  depending on the role argument passed on the command line; see the
  module-level doc-comment in the source.
