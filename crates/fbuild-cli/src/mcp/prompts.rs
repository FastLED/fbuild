//! Implementation of MCP `prompts/get` dispatch.

use super::types::{PromptMessage, TextContent};
use crate::daemon_client::DaemonClient;
use serde_json::Value;

pub(super) async fn execute_prompt(
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
