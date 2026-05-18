//! Implementation of MCP `resources/read` dispatch.

use super::util::urlencoding_decode;

pub(super) fn read_resource(uri: &str) -> Result<(String, String), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
