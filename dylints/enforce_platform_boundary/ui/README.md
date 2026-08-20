# UI fixtures

`disallowed.rs` covers private, inactive, test-only, compile-host-fact,
single-segment native-import, concrete-module, and repeated-identical
constructs. The lint's focused unit test proves that the second identical
occurrence exceeds a one-row baseline. `allowed.rs` proves ordinary
target-independent Rust remains valid.
