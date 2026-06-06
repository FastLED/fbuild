//! Subprocess runner backed by `running-process`.
//!
//! Every synchronous spawn in fbuild flows through this module. We use
//! [`running_process::NativeProcess`] so that stdout and stderr are
//! drained concurrently from the moment the child starts — the manual
//! drain loop that preceded this module deadlocked the moment a compiler
//! filled its stderr pipe (see FastLED/fbuild#141).
//!
//! On Windows we still:
//!   * prepend the executable's directory to PATH so GCC's `cc1plus` can
//!     find its sibling DLLs; and
//!   * strip MSYS/MSYS2 env vars that would otherwise poison native
//!     Windows toolchain binaries.
//!
//! On Windows, containment is applied post-spawn by
//! `crate::containment::windows_job::assign` when the daemon has
//! installed the global containment group; CLI binaries and unit tests
//! run uncontained just as before. (Earlier code used a `containment:`
//! field on `ProcessConfig` from a pre-release `running-process-core`;
//! the published `running-process` 4.0 API does not expose that field —
//! see #32.)

use std::path::Path;
use std::time::Duration;

use running_process::{
    CommandSpec, NativeProcess, ProcessConfig, ProcessError, StderrMode, StdinMode,
};

use crate::{FbuildError, Result};

/// Build the env overlay that GCC link steps should pass to
/// [`run_command`] so that `lto-wrapper`'s temp files (the `*.ltrans*.o`
/// pieces it shuffles between partitions) land inside a fbuild-owned,
/// forward-slashed directory.
///
/// Why this exists — see FastLED/fbuild#261. On Windows hosts with an
/// MSYS/Git Bash shell, GCC's `lto-wrapper` emits a make rule whose
/// recipe shells out to `mv` to rename `*.ltrans*.o.tem` files. If the
/// temp path contains literal backslashes (the default `%USERPROFILE%`
/// resolution does), MSYS collapses them on the recipe line and `mv`
/// can't find the source file. Forcing TMPDIR/TMP/TEMP to a path that
/// already uses `/` sidesteps the issue.
///
/// The temp dir is created under `<build_dir>/.lto-tmp/` so it's
/// tracked by fbuild's existing build-dir cleanup — no `%TEMP%`
/// pollution.
///
/// Returns an owned `Vec<(String, String)>` so callers can build the
/// `&[(&str, &str)]` slice that `run_command` expects.
pub fn link_env_for_build(build_dir: &Path) -> std::io::Result<Vec<(String, String)>> {
    let lto_tmp = build_dir.join(".lto-tmp");
    std::fs::create_dir_all(&lto_tmp)?;
    // Forward-slash form so MSYS-flavored shells (used by the `mv` step
    // inside GCC's LTO wrapper recipe on Windows) don't lose the path
    // separators.
    let posix_path = lto_tmp.to_string_lossy().replace('\\', "/");
    Ok(vec![
        ("TMPDIR".to_string(), posix_path.clone()),
        ("TMP".to_string(), posix_path.clone()),
        ("TEMP".to_string(), posix_path),
    ])
}

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
    let config = build_config(args, cwd, env, /*capture=*/ true, StdinMode::Null)?;
    run_captured(config, args, timeout)
}

/// Run an external command, feed `stdin_bytes` to its stdin, and
/// capture stdout+stderr. Used by tools that operate on a payload
/// piped through a filter (e.g. `c++filt`, `clang-format`).
///
/// Routing through `NativeProcess` is critical for large stdin
/// payloads: the running-process reader thread drains stdout in
/// background while we write stdin, avoiding the Windows pipe-buffer
/// deadlock that hits when ~3k mangled symbols (~250 KB) saturate
/// the 4-8 KB stdout pipe before stdin EOF.
pub fn run_command_with_stdin(
    args: &[&str],
    stdin_bytes: &[u8],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    let config = build_config(args, cwd, env, /*capture=*/ true, StdinMode::Piped)?;
    let process = NativeProcess::new(config);
    process.start().map_err(|e| spawn_err(args, e))?;
    // Writes the data then closes stdin (signals EOF). The reader
    // threads spawned by `start()` keep stdout/stderr drained
    // throughout the write.
    if !stdin_bytes.is_empty() {
        process
            .write_stdin(stdin_bytes)
            .map_err(|e| FbuildError::Other(format!("stdin write to {:?} failed: {}", args, e)))?;
    } else {
        // Empty payload: close stdin immediately so the child sees EOF.
        let _ = process.close_stdin();
    }
    let exit_code = match process.wait(timeout) {
        Ok(code) => code,
        Err(ProcessError::Timeout) => {
            let _ = process.kill();
            return Err(FbuildError::Timeout(format!(
                "command timed out after {}s",
                timeout.map(|d| d.as_secs()).unwrap_or(0)
            )));
        }
        Err(e) => {
            return Err(FbuildError::Other(format!(
                "command {:?} failed: {}",
                args, e
            )))
        }
    };
    let stdout = join_lines(process.captured_stdout());
    let stderr = join_lines(process.captured_stderr());
    Ok(ToolOutput {
        stdout,
        stderr,
        exit_code,
    })
}

