# Source

## Modules

- **`archive.rs`** -- Shared GNU archiver invocation with temporary response files for large or Windows object lists (#1656)

- **`lib.rs`** -- Crate root; defines `FbuildError`, `Result`, `BuildProfile`, `Platform`, `OperationType`, `DaemonState`, `SizeInfo`; re-exports `BuildLog`
- **`elf_size.rs`** -- `ElfSectionSize`, `SizeInfo::from_elf_sections()` / `from_elf_sections_with_ram()` and `PLATFORMIO_RAM_SECTIONS`: flash is every allocated section with contents (`SHT_NOBITS` never counts, FastLED/fbuild#2012); RAM is PlatformIO's `SIZEDATAREGEXP` section set by exact name (default `.data`/`.bss`/`.noinit`), so heap/stack reservations are not static RAM (FastLED/fbuild#2027)
- **`build_stamp.rs`** -- `build.rs` helper, compiled into the library only for its tests (`build_stamp_tests.rs`): digest the workspace's embedded assets (`src/` and `assets/` non-Rust files) into `build_id::EMBEDDED_ASSETS_DIGEST` for the no-op fingerprint (FastLED/fbuild#2042)
- **`commit_stamp_tests.rs`** -- tests for `../commit_stamp.rs`: git-dir resolution for worktrees (`.git` file + `commondir`) and packed refs, so the commit stamp follows HEAD, and the probe's degrade path outside a checkout
- **`build_log.rs`** -- `BuildLog` struct that accumulates output lines and optionally streams them through an `mpsc::Sender`
- **`daemon_health.rs`** -- Daemon `/health` readiness states (`healthy` / `starting` / unreachable), `ReadinessDeadline` that stretches while the daemon reports `starting`, and `wait_until_healthy()` shared by the CLI and PyO3 clients (FastLED/fbuild#1480)
- **`compiler_flags.rs`** -- `prepare_flags_for_exec()` to strip backslash-escaped quotes from GCC define flags on non-Windows platforms
- **`response_file.rs`** -- `async fn write_response_file()` (and `write_response_file_blocking` for sync escape) for GCC `@file` syntax on Windows; `replace_path_backslashes()` and `windows_temp_dir()` helpers
- **`shell_split.rs`** -- `split()` function for quote-aware tokenization that preserves backslashes as literal characters
- **`subprocess.rs`** -- `async fn run_command()` / `run_command_with_stdin()` / `run_command_passthrough()` with optional timeout, plus `_blocking` sync escape hatches; `ToolOutput` result type, Windows `CREATE_NO_WINDOW` flag, MSYS environment variable stripping, and tokio-driven Job-Object / `PR_SET_PDEATHSIG` containment (#813)
- **`termination.rs`** -- `termination(exit_code, is_unix_host)` / `host_termination()`: recognise a signal-killed subprocess (`-signal` on Unix, `128 + signal` for a tool that shells out) and name it (`SIGKILL (signal 9)`), with `KILL_HINT` saying a kill is a host/containment event rather than a source error. Only the signals whose numbers are identical on Linux and macOS are named; Windows keeps no signal number and reports `None` (FastLED/fbuild#1628 ask 3, #1630)
