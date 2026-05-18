# Source

## Modules

- **`main.rs`** -- CLI entry point; spawns the larger-stack `fbuild-main` thread that hands control to `cli::async_main`
- **`cli/`** -- Clap parser, subcommand argument types, and per-subcommand handlers (see `cli/README.md`); split out of `main.rs` to keep each `.rs` under the 900 LOC gate
- **`mcp/`** -- MCP (Model Context Protocol) stdio JSON-RPC server (see `mcp/README.md`); split out of a flat `mcp.rs` to keep each `.rs` under the 900 LOC gate
- **`daemon_client.rs`** -- `DaemonClient` HTTP client, request/response types, daemon spawn with stale binary detection, NDJSON streaming, compact status display
- **`lib_select.rs`** -- diagnostic LDF-style library selection resolver (`fbuild lib-select`)
