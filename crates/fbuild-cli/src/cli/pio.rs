//! PlatformIO passthrough: delegates to `pio` CLI instead of fbuild daemon.
//!
//! Used when callers pass `--platformio` to the top-level `build`, `deploy`,
//! or `monitor` subcommands so a single fbuild binary can drive either the
//! fbuild daemon or the upstream `pio` CLI for A/B comparisons.

/// Find the `pio` binary. Checks PATH first, then the fbuild cache.
pub fn find_pio() -> fbuild_core::Result<std::path::PathBuf> {
    // Check PATH
    let locator = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = fbuild_core::subprocess::run_command(&[locator, "pio"], None, None, None) {
        if output.success() {
            let path = output
                .stdout
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Ok(std::path::PathBuf::from(path));
            }
        }
    }

    // Check fbuild cache (PlatformIO installed via iso_env)
    let cache = fbuild_paths::get_cache_root().join("platform");
    let candidates = if cfg!(windows) {
        vec![
            cache.join("Scripts").join("pio.exe"),
            cache.join("Scripts").join("pio"),
        ]
    } else {
        vec![cache.join("bin").join("pio")]
    };
    for c in candidates {
        if c.exists() {
            return Ok(c);
        }
    }

    Err(fbuild_core::FbuildError::Other(
        "PlatformIO not found. Install it with: pip install platformio".to_string(),
    ))
}

/// Run a PlatformIO command with real-time output streaming.
pub fn run_pio_command(args: &[&str]) -> fbuild_core::Result<()> {
    let pio = find_pio()?;
    let pio_str = pio.to_string_lossy();
    let mut argv: Vec<&str> = vec![pio_str.as_ref()];
    argv.extend_from_slice(args);
    let code = fbuild_core::subprocess::run_command_passthrough(&argv, None, None, None)
        .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to run pio: {}", e)))?;

    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub fn pio_build(
    project_dir: &str,
    environment: Option<&str>,
    clean: bool,
    verbose: bool,
) -> fbuild_core::Result<()> {
    if clean {
        let mut args = vec!["run", "--target", "clean", "-d", project_dir];
        if let Some(env) = environment {
            args.extend(["-e", env]);
        }
        let _ = run_pio_command(&args);
    }
    let mut args = vec!["run", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if verbose {
        args.push("-v");
    }
    run_pio_command(&args)
}

pub fn pio_deploy(
    project_dir: &str,
    environment: Option<&str>,
    port: Option<&str>,
    clean: bool,
    verbose: bool,
) -> fbuild_core::Result<()> {
    if clean {
        let mut args = vec!["run", "--target", "clean", "-d", project_dir];
        if let Some(env) = environment {
            args.extend(["-e", env]);
        }
        let _ = run_pio_command(&args);
    }
    let mut args = vec!["run", "--target", "upload", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if let Some(p) = port {
        args.extend(["--upload-port", p]);
    }
    if verbose {
        args.push("-v");
    }
    run_pio_command(&args)
}

pub fn pio_monitor(
    project_dir: &str,
    environment: Option<&str>,
    port: Option<&str>,
    baud_rate: Option<u32>,
) -> fbuild_core::Result<()> {
    let baud_str;
    let mut args = vec!["device", "monitor", "-d", project_dir];
    if let Some(env) = environment {
        args.extend(["-e", env]);
    }
    if let Some(p) = port {
        args.extend(["--port", p]);
    }
    if let Some(b) = baud_rate {
        baud_str = b.to_string();
        args.extend(["--baud", &baud_str]);
    }
    run_pio_command(&args)
}
