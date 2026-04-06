//! MCP (Model Context Protocol) server for fbuild.
//!
//! Runs as a stdio-based JSON-RPC server that AI assistants (Claude Desktop,
//! Cursor, VS Code) can connect to for querying and controlling the fbuild
//! daemon. Translates MCP tool/resource/prompt calls into HTTP requests to
//! the running daemon.

use crate::daemon_client::DaemonClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// MCP protocol types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<ToolAnnotations>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotent_hint: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResourceDefinition {
    uri: String,
    name: String,
    description: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Debug, Serialize)]
struct PromptDefinition {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Serialize)]
struct PromptArgument {
    name: String,
    description: String,
    required: bool,
}

#[derive(Debug, Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Debug, Serialize)]
// ResourceContent is used by the MCP resources/read response format.
#[allow(dead_code)]
struct ResourceContent {
    uri: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct PromptMessage {
    role: String,
    content: TextContent,
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "get_daemon_status".to_string(),
            description: "Get daemon status including PID, uptime, version, port, and current operation state.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: None,
                idempotent_hint: None,
            }),
        },
        ToolDefinition {
            name: "list_devices".to_string(),
            description: "List all serial devices known to the daemon, with connection and lease information.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: None,
                idempotent_hint: None,
            }),
        },
        ToolDefinition {
            name: "get_lock_status".to_string(),
            description: "Get active and stale lock information from the daemon.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: None,
                idempotent_hint: None,
            }),
        },
        ToolDefinition {
            name: "trigger_build".to_string(),
            description: "Trigger a firmware build for a project. Blocks until the build completes.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory."
                    },
                    "environment": {
                        "type": "string",
                        "description": "Build environment name (e.g. 'uno', 'esp32c6')."
                    },
                    "clean": {
                        "type": "boolean",
                        "description": "Whether to perform a clean build.",
                        "default": false
                    },
                    "verbose": {
                        "type": "boolean",
                        "description": "Enable verbose compiler output.",
                        "default": false
                    },
                    "jobs": {
                        "type": ["integer", "null"],
                        "description": "Number of parallel compilation workers (null = auto)."
                    }
                },
                "required": ["project_dir", "environment"]
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                destructive_hint: Some(false),
                idempotent_hint: None,
            }),
        },
        ToolDefinition {
            name: "trigger_deploy".to_string(),
            description: "Trigger a firmware deploy (build + flash) for a project. Blocks until the deploy completes.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory."
                    },
                    "environment": {
                        "type": "string",
                        "description": "Build environment name."
                    },
                    "port": {
                        "type": ["string", "null"],
                        "description": "Serial port (e.g. 'COM3'). Null for auto-detect."
                    },
                    "skip_build": {
                        "type": "boolean",
                        "description": "Skip the build step and flash existing firmware.",
                        "default": false
                    }
                },
                "required": ["project_dir", "environment"]
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                destructive_hint: Some(true),
                idempotent_hint: None,
            }),
        },
        ToolDefinition {
            name: "refresh_devices".to_string(),
            description: "Re-scan serial ports and update the device inventory. Returns the list of currently connected devices.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                destructive_hint: Some(false),
                idempotent_hint: Some(true),
            }),
        },
        ToolDefinition {
            name: "clear_stale_locks".to_string(),
            description: "Force-release any stale (stuck) locks in the daemon.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                destructive_hint: Some(true),
                idempotent_hint: Some(true),
            }),
        },
        ToolDefinition {
            name: "get_firmware_status".to_string(),
            description: "Get firmware deployment information for a serial port (hash, source, staleness).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "port": {
                        "type": "string",
                        "description": "Serial port (e.g. 'COM3', '/dev/ttyUSB0')."
                    }
                },
                "required": ["port"]
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: None,
                idempotent_hint: None,
            }),
        },
    ]
}

