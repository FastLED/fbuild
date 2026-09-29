# Tests

Repository-level contract tests for the `fbuild-python` PyO3 extension.

- `pyo3_policy.rs` keeps the PyO3 dependency family and cross-build workflow policy in sync.
- `python_facades.rs` — embedded-CPython integration tests for `SerialMonitor` /
  `AsyncSerialMonitor` (FastLED/fbuild#1485 §5.2, "AT-P" tests). Needs
  `libpython` at link and run time (the `pyo3` `auto-initialize`
  dev-dependency feature), so every test is `#[ignore]`d and run separately
  with `--ignored` by the `python-facade-tests` CI job
  (`.github/workflows/check-ubuntu.yml`), not by `bash test`. Locally:

  ```bash
  export PYO3_PYTHON="$(uv python find 3.13)"   # or your local interpreter
  soldr cargo test -p fbuild-python --test python_facades -- --ignored
  ```

  No `LD_LIBRARY_PATH` export is needed: `crates/fbuild-python/build.rs` adds the
  library directory of the exact `PYO3_PYTHON` interpreter to the test binaries'
  RUNPATH (`cargo:rustc-link-arg-tests`, unix only; the production extension
  module is unaffected), and
  `loaded_libpython_comes_from_the_embedded_interpreters_libdir` fails if the
  loaded `libpython` ever comes from a different interpreter
  (FastLED/fbuild#1487). The tests cover AT-P1..AT-P16;
  see each test's comment for the scenario and acceptance criterion.
