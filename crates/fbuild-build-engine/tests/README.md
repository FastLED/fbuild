# `fbuild-build-engine` integration tests

Tests that need a separate final executable, or that exercise a contract
spanning this crate and one of its external dependencies.

- **`dev_daemon_namespace_isolation.rs`** — proves the dev daemon-identity
  stamp `fbuild-paths` exports actually changes the zccache IPC endpoint
  (FastLED/fbuild#1285). It lives here because this is the crate that depends
  on both sides: `fbuild-paths` produces the stamp but cannot see zccache, and
  zccache consumes it but is a pinned external dependency. A repin that
  dropped endpoint namespacing would otherwise pass every other test in the
  tree while quietly restoring the `displace-stale` daemon war.
