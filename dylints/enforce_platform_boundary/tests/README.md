# Integration tests

This directory isolates the Dylint UI harness in its own test process so its
nested-Cargo environment overrides cannot race with lint-library unit tests.
