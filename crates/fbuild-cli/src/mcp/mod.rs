//! MCP (Model Context Protocol) server for fbuild.
//!
//! Runs as a stdio-based JSON-RPC server that AI assistants (Claude Desktop,
//! Cursor, VS Code) can connect to for querying and controlling the fbuild
//! daemon. Translates MCP tool/resource/prompt calls into HTTP requests to
//! the running daemon.
//!
//! Submodules:
//!
//! - [`jsonrpc`] - JSON-RPC 2.0 envelope types.
//! - [`types`] - MCP protocol value types (tool/resource/prompt definitions).
//! - [`definitions`] - Static tool, resource, and prompt advertisements.
//! - [`tools`] - `tools/call` dispatch and per-tool implementations.
//! - [`resources`] - `resources/read` dispatch.
//! - [`prompts`] - `prompts/get` dispatch.
//! - [`util`] - Small standalone helpers (URI decoding, UUID generation).
//! - [`server`] - Stdio loop tying everything together.

mod definitions;
mod jsonrpc;
mod prompts;
mod resources;
mod server;
mod tools;
mod types;
mod util;

pub use server::run_mcp_server;
