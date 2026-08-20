# Integration tests

This directory isolates the Dylint UI harness in its own test process so its
nested-Cargo target override cannot race with lint-library unit tests.
