//! `fbuild reset` — reboot a device without re-flashing.

use crate::output;

pub fn run_reset(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let project_path = std::path::Path::new(&project_dir);
    let ini_path = project_path.join("platformio.ini");

    // Read config to detect board/platform
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    let env_name = if let Some(ref e) = environment {
        e.clone()
    } else {
        config
            .get_default_environment()
            .ok_or_else(|| {
                fbuild_core::FbuildError::ConfigError(
                    "no environment found in platformio.ini".to_string(),
                )
            })?
            .to_string()
    };

    let env_config = config.get_env_config(&env_name)?;
    let board = env_config.get("board").ok_or_else(|| {
        fbuild_core::FbuildError::ConfigError(format!(
            "no 'board' key in environment '{}'",
            env_name
        ))
    })?;

    let platform = fbuild_deploy::reset::detect_platform_for_reset(board);

    // Determine port
    let port = port.ok_or_else(|| {
        fbuild_core::FbuildError::SerialError("no serial port specified (use --port)".to_string())
    })?;

    output::progress(format!("resetting {} device on {}...", platform, port));
    match fbuild_deploy::reset::reset_device(platform, &port, verbose)? {
        true => {
            output::result("device reset successful");
            Ok(())
        }
        false => {
            output::error("device reset failed");
            std::process::exit(1);
        }
    }
}
