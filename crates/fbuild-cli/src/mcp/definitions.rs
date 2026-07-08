//! Static tool, resource, and prompt definitions advertised by the MCP server.

use super::types::{
    PromptArgument, PromptDefinition, ResourceDefinition, ToolAnnotations, ToolDefinition,
};

pub(super) fn tool_definitions() -> Vec<ToolDefinition> {
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
                    },
                    "no_probe_rs": {
                        "type": "boolean",
                        "description": "Force LPC deploys through lpc21isp instead of the probe-rs SWD fast path.",
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

pub(super) fn resource_definitions() -> Vec<ResourceDefinition> {
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

pub(super) fn prompt_definitions() -> Vec<PromptDefinition> {
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
    fn trigger_deploy_exposes_probe_rs_opt_out() {
        let tools = tool_definitions();
        let deploy = tools
            .iter()
            .find(|tool| tool.name == "trigger_deploy")
            .expect("trigger_deploy tool is advertised");

        assert_eq!(
            deploy
                .input_schema
                .pointer("/properties/no_probe_rs/type")
                .and_then(|value| value.as_str()),
            Some("boolean")
        );
        assert_eq!(
            deploy
                .input_schema
                .pointer("/properties/no_probe_rs/default")
                .and_then(|value| value.as_bool()),
            Some(false)
        );
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
}
