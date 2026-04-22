# Integration Tests

Tests that download real toolchains and compile real sketches. Marked `#[ignore]` so they don't run during normal `uv run test`.

Run with: `uv run soldr cargo test -p fbuild-build -- --ignored`