fn resource_definitions() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "fbuild://daemon/log".to_string(),
            name: "Daemon Log".to_string(),
            description: "Last 200 lines of the daemon log file.".to_string(),
            mime_type: "text/plain".to_string(),
        },
        ResourceDefinition {
            uri: "fbuild://project/{project_dir}/config".to_string(),
            name: "Project Config".to_string(),
            description: "Parsed platformio.ini configuration for a project.".to_string(),
            mime_type: "application/json".to_string(),
        },
        ResourceDefinition {
            uri: "fbuild://firmware/{port}".to_string(),
            name: "Firmware Status".to_string(),
            description: "Firmware deployment information for a serial port (connection, lease, availability).".to_string(),
            mime_type: "application/json".to_string(),
        },
    ]
}

fn prompt_definitions() -> Vec<PromptDefinition> {
    vec![
        PromptDefinition {
            name: "diagnose_build_failure".to_string(),
            description: "Gather diagnostic information for a build failure (errors, recent ops, stale locks).".to_string(),
            arguments: Some(vec![PromptArgument {
                name: "project_dir".to_string(),
                description: "Project directory to filter diagnostics (optional).".to_string(),
                required: false,
            }]),
        },
        PromptDefinition {
            name: "recommend_deploy_target".to_string(),
            description: "Recommend which device to deploy firmware to based on device inventory and lease status.".to_string(),
            arguments: Some(vec![PromptArgument {
                name: "environment".to_string(),
                description: "Target build environment (optional).".to_string(),
                required: false,
            }]),
        },
    ]
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

async fn execute_tool(client: &DaemonClient, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "get_daemon_status" => {
            let info = client
                .daemon_info()
                .await
                .map_err(|e| format!("Failed to get daemon info: {}", e))?;

            Ok(serde_json::json!({
                "pid": info.pid,
                "uptime_seconds": info.uptime_seconds,
                "version": info.version,
                "port": info.port,
                "dev_mode": info.dev_mode,
                "state": format!("{:?}", info.daemon_state).to_lowercase(),
                "operation_in_progress": info.operation_in_progress,
                "current_operation": info.current_operation,
                "client_count": info.client_count,
                "spawner_cwd": info.spawner_cwd,
            }))
        }
        "list_devices" => {
            let devices = client
                .list_devices(false)
                .await
                .map_err(|e| format!("Failed to list devices: {}", e))?;

            let device_list: Vec<Value> = devices
                .devices
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "port": d.port,
                        "device_id": d.device_id,
                        "description": d.description,
                        "vid": d.vid,
                        "pid": d.pid,
                    })
                })
                .collect();

            Ok(serde_json::json!({
                "device_count": device_list.len(),
                "devices": device_list,
            }))
        }
        "get_lock_status" => {
            let locks = client
                .lock_status()
                .await
                .map_err(|e| format!("Failed to get lock status: {}", e))?;

            Ok(serde_json::json!({
                "port_locks": locks.port_locks.iter().map(|l| serde_json::json!({
                    "port": l.port,
                    "is_held": l.is_held,
                    "is_open": l.is_open,
                    "writer_client_id": l.writer_client_id,
                    "reader_count": l.reader_count,
                })).collect::<Vec<_>>(),
                "project_locks": locks.project_locks.iter().map(|l| serde_json::json!({
                    "project_dir": l.project_dir,
                    "is_held": l.is_held,
                })).collect::<Vec<_>>(),
                "stale_locks": locks.stale_locks,
                "active_port_lock_count": locks.port_locks.iter().filter(|l| l.is_held).count(),
                "active_project_lock_count": locks.project_locks.iter().filter(|l| l.is_held).count(),
            }))
        }
        "trigger_build" => {
            let project_dir = args
                .get("project_dir")
                .and_then(|v| v.as_str())
                .ok_or("project_dir is required")?
                .to_string();
            let environment = args
                .get("environment")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let clean = args.get("clean").and_then(|v| v.as_bool()).unwrap_or(false);
            let verbose = args
                .get("verbose")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let jobs = args
                .get("jobs")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);

            let (caller_pid, caller_cwd) = crate::daemon_client::caller_info();
            let req = crate::daemon_client::BuildRequest {
                project_dir,
                environment,
                clean_build: clean,
                verbose,
                jobs,
                profile: None,
                generate_compiledb: false,
                compiledb_only: false,
                request_id: Some(uuid_v4()),
                caller_pid,
                caller_cwd,
                stream: false,
                symbol_analysis: false,
                symbol_analysis_path: None,
                no_timestamp: false,
                src_dir: std::env::var("PLATFORMIO_SRC_DIR")
                    .ok()
                    .filter(|s| !s.is_empty()),
                pio_env: crate::daemon_client::capture_pio_env(),
            };

            let resp = client
                .build(&req)
                .await
                .map_err(|e| format!("Build request failed: {}", e))?;

            Ok(serde_json::json!({
                "success": resp.success,
                "message": resp.message,
                "exit_code": resp.exit_code,
                "request_id": resp.request_id,
            }))
        }
        "trigger_deploy" => {
            let project_dir = args
                .get("project_dir")
                .and_then(|v| v.as_str())
                .ok_or("project_dir is required")?
                .to_string();
            let environment = args
                .get("environment")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let port = args
                .get("port")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let skip_build = args
                .get("skip_build")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let (caller_pid, caller_cwd) = crate::daemon_client::caller_info();
            let req = crate::daemon_client::DeployRequest {
                project_dir,
                environment,
                port,
                monitor_after: false,
                skip_build,
                clean_build: false,
                verbose: false,
                monitor_timeout: None,
                monitor_halt_on_error: None,
                monitor_halt_on_success: None,
                monitor_expect: None,
                monitor_show_timestamp: true,
                baud_rate: None,
                qemu: false,
                qemu_timeout: 30,
                request_id: Some(uuid_v4()),
                caller_pid,
                caller_cwd,
                src_dir: std::env::var("PLATFORMIO_SRC_DIR")
                    .ok()
                    .filter(|s| !s.is_empty()),
                pio_env: crate::daemon_client::capture_pio_env(),
            };

            let resp = client
                .deploy(&req)
                .await
                .map_err(|e| format!("Deploy request failed: {}", e))?;

            Ok(serde_json::json!({
                "success": resp.success,
                "message": resp.message,
                "exit_code": resp.exit_code,
                "request_id": resp.request_id,
            }))
        }
        "refresh_devices" => {
            let devices = client
                .list_devices(true)
                .await
                .map_err(|e| format!("Failed to refresh devices: {}", e))?;

            let device_list: Vec<Value> = devices
                .devices
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "port": d.port,
                        "device_id": d.device_id,
                        "description": d.description,
                    })
                })
                .collect();

            Ok(serde_json::json!({
                "device_count": device_list.len(),
                "devices": device_list,
            }))
        }
        "clear_stale_locks" => {
            let resp = client
                .clear_locks()
                .await
                .map_err(|e| format!("Failed to clear locks: {}", e))?;

            Ok(serde_json::json!({
                "released_count": resp.cleared_count,
                "message": resp.message,
            }))
        }
        "get_firmware_status" => {
            let port = args
                .get("port")
                .and_then(|v| v.as_str())
                .ok_or("port is required")?;

            let status = client
                .device_status(port)
                .await
                .map_err(|e| format!("Failed to get device status: {}", e))?;

            Ok(serde_json::json!({
                "port": status.port,
                "device_id": status.device_id,
                "description": status.description,
                "is_connected": status.is_connected,
                "available_for_exclusive": status.available_for_exclusive,
                "exclusive_holder": status.exclusive_holder,
                "monitor_count": status.monitor_count,
            }))
        }
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