/// Run an external command with inherited stdin/stdout/stderr (no
/// capture). Intended for pass-through cases like the `pio` CLI
/// delegation where users expect the tool's live output.
///
/// Returns the exit code.
pub fn run_command_passthrough(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<i32> {
    let config = build_config(args, cwd, env, /*capture=*/ false, StdinMode::Inherit)?;
    let process = NativeProcess::new(config);
    process.start().map_err(|e| spawn_err(args, e))?;
    match process.wait(timeout) {
        Ok(code) => Ok(code),
        Err(ProcessError::Timeout) => {
            let _ = process.kill();
            Err(FbuildError::Timeout(format!(
                "command timed out after {}s",
                timeout.map(|d| d.as_secs()).unwrap_or(0)
            )))
        }
        Err(e) => Err(FbuildError::Other(format!(
            "command {:?} failed: {}",
            args, e
        ))),
    }
}

fn run_captured(
    config: ProcessConfig,
    args: &[&str],
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    let process = NativeProcess::new(config);
    process.start().map_err(|e| spawn_err(args, e))?;
    let exit_code = match process.wait(timeout) {
        Ok(code) => code,
        Err(ProcessError::Timeout) => {
            let _ = process.kill();
            return Err(FbuildError::Timeout(format!(
                "command timed out after {}s",
                timeout.map(|d| d.as_secs()).unwrap_or(0)
            )));
        }
        Err(e) => {
            return Err(FbuildError::Other(format!(
                "command {:?} failed: {}",
                args, e
            )))
        }
    };

    let stdout = join_lines(process.captured_stdout());
    let stderr = join_lines(process.captured_stderr());

    Ok(ToolOutput {
        stdout,
        stderr,
        exit_code,
    })
}

fn build_config(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    capture: bool,
    stdin_mode: StdinMode,
) -> Result<ProcessConfig> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }

    let argv: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();

    // Build the environment the child will see. Windows needs PATH
    // rewriting (prepend exe dir) and optional MSYS-var stripping; Unix
    // only needs overlay vars applied. When no changes are required
    // leave `env = None` so the child inherits the parent environment
    // verbatim (matching the pre-migration behaviour).
    let env_vec = compute_env(args[0], env);

    #[cfg(windows)]
    let creationflags = {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        Some(CREATE_NO_WINDOW)
    };
    #[cfg(not(windows))]
    let creationflags: Option<u32> = None;

    Ok(ProcessConfig {
        command: CommandSpec::Argv(argv),
        cwd: cwd.map(|p| p.to_path_buf()),
        env: env_vec,
        capture,
        stderr_mode: StderrMode::Pipe,
        creationflags,
        create_process_group: false,
        stdin_mode,
        nice: None,
    })
}

fn join_lines(lines: Vec<Vec<u8>>) -> String {
    // NativeProcess returns one Vec<u8> per line (CR/LF stripped). Join
    // with '\n' and add a trailing newline when non-empty so the result
    // matches the shape of the previous `String::from_utf8_lossy(&raw)`
    // output closely enough for downstream parsers (which mostly call
    // `.trim()` or `.lines()`).
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(line));
    }
    out.push('\n');
    out
}

fn spawn_err(args: &[&str], e: ProcessError) -> FbuildError {
    FbuildError::Other(format!("failed to spawn {:?}: {}", args, e))
}

/// Build the env vector to pass to the child.
///
/// * On Unix: when `overlay` is Some, merge it into the current
///   environment and return `Some(vec)`. Otherwise return `None` so the
///   child inherits `std::env::vars()` transparently.
/// * On Windows: always construct a full env vector when we need to
///   rewrite PATH (to prepend the exe directory) or strip MSYS vars.
///   This mirrors the behaviour of the pre-migration code, which called
///   `cmd.env("PATH", …)` and `cmd.env_remove(...)` on a command that
///   otherwise inherited the current env.
fn compute_env(program: &str, overlay: Option<&[(&str, &str)]>) -> Option<Vec<(String, String)>> {
    #[cfg(windows)]
    {
        let mut env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();

        // Prepend the executable's directory to PATH so that child
        // processes (e.g., cc1plus launched by g++) can find DLLs in
        // the same bin/ dir.
        if let Some(exe_dir) = Path::new(program).parent() {
            let exe_dir_str = exe_dir.to_string_lossy().to_string();
            if !exe_dir_str.is_empty() {
                let current_path = env_map
                    .get("PATH")
                    .or_else(|| env_map.get("Path"))
                    .cloned()
                    .unwrap_or_default();
                env_map.insert(
                    "PATH".to_string(),
                    format!("{};{}", exe_dir_str, current_path),
                );
            }
        }

        // Strip MSYS/MSYS2 environment variables that interfere with
        // native Windows toolchain binaries finding their internal
        // tools.
        if is_msys_environment(&env_map) {
            strip_msys_env(&mut env_map);
        }

        if let Some(vars) = overlay {
            for (k, v) in vars {
                env_map.insert((*k).to_string(), (*v).to_string());
            }
        }

        Some(env_map.into_iter().collect())
    }
    #[cfg(not(windows))]
    {
        let _ = program;
        match overlay {
            Some(vars) if !vars.is_empty() => {
                let mut env_map: std::collections::BTreeMap<String, String> =
                    std::env::vars().collect();
                for (k, v) in vars {
                    env_map.insert((*k).to_string(), (*v).to_string());
                }
                Some(env_map.into_iter().collect())
            }
            _ => None,
        }
    }
}

