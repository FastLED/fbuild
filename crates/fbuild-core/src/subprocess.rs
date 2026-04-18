//! Subprocess runner with timeout support.
//!
//! On Windows, uses CREATE_NO_WINDOW to prevent compiler processes from
//! spawning visible console windows.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use crate::{FbuildError, Result};

/// Output from a subprocess invocation.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl ToolOutput {
    /// True if the process exited with code 0.
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Run an external command and capture its output.
pub fn run_command(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }

    let mut cmd = build_command(args);

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    if let Some(vars) = env {
        for (k, v) in vars {
            cmd.env(k, v);
        }
    }

    if let Some(timeout) = timeout {
        run_with_timeout(cmd, timeout)
    } else {
        run_no_timeout(cmd)
    }
}

/// Build a `Command` with platform-specific settings.
///
/// On Windows: adds the executable's directory to PATH so child processes
/// (like GCC's cc1plus) can find shared libraries (libgcc_s_seh-1.dll, etc.)
/// that live alongside the main binary. Also strips MSYS/MSYS2 environment
/// variables when detected, to prevent interference with native Windows
/// toolchain binaries.
fn build_command(args: &[&str]) -> Command {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);

        // Add the executable's parent directory to PATH so that child processes
        // (e.g., cc1plus launched by g++) can find DLLs in the same bin/ dir.
        if let Some(exe_dir) = Path::new(args[0]).parent() {
            let exe_dir_str = exe_dir.to_string_lossy();
            if !exe_dir_str.is_empty() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{};{}", exe_dir_str, current_path));
            }
        }

        // Strip MSYS/MSYS2 environment variables that interfere with
        // native Windows toolchain binaries finding their internal tools.
        if is_msys_environment() {
            strip_msys_env(&mut cmd);
        }
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

/// Check if we're running inside an MSYS/MSYS2 environment.
#[cfg(windows)]
fn is_msys_environment() -> bool {
    std::env::var("MSYSTEM").is_ok() || std::env::var("MSYS").is_ok()
}

/// Strip MSYS-specific environment variables and rebuild PATH without MSYS dirs.
///
/// Matches Python's `get_pio_safe_env()` in `pio_env.py`: strips variables with
/// prefixes (MSYS*, MINGW*, CHERE*, ORIGINAL_PATH*), exact shell/terminal keys,
/// and PATH entries starting with "/" (MSYS-style paths).
#[cfg(windows)]
fn strip_msys_env(cmd: &mut Command) {
    // Prefixes to strip (matches Python's strip_prefixes)
    let strip_prefixes: &[&str] = &["MSYS", "MINGW", "CHERE", "ORIGINAL_PATH"];

    // Exact keys to strip (matches Python's strip_exact)
    let strip_exact: &[&str] = &[
        "SHELL",
        "SHLVL",
        "TERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "TMPDIR",
        "TMP",
        "TEMP",
        "_",
        "!",
        "POSIXLY_CORRECT",
        "EXECIGNORE",
        "HOSTTYPE",
        "MACHTYPE",
        "OSTYPE",
        "CONFIG_SITE",
    ];

    // Collect keys to remove (prefix-based + exact)
    let keys_to_remove: Vec<String> = std::env::vars()
        .filter_map(|(k, _)| {
            let should_strip = strip_prefixes.iter().any(|prefix| k.starts_with(prefix))
                || strip_exact.contains(&k.as_str());
            if should_strip {
                Some(k)
            } else {
                None
            }
        })
        .collect();

    for key in &keys_to_remove {
        cmd.env_remove(key);
    }

    // Clean PATH: remove MSYS-style entries (start with "/") and dirs containing msys/usr
    if let Ok(path) = std::env::var("PATH") {
        let filtered: Vec<&str> = path
            .split(';')
            .filter(|p| {
                // Remove MSYS-style paths (start with "/")
                if p.starts_with('/') {
                    return false;
                }
                let lower = p.to_lowercase();
                !lower.contains("\\msys") && !lower.contains("/msys") && !lower.contains("/usr/")
            })
            .collect();
        cmd.env("PATH", filtered.join(";"));
    }
}

fn run_no_timeout(mut cmd: Command) -> Result<ToolOutput> {
    // Route the spawn through the process-wide containment group so the
    // resulting child (and any grandchildren it forks) dies with the
    // daemon. Falls back to an uncontained spawn when the global group
    // is not initialised (CLI binary, unit tests). See FastLED/fbuild#32.
    let mut child = crate::containment::spawn_contained(&mut cmd)?;
    let stdout = child
        .stdout
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            buf
        })
        .unwrap_or_default();
    let stderr = child
        .stderr
        .take()
        .map(|mut s| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut buf).ok();
            buf
        })
        .unwrap_or_default();
    let status = child.wait()?;
    let output = Output {
        status,
        stdout,
        stderr,
    };
    Ok(output_to_tool_output(output))
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<ToolOutput> {
    // See `run_no_timeout`: route through containment.
    let mut child = crate::containment::spawn_contained(&mut cmd)?;

    let timeout_ms = timeout.as_millis() as u64;
    let start = std::time::Instant::now();

    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut s, &mut buf).ok();
                        buf
                    })
                    .unwrap_or_default();
                return Ok(ToolOutput {
                    stdout: String::from_utf8_lossy(&stdout).to_string(),
                    stderr: String::from_utf8_lossy(&stderr).to_string(),
                    exit_code: status.code().unwrap_or(-1),
                });
            }
            None => {
                if start.elapsed().as_millis() as u64 > timeout_ms {
                    let _ = child.kill();
                    return Err(FbuildError::Timeout(format!(
                        "command timed out after {}s",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn output_to_tool_output(output: Output) -> ToolOutput {
    ToolOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_echo() {
        let args = if cfg!(windows) {
            vec!["cmd", "/C", "echo", "hello"]
        } else {
            vec!["echo", "hello"]
        };
        let result = run_command(&args, None, None, None).unwrap();
        assert!(result.success());
        assert!(result.stdout.trim().contains("hello"));
    }

    #[test]
    fn run_nonexistent_command() {
        let result = run_command(&["nonexistent_command_xyz"], None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn run_empty_args() {
        let result = run_command(&[], None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn run_with_cwd() {
        let result = run_command(&["pwd"], Some(Path::new("/")), None, None);
        let _ = result;
    }

    #[test]
    fn tool_output_success() {
        let output = ToolOutput {
            stdout: "ok".to_string(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert!(output.success());

        let output = ToolOutput {
            stdout: String::new(),
            stderr: "error".to_string(),
            exit_code: 1,
        };
        assert!(!output.success());
    }
}