// ---------------------------------------------------------------------------
// Resource reading
// ---------------------------------------------------------------------------

fn read_resource(uri: &str) -> Result<(String, String), String> {
    if uri == "fbuild://daemon/log" {
        let log_file = fbuild_paths::get_daemon_log_file();
        let text = std::fs::read_to_string(&log_file)
            .unwrap_or_else(|_| "(daemon log not available)".to_string());
        let lines: Vec<&str> = text.lines().collect();
        let tail = if lines.len() > 200 {
            &lines[lines.len() - 200..]
        } else {
            &lines
        };
        Ok(("text/plain".to_string(), tail.join("\n")))
    } else if uri.starts_with("fbuild://project/") && uri.ends_with("/config") {
        let path_part = uri
            .strip_prefix("fbuild://project/")
            .and_then(|s| s.strip_suffix("/config"))
            .ok_or("Invalid project config URI")?;

        let decoded = urlencoding_decode(path_part);
        let ini_path = std::path::Path::new(&decoded).join("platformio.ini");

        if !ini_path.exists() {
            return Ok((
                "application/json".to_string(),
                serde_json::json!({"error": format!("platformio.ini not found at {}", ini_path.display())}).to_string(),
            ));
        }

        let content = std::fs::read_to_string(&ini_path)
            .map_err(|e| format!("Failed to read {}: {}", ini_path.display(), e))?;

        Ok((
            "application/json".to_string(),
            serde_json::json!({
                "project_dir": decoded,
                "raw_ini": content,
            })
            .to_string(),
        ))
    } else if uri.starts_with("fbuild://firmware/") {
        let port = uri
            .strip_prefix("fbuild://firmware/")
            .ok_or("Invalid firmware URI")?;
        let port = urlencoding_decode(port);

        // This is a synchronous context, but we need the device_status endpoint.
        // Return a JSON pointer that tells the client how to fetch it.
        Ok((
            "application/json".to_string(),
            serde_json::json!({
                "port": port,
                "note": "Use the get_firmware_status tool for live device status.",
                "endpoint": format!("/api/devices/{}/status", port)
            })
            .to_string(),
        ))
    } else {
        Err(format!("Unknown resource URI: {}", uri))
    }
}

