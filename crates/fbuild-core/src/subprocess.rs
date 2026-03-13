//! Subprocess runner with timeout support.
//!
//! Rust's std::process::Command handles process management correctly —
//! no need for the Python workarounds (CREATE_NO_WINDOW, priority hacks, stdin stealing).

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

    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

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

fn run_no_timeout(mut cmd: Command) -> Result<ToolOutput> {
    let output: Output = cmd.output()?;
    Ok(output_to_tool_output(output))
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<ToolOutput> {
    let mut child = cmd.spawn()?;

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
        let result = run_command(&["echo", "hello"], None, None, None).unwrap();
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
