# Source

## Modules

- **`lib.rs`** -- Crate root; defines `FbuildError`, `Result`, `BuildProfile`, `Platform`, `OperationType`, `DaemonState`, `SizeInfo`; re-exports `BuildLog`
- **`build_log.rs`** -- `BuildLog` struct that accumulates output lines and optionally streams them through an `mpsc::Sender`
- **`compiler_flags.rs`** -- `prepare_flags_for_exec()` to strip backslash-escaped quotes from GCC define flags on non-Windows platforms
- **`response_file.rs`** -- `async fn write_response_file()` (and `write_response_file_blocking` for sync escape) for GCC `@file` syntax on Windows; `replace_path_backslashes()` and `windows_temp_dir()` helpers
- **`shell_split.rs`** -- `split()` function for quote-aware tokenization that preserves backslashes as literal characters
- **`subprocess.rs`** -- `async fn run_command()` / `run_command_with_stdin()` / `run_command_passthrough()` with optional timeout, plus `_blocking` sync escape hatches; `ToolOutput` result type, Windows `CREATE_NO_WINDOW` flag, MSYS environment variable stripping, and tokio-driven Job-Object / `PR_SET_PDEATHSIG` containment (#813)
