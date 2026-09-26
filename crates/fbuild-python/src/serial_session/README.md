# Serial session tests

`tests.rs` exercises the shared WebSocket session core against a local fake
daemon. Longer stress and teardown cases live in `tests/extended.rs` so each
Rust source file stays below the repository's 1,000-line CI limit.
