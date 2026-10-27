# fbuild-build integration tests

All integration tests for `fbuild-build` compile into this single `it` binary; each
`*.rs` here is a module listed in `main.rs`. Add new integration tests
as a module here, not as a new top-level `tests/*.rs` file: every top-level
file is a separate binary that re-links the crate's whole dependency graph
(zackees/ci.yml RUST-005).

`samd_acceptance` builds real SAMD21 and SAMD51 firmware with a sketch-provided
interrupt handler (including an LTO build), plus ClearCore vendor syscall linkage. Run it explicitly with
`soldr cargo test -p fbuild-build --test it samd_acceptance:: -- --ignored`.

`renesas_acceptance` builds UNO R4 firmware using `String` and `IPAddress`
and verifies that the supported default core supplied both source files.
Run with `soldr cargo test -p fbuild-build --test it renesas_acceptance:: -- --ignored`.

`rp2040_acceptance` builds a real default `pico` project, including boot2
assembly and USB string definitions. Run it with
`soldr cargo test -p fbuild-build --test it rp2040_acceptance:: -- --ignored`.

`link_overflow` invokes native GCC and the production linker to check failed
GNU links report each overflowing region's actual map capacity (#1658).
