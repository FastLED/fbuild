# Python facade acceptance cases

`cases.rs` and `extended.rs` contain the embedded-CPython tests used by the
`python_facades` integration-test target. The parent file holds the child
process fake daemon and common helpers. Splitting keeps each Rust source file
below the repository's 1,000-line CI limit.
