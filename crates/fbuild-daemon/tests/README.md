## fbuild-daemon integration tests

Integration-level tests that need to spawn the actual `fbuild-daemon`
binary. These rely on `CARGO_BIN_EXE_fbuild-daemon` (provided automatically
by Cargo for `[[bin]]` targets in the same crate) and on real OS-level
networking.

Tests that require an external binary, real ports, or platform-specific
process control (`taskkill`, `kill -9`) are marked `#[ignore]` and must be
run explicitly:

```bash
cargo test --release -p fbuild-daemon -- --ignored
```

### Files

- `port_recovery.rs` — exposes ISSUES.md "Issue B5a": after a hard kill of
  a daemon with an open client connection, a fresh daemon must still be
  able to bind the same port. The test is `#[ignore]` because it leaves
  port state lingering and depends on `taskkill`/`kill` being on PATH.
