//! MCP stdio server main loop: reads JSON-RPC requests from stdin, writes responses to stdout.

use super::definitions::{prompt_definitions, resource_definitions, tool_definitions};
use super::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use super::prompts::execute_prompt;
use super::resources::read_resource;
use super::tools::execute_tool;
use crate::daemon_client::DaemonClient;
use serde_json::Value;
use std::io::{self, BufRead, Write};

pub async fn run_mcp_server() -> i32 {
    let client = DaemonClient::new();

    // Ensure daemon is running before starting MCP server
    if let Err(e) = crate::daemon_client::ensure_daemon_running().await {
        let err = serde_json::json!({
            "error": format!("Failed to start daemon: {}", e)
        });
        eprintln!("MCP server: {}", err);
        return 1;
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // EOF or read error
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                // Parse error — send JSON-RPC error
                let resp =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {}", e));
                send_response(&stdout, &resp);
                continue;
            }
        };

        if request.jsonrpc != "2.0" {
            if let Some(id) = request.id {
                let resp =
                    JsonRpcResponse::error(id, -32600, "Invalid JSON-RPC version".to_string());
                send_response(&stdout, &resp);
            }
            continue;
        }

        // Notifications (no id) — handle silently
        if request.id.is_none() {
            // "initialized", "notifications/cancelled", etc. — just acknowledge
            continue;
        }

        let id = request
            .id
            .expect("fbuild-cli: notification (id.is_none()) handled above; id is Some here");
        let params = request.params.unwrap_or(Value::Null);

        let response = match request.method.as_str() {
            "initialize" => JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {},
                        "resources": {},
                        "prompts": {}
                    },
                    "serverInfo": {
                        "name": "fbuild",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),
            "ping" => JsonRpcResponse::success(id, serde_json::json!({})),
            "tools/list" => {
                let tools = tool_definitions();
                JsonRpcResponse::success(id, serde_json::json!({ "tools": tools }))
            }
            "tools/call" => {
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let tool_args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                match execute_tool(&client, tool_name, &tool_args).await {
                    Ok(result) => JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&result).unwrap_or_default()
                            }]
                        }),
                    ),
                    Err(e) => JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": format!("Error: {}", e)
                            }],
                            "isError": true
                        }),
                    ),
                }
            }
            "resources/list" => {
                let resources = resource_definitions();
                JsonRpcResponse::success(id, serde_json::json!({ "resources": resources }))
            }
            "resources/read" => {
                let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

                match read_resource(uri) {
                    Ok((mime_type, text)) => JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "contents": [{
                                "uri": uri,
                                "mimeType": mime_type,
                                "text": text
                            }]
                        }),
                    ),
                    Err(e) => JsonRpcResponse::error(id, -32602, format!("Resource error: {}", e)),
                }
            }
            "prompts/list" => {
                let prompts = prompt_definitions();
                JsonRpcResponse::success(id, serde_json::json!({ "prompts": prompts }))
            }
            "prompts/get" => {
                let prompt_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let prompt_args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                match execute_prompt(&client, prompt_name, &prompt_args).await {
                    Ok(messages) => JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "description": format!("Results for prompt '{}'", prompt_name),
                            "messages": messages
                        }),
                    ),
                    Err(e) => JsonRpcResponse::error(id, -32602, format!("Prompt error: {}", e)),
                }
            }
            _ => {
                JsonRpcResponse::error(id, -32601, format!("Method not found: {}", request.method))
            }
        };

        send_response(&stdout, &response);
    }

    0
}

fn send_response(stdout: &io::Stdout, response: &JsonRpcResponse) {
    let json = serde_json::to_string(response).unwrap_or_default();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", json);
    let _ = out.flush();
}
