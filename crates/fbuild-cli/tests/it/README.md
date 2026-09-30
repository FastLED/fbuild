# fbuild-cli integration tests

All integration tests for `fbuild-cli` compile into this single `it` binary; each
`*.rs` here is a module listed in `main.rs`. Add new integration tests
as a module here, not as a new top-level `tests/*.rs` file: every top-level
file is a separate binary that re-links the crate's whole dependency graph
(zackees/ci.yml RUST-005).
