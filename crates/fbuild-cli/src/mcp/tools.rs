//! Implementation of MCP `tools/call` dispatch.

use super::util::uuid_v4;
use crate::daemon_client::DaemonClient;
use serde_json::Value;

pub(super) async fn execute_tool(
    client: &DaemonClient,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
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
                output_dir: None,
                pio_env: crate::daemon_client::capture_pio_env(),
                bloat_analysis: false,
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
                no_probe_rs: false,
                to: None,
                emulator: None,
                target: None,
                qemu: false,
                qemu_timeout: 30,
                request_id: Some(uuid_v4()),
                caller_pid,
                caller_cwd,
                src_dir: std::env::var("PLATFORMIO_SRC_DIR")
                    .ok()
                    .filter(|s| !s.is_empty()),
                output_dir: None,
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
