# fbuild-test-support

Test utilities and fixtures for fbuild workspace crates.

## Key Functions

- `create_test_project(env_name, platform, board)` -- Creates a `tempfile::TempDir` containing a minimal `platformio.ini`, `src/` directory, and `src/main.cpp` with Arduino stubs (`setup`/`loop`)

## Usage

Used by other crates as a `[dev-dependencies]` entry to get realistic temporary project directories for integration tests without manual setup.