#[cfg(windows)]
fn is_msys_environment(env_map: &std::collections::BTreeMap<String, String>) -> bool {
    env_map.contains_key("MSYSTEM") || env_map.contains_key("MSYS")
}

/// Strip MSYS-specific environment variables and rebuild PATH without
/// MSYS dirs.
///
/// Matches Python's `get_pio_safe_env()` in `pio_env.py`: strips vars
/// with prefixes (MSYS*, MINGW*, CHERE*, ORIGINAL_PATH*), exact
/// shell/terminal keys, and PATH entries starting with "/" (MSYS-style
/// paths).
#[cfg(windows)]
fn strip_msys_env(env_map: &mut std::collections::BTreeMap<String, String>) {
    let strip_prefixes: &[&str] = &["MSYS", "MINGW", "CHERE", "ORIGINAL_PATH"];
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

    let keys_to_remove: Vec<String> = env_map
        .keys()
        .filter(|k| {
            strip_prefixes.iter().any(|prefix| k.starts_with(prefix))
                || strip_exact.contains(&k.as_str())
        })
        .cloned()
        .collect();
    for key in keys_to_remove {
        env_map.remove(&key);
    }

    // Clean PATH: remove MSYS-style entries (start with "/") and dirs
    // containing msys/usr.
    if let Some(path) = env_map.get("PATH").cloned() {
        let filtered: Vec<&str> = path
            .split(';')
            .filter(|p| {
                if p.starts_with('/') {
                    return false;
                }
                let lower = p.to_lowercase();
                !lower.contains("\\msys") && !lower.contains("/msys") && !lower.contains("/usr/")
            })
            .collect();
        env_map.insert("PATH".to_string(), filtered.join(";"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_echo() {
        let args = if cfg!(windows) {
            vec!["cmd", "/C", "echo hello"]
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
    fn link_env_for_build_creates_dir_and_returns_posix_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = link_env_for_build(tmp.path()).expect("link_env_for_build");

        // Directory created on disk.
        let lto_tmp = tmp.path().join(".lto-tmp");
        assert!(
            lto_tmp.is_dir(),
            ".lto-tmp must be a directory after the call"
        );

        // The three keys GCC/lto-wrapper look at, in TMPDIR > TMP > TEMP order.
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"TMPDIR"), "missing TMPDIR; got {keys:?}");
        assert!(keys.contains(&"TMP"), "missing TMP; got {keys:?}");
        assert!(keys.contains(&"TEMP"), "missing TEMP; got {keys:?}");

        // Each value must be forward-slashed and rooted under the
        // caller's build_dir — the whole point of the helper.
        for (k, v) in &env {
            assert!(
                !v.contains('\\'),
                "{k}={v:?} must not contain backslashes — see #261"
            );
            assert!(
                v.contains('/'),
                "{k}={v:?} must use forward slashes — see #261"
            );
            assert!(
                v.ends_with(".lto-tmp"),
                "{k}={v:?} must point at the .lto-tmp subdir under build_dir"
            );
        }
    }

    #[test]
    fn link_env_for_build_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = link_env_for_build(tmp.path()).expect("first call");
        let second = link_env_for_build(tmp.path()).expect("second call (dir already exists)");
        assert_eq!(first, second, "helper must be idempotent");
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

    #[test]
    fn run_captures_stderr() {
        // Verify that stderr is captured independently from stdout.
        let args = if cfg!(windows) {
            vec!["cmd", "/C", "echo err 1>&2"]
        } else {
            vec!["sh", "-c", "echo err 1>&2"]
        };
        let result = run_command(&args, None, None, None).unwrap();
        assert!(result.success());
        assert!(result.stderr.contains("err"), "got: {:?}", result);
    }
}