// ---------------------------------------------------------------------------
// Prompt execution
// ---------------------------------------------------------------------------

async fn execute_prompt(
    client: &DaemonClient,
    name: &str,
    args: &Value,
) -> Result<Vec<PromptMessage>, String> {
    match name {
        "diagnose_build_failure" => {
            let mut sections = vec!["# Build Failure Diagnostic Report\n".to_string()];

            // Daemon status
            match client.daemon_info().await {
                Ok(info) => {
                    sections.push("## Daemon Status\n".to_string());
                    sections.push(format!(
                        "- State: {:?}\n- PID: {}\n- Uptime: {:.1}s\n- Operation in progress: {}\n",
                        info.daemon_state,
                        info.pid,
                        info.uptime_seconds,
                        info.operation_in_progress
                    ));
                    if let Some(op) = &info.current_operation {
                        sections.push(format!("- Current operation: {}\n", op));
                    }
                }
                Err(e) => {
                    sections.push(format!("## Daemon Status\n\nDaemon unreachable: {}\n", e));
                }
            }

            // Stale locks
            if let Ok(locks) = client.lock_status().await {
                if !locks.stale_locks.is_empty() {
                    sections.push("## Stale Lock Warning\n".to_string());
                    for lock in &locks.stale_locks {
                        sections.push(format!("- Stale lock: `{}`", lock));
                    }
                    sections.push(
                        "\nConsider running the `clear_stale_locks` tool to release these.\n"
                            .to_string(),
                    );
                }
            }

            // Devices
            if let Ok(devices) = client.list_devices(false).await {
                sections.push("## Connected Devices\n".to_string());
                if devices.devices.is_empty() {
                    sections.push("No devices connected.\n".to_string());
                } else {
                    for d in &devices.devices {
                        sections.push(format!("- **{}** ({})", d.port, d.description));
                    }
                    sections.push(String::new());
                }
            }

            let _project_dir = args
                .get("project_dir")
                .and_then(|v| v.as_str())
                .unwrap_or("(not specified)");

            Ok(vec![PromptMessage {
                role: "user".to_string(),
                content: TextContent {
                    content_type: "text".to_string(),
                    text: sections.join("\n"),
                },
            }])
        }
        "recommend_deploy_target" => {
            let mut sections = vec!["# Deploy Target Recommendation\n".to_string()];

            match client.list_devices(true).await {
                Ok(devices) => {
                    sections.push("## Device Inventory\n".to_string());
                    sections.push(format!("- Total devices: {}\n", devices.devices.len()));

                    if devices.devices.is_empty() {
                        sections.push(
                            "**No devices connected.** Plug in a board and run `refresh_devices`.\n"
                                .to_string(),
                        );
                    } else {
                        sections.push("## Connected Devices\n".to_string());
                        for d in &devices.devices {
                            sections.push(format!("- **{}** ({})", d.port, d.description));
                        }
                        sections.push(String::new());

                        sections.push("## Recommendation\n".to_string());
                        let first = &devices.devices[0];
                        sections.push(format!(
                            "Deploy to **{}** ({}) - first available device.\n",
                            first.port, first.description
                        ));
                    }
                }
                Err(e) => {
                    sections.push(format!("Cannot list devices (daemon error): {}\n", e));
                }
            }

            Ok(vec![PromptMessage {
                role: "user".to_string(),
                content: TextContent {
                    content_type: "text".to_string(),
                    text: sections.join("\n"),
                },
            }])
        }
        _ => Err(format!("Unknown prompt: {}", name)),
    }
}

