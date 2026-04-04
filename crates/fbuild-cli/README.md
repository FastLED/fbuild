# fbuild-cli

Clap-based CLI for fbuild. Thin HTTP client that delegates all work to the daemon. Subcommands: build, deploy, monitor, reset, purge, daemon, device, show, mcp, clang-tidy, iwyu, clang-query.

## Key Types

- `DaemonClient` -- async HTTP client wrapping reqwest; methods for build, deploy, monitor, health, device management, lock management
- `BuildRequest` / `DeployRequest` / `MonitorRequest` -- JSON request bodies sent to daemon endpoints
- `OperationResponse` -- parsed daemon response with success/exit_code/message

## Modules

- **daemon_client** -- `DaemonClient`, request/response types, `ensure_daemon_running` (spawn + stale detection), streaming NDJSON build output
- **mcp** -- stdio-based MCP (Model Context Protocol) JSON-RPC server for AI assistant integration

## Subcommands

- `build` -- compile firmware (supports streaming output, compiledb target, quick/release profiles)
- `deploy` -- flash firmware to device (optional post-deploy monitor, QEMU support)
- `monitor` -- serial monitor with halt-on-error/success and timeout
- `reset` -- reset device via DTR/RTS without re-flashing
- `purge` -- clear cached packages and build artifacts
- `daemon` -- start/stop/info/restart/logs management
- `device` -- list/status/lease/release/preempt connected devices
- `show` -- display daemon logs
- `mcp` -- start MCP server for AI assistants
