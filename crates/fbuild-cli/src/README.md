# Source

## Modules

- **`main.rs`** -- CLI entry point; Clap parser with subcommands (build, deploy, monitor, reset, purge, daemon, device, show, mcp, clang-tidy, iwyu, clang-query), dispatches to daemon client
- **`daemon_client.rs`** -- `DaemonClient` HTTP client, request/response types, daemon spawn with stale binary detection, NDJSON streaming, compact status display
- **`mcp.rs`** -- MCP (Model Context Protocol) stdio JSON-RPC server; translates AI assistant tool/resource calls into daemon HTTP requests