// ---------------------------------------------------------------------------
// MCP server main loop
// ---------------------------------------------------------------------------

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

        let id = request.id.unwrap();
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

/// Simple percent-decoding for URI path segments.
fn urlencoding_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let val = hex_val(hi) * 16 + hex_val(lo);
            result.push(val as char);
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Generate a UUID v4 string (simple implementation without extra deps).
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Use time-based pseudo-random (good enough for request IDs, not crypto)
    let pid = std::process::id() as u128;
    let val = seed ^ (pid << 32) ^ (seed >> 16);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (val >> 96) as u32,
        (val >> 80) as u16,
        (val >> 64) as u16 & 0x0fff,
        ((val >> 48) as u16 & 0x3fff) | 0x8000,
        val as u64 & 0xffffffffffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_are_valid_json() {
        let tools = tool_definitions();
        assert!(tools.len() >= 7);
        for tool in &tools {
            let json = serde_json::to_value(tool).unwrap();
            assert!(json.get("name").is_some());
            assert!(json.get("inputSchema").is_some());
        }
    }

    #[test]
    fn resource_definitions_are_valid() {
        let resources = resource_definitions();
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0].uri, "fbuild://daemon/log");
        assert_eq!(resources[2].uri, "fbuild://firmware/{port}");
    }

    #[test]
    fn prompt_definitions_are_valid() {
        let prompts = prompt_definitions();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].name, "diagnose_build_failure");
        assert_eq!(prompts[1].name, "recommend_deploy_target");
    }

    #[test]
    fn urlencoding_decode_works() {
        assert_eq!(urlencoding_decode("hello%20world"), "hello world");
        assert_eq!(urlencoding_decode("C%3A%5Cdev"), "C:\\dev");
        assert_eq!(urlencoding_decode("no-encoding"), "no-encoding");
    }

    #[test]
    fn uuid_v4_has_correct_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().nth(8), Some('-'));
        assert_eq!(id.chars().nth(13), Some('-'));
        assert_eq!(id.chars().nth(14), Some('4')); // version 4
        assert_eq!(id.chars().nth(18), Some('-'));
    }

    #[test]
    fn json_rpc_response_serialization() {
        let resp =
            JsonRpcResponse::success(Value::Number(1.into()), serde_json::json!({"key": "value"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn json_rpc_error_serialization() {
        let resp = JsonRpcResponse::error(
            Value::Number(2.into()),
            -32601,
            "Method not found".to_string(),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32601"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn read_resource_unknown_uri_returns_error() {
        let result = read_resource("fbuild://unknown/thing");
        assert!(result.is_err());
    }

    #[test]
    fn read_resource_firmware_returns_json() {
        let (mime, text) = read_resource("fbuild://firmware/COM3").unwrap();
        assert_eq!(mime, "application/json");
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["port"], "COM3");
    }
}
