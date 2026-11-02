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

## Library modules

- `entry/mod.rs` — the daemon's in-process entry point
  (`fbuild_daemon::entry::run`). A library module, not a binary: it lives here
  because it is a program entry point and the process-scope dylints exempt
  `src/bin/**`. See `entry/README.md`.
