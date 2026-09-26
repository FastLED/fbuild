# Tests

Repository-level contract tests for the `fbuild-python` PyO3 extension.

- `pyo3_policy.rs` keeps the PyO3 dependency family and cross-build workflow policy in sync.
- `python_facades.rs` — embedded-CPython integration tests for `SerialMonitor` /
  `AsyncSerialMonitor` (FastLED/fbuild#1485 §5.2, "AT-P" tests). Needs
  `libpython` at link and run time (the `pyo3` `auto-initialize`
  dev-dependency feature), so every test is `#[ignore]`d and run separately
  with `--ignored` by the `python-facade-tests` CI job
  (`.github/workflows/ci-test.yml`), not by `bash test`. Locally:

  ```bash
  export PYO3_PYTHON=$(which python3.13)   # or your local interpreter
  export LD_LIBRARY_PATH="$(python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))"):$LD_LIBRARY_PATH"
  soldr cargo test -p fbuild-python --test python_facades -- --ignored
  ```

  Only a representative subset of the spec's AT-P1..AT-P16 table is
  implemented (AT-P1, AT-P9); see the module doc comment for the full gap
  list and rationale.
