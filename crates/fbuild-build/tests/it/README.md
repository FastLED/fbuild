# fbuild-build integration tests

All integration tests for `fbuild-build` compile into this single `it` binary; each
`*.rs` here is a module listed in `main.rs`. Add new integration tests
as a module here, not as a new top-level `tests/*.rs` file: every top-level
file is a separate binary that re-links the crate's whole dependency graph
(zackees/ci.yml RUST-005).

`samd_acceptance` builds real SAMD21 and SAMD51 firmware with a sketch-provided
interrupt handler (including an LTO build), plus ClearCore vendor syscall linkage. Run it explicitly with
`soldr cargo test -p fbuild-build --test it samd_acceptance:: -- --ignored`.
