# fbuild-core

Core types, errors, and utilities shared across all fbuild crates.

## Key Types

- `FbuildError` -- Workspace-wide error enum (build, deploy, serial, config, package, daemon, timeout, IO)
- `Result<T>` -- Alias for `std::result::Result<T, FbuildError>`
- `BuildProfile` -- Release or Quick build profile, with directory-name mapping
- `Platform` -- Target platform enum (AtmelAvr, Espressif32, Teensy, Wasm, etc.) with substring-based parsing from PlatformIO config values
- `OperationType` -- Daemon operation variants (Build, Deploy, Monitor, BuildAndDeploy, InstallDependencies)
- `DaemonState` -- Daemon lifecycle states (Idle, Building, Deploying, Monitoring, Completed, Failed, Cancelled)
- `SizeInfo` -- Firmware size breakdown (text/data/bss/flash/RAM) with Berkeley and AVR section format parsers
- `BuildLog` -- Build output accumulator with optional real-time channel streaming
- `ToolOutput` -- Captured subprocess result (stdout, stderr, exit code)
- `platform` -- Single host selector and neutral process, fs, IPC, executable, host, and device namespaces

## Modules

- **build_log** -- Centralized build output log with optional `mpsc::Sender` streaming
- **compiler_flags** -- Platform-correct escaping for GCC `-D` define flags
- **file_lock** -- Generic OS-released shared/exclusive file-lock primitives, used by `fbuild-paths::daemon_ownership` for fbuild-daemon startup/lifetime ownership (not object-cache access, which stays zccache-internal)
- **platform::process** -- neutral containment, detached and Tokio child spawning, PID liveness/image inspection, termination, wait, and exit-code interpretation; concrete host mechanics stay in the selected platform tree and compatible operations delegate to `running-process`
- **response_file** -- GCC `@file` response file writer for Windows command-line length limits
- **shell_split** -- Quote-aware string splitting that treats backslashes as literal (Windows-safe)
- **subprocess** -- Command runner with timeout, `CREATE_NO_WINDOW` on Windows, and MSYS environment stripping
