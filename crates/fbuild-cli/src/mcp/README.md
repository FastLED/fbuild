# MCP Module

The MCP (Model Context Protocol) stdio JSON-RPC server. Translates AI assistant
tool/resource/prompt calls into HTTP requests against the fbuild daemon.

## Files

- **`mod.rs`** -- Module root. Wires submodules together and re-exports `run_mcp_server` (the only public item).
- **`jsonrpc.rs`** -- JSON-RPC 2.0 envelope types (`JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`).
- **`types.rs`** -- MCP protocol value types (`ToolDefinition`, `ToolAnnotations`, `ResourceDefinition`, `PromptDefinition`, `PromptArgument`, `TextContent`, `ResourceContent`, `PromptMessage`).
- **`definitions.rs`** -- Static lists of tools, resources, and prompts advertised by the server.
- **`tools.rs`** -- `tools/call` dispatch; one match arm per tool, each translating into a `DaemonClient` HTTP call.
- **`resources.rs`** -- `resources/read` dispatch for `fbuild://daemon/log`, `fbuild://project/{dir}/config`, and `fbuild://firmware/{port}`.
- **`prompts.rs`** -- `prompts/get` dispatch for `diagnose_build_failure` and `recommend_deploy_target`.
- **`util.rs`** -- Helpers: `urlencoding_decode`, `uuid_v4` (no extra dependencies).
- **`server.rs`** -- Stdio loop: reads JSON-RPC requests from stdin, dispatches to the relevant submodule, writes responses to stdout. Exports `run_mcp_server`.

## Public API

Only `run_mcp_server` is re-exported through `mcp::`; everything else is crate-private. The original `mcp.rs` was split into submodules to keep each file under the 1000-LOC gate.
