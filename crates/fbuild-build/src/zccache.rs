//! Optional zccache compiler cache integration.
//!
//! When zccache is found on PATH, compiler invocations are wrapped as
//! `zccache wrap <real-compiler> <args...>` so that repeated compilations
//! serve cached object files instead of re-invoking gcc/g++.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use fbuild_core::{FbuildError, Result};

/// Cached result of searching for zccache on PATH.
static ZCCACHE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// A persistent zccache fingerprint watch.
#[derive(Debug, Clone)]
pub struct FingerprintWatch {
    pub cache_file: PathBuf,
    pub root: PathBuf,
    pub extensions: Vec<String>,
    pub excludes: Vec<String>,
}

/// Result of a zccache fingerprint check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintCheck {
    Changed,
    Unchanged,
}

/// Find the zccache binary.
///
/// Resolution order:
/// 1. `FBUILD_NO_ZCCACHE` set → disabled (returns `None`).
/// 2. `FBUILD_ZCCACHE_BIN` → explicit binary path override (for local
///    zccache builds / debugging).
/// 3. fbuild's internal managed zccache — the pinned
///    [`crate::managed_zccache::MANAGED_ZCCACHE_VERSION`] downloaded into
///    `~/.fbuild/<mode>/bin/`. This is the default so the cache version is
///    decoupled from whatever (if anything) is installed in the ambient
///    Python environment.
/// 4. Offline/dev fallback: discover a `zccache` already present in the
///    active virtualenv, next to the executable, or on `PATH`.
pub fn find_zccache() -> Option<&'static Path> {
    ZCCACHE_PATH
        .get_or_init(|| {
            // Allow disabling zccache via environment variable
            if std::env::var_os("FBUILD_NO_ZCCACHE").is_some() {
                tracing::info!("zccache disabled via FBUILD_NO_ZCCACHE");
                return None;
            }

            // 1. Explicit override: a caller pointing at a specific binary
            //    (e.g. a local `cargo build` of zccache).
            if let Some(bin) = std::env::var_os("FBUILD_ZCCACHE_BIN") {
                let candidate = PathBuf::from(bin);
                if candidate.is_file() {
                    tracing::info!(
                        "using zccache from FBUILD_ZCCACHE_BIN at {}",
                        candidate.display()
                    );
                    return Some(candidate);
                }
                tracing::warn!(
                    "FBUILD_ZCCACHE_BIN set but not a file: {}",
                    candidate.display()
                );
            }

            // 2. Managed binary — fbuild owns a pinned zccache, decoupled
            //    from the ambient environment. This is the default path.
            match crate::managed_zccache::ensure() {
                Ok(path) => {
                    tracing::info!(
                        "using managed zccache {} at {}",
                        crate::managed_zccache::MANAGED_ZCCACHE_VERSION,
                        path.display()
                    );
                    return Some(path);
                }
                Err(e) => {
                    tracing::warn!(
                        "managed zccache unavailable ({e}); \
                         falling back to environment discovery"
                    );
                }
            }

            // 3. Offline/dev fallback.
            discover_env_zccache()
        })
        .as_deref()
}

/// Best-effort discovery of a `zccache` binary already present in the
/// environment (active virtualenv → ancestor `.venv` → sibling of the
/// current exe → `PATH`). Used only when the managed download is
/// unavailable, e.g. offline.
fn discover_env_zccache() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "zccache.exe"
    } else {
        "zccache"
    };

    // 1. Prefer the virtual environment's zccache (set by `uv run`)
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        let venv_dir = PathBuf::from(venv);
        let bin_dir = if cfg!(windows) {
            venv_dir.join("Scripts")
        } else {
            venv_dir.join("bin")
        };
        let candidate = bin_dir.join(exe_name);
        if candidate.is_file() {
            tracing::info!("found zccache in VIRTUAL_ENV at {}", candidate.display());
            return Some(candidate);
        }
    }

    // 2. Walk up from cwd looking for .venv (handles daemons without VIRTUAL_ENV)
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(found) = find_zccache_in_venv(&cwd, exe_name) {
            return Some(found);
        }
    }

    // 3. Sibling of current executable (normal package install)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                tracing::info!(
                    "found zccache next to executable at {}",
                    candidate.display()
                );
                return Some(candidate);
            }
        }
    }

    // 4. Fallback: search PATH
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var).find_map(|dir| {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                tracing::info!("found zccache on PATH at {}", candidate.display());
                Some(candidate)
            } else {
                None
            }
        })
    })
}

/// Walk up from `start` looking for a `.venv` directory containing zccache.
fn find_zccache_in_venv(start: &Path, exe_name: &str) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let venv = dir.join(".venv");
        if venv.is_dir() {
            let bin_dir = if cfg!(windows) {
                venv.join("Scripts")
            } else {
                venv.join("bin")
            };
            let candidate = bin_dir.join(exe_name);
            if candidate.is_file() {
                tracing::info!("found zccache in .venv at {}", candidate.display());
                return Some(candidate);
            }
        }
        dir = dir.parent()?;
    }
}

/// Start the zccache daemon if it's not already running.
///
/// This is idempotent — `zccache start` is a no-op when the daemon is up.
///
/// **No-op in embedded mode** (FastLED/fbuild#789 Phase 2 / #791): when
/// the embedded backend is the active global, fbuild owns the
/// in-process `ZccacheService` directly and the external `zccache`
/// daemon is not needed. Skipping the spawn here keeps embedded mode
/// from leaving an orphaned wrapper daemon behind.
pub fn ensure_running(zccache: &Path) -> Result<()> {
    #[cfg(feature = "embedded")]
    {
        if let Some(global) = crate::compile_backend::get_global() {
            if matches!(
                &global.backend,
                crate::compile_backend::CompileBackend::Embedded(_)
            ) {
                tracing::debug!(
                    "zccache::ensure_running skipped — embedded backend active"
                );
                return Ok(());
            }
        }
    }

    // INTENTIONALLY DETACHED (FastLED/fbuild#32): zccache is itself a
    // long-running daemon with independent lifecycle management. `start`
    // is a no-op when it's already running, and either way the zccache
    // daemon must survive the fbuild daemon — so this spawn stays out
    // of the containment group.
    // allow-direct-spawn: zccache daemon must outlive the fbuild daemon.
    let mut cmd = std::process::Command::new(zccache);
    cmd.arg("start")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) => {
            let message = format!(
                "failed to spawn zccache daemon via `{}` start: {}",
                zccache.display(),
                e
            );
            tracing::warn!("{message}");
            return Err(FbuildError::BuildFailed(message));
        }
    };

    if output.status.success() {
        tracing::info!("zccache daemon running");
        Ok(())
    } else if output_has_stale_daemon_error(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    ) {
        tracing::warn!("zccache daemon appears stale; stopping and retrying start");
        let _ = stop(zccache);
        std::thread::sleep(std::time::Duration::from_millis(250));

        let mut retry_cmd = std::process::Command::new(zccache);
        retry_cmd
            .arg("start")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            retry_cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let retry = retry_cmd.output().map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to spawn zccache daemon via `{}` start: {}",
                zccache.display(),
                e
            ))
        })?;
        if retry.status.success() {
            tracing::info!("zccache daemon running after stale-daemon recovery");
            Ok(())
        } else {
            let message = format_zccache_start_failure(
                zccache,
                retry.status.to_string(),
                &retry.stdout,
                &retry.stderr,
            );
            tracing::warn!("{message}");
            Err(FbuildError::BuildFailed(message))
        }
    } else {
        let message = format_zccache_start_failure(
            zccache,
            output.status.to_string(),
            &output.stdout,
            &output.stderr,
        );
        tracing::warn!("{message}");
        Err(FbuildError::BuildFailed(message))
    }
}

/// Stop the zccache daemon for this user, if one is running.
pub fn stop(zccache: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(zccache);
    cmd.arg("stop")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output().map_err(|e| {
        FbuildError::BuildFailed(format!(
            "failed to spawn zccache daemon via `{}` stop: {}",
            zccache.display(),
            e
        ))
    })?;

    if output.status.success() {
        Ok(())
    } else {
        Err(FbuildError::BuildFailed(format_zccache_start_failure(
            zccache,
            output.status,
            &output.stdout,
            &output.stderr,
        )))
    }
}

/// zccache can leave a previous-version daemon running across fbuild upgrades.
/// The new client then fails wrapped compiles with a protocol mismatch until
/// the old daemon is stopped.
pub fn output_has_protocol_mismatch(stdout: &str, stderr: &str) -> bool {
    stdout.contains("protocol version mismatch") || stderr.contains("protocol version mismatch")
}

pub fn output_has_stale_daemon_error(stdout: &str, stderr: &str) -> bool {
    output_has_protocol_mismatch(stdout, stderr)
        || stdout.contains("lost connection to daemon")
        || stderr.contains("lost connection to daemon")
        || stdout.contains("not accepting connections")
        || stderr.contains("not accepting connections")
}

fn format_zccache_start_failure(
    zccache: &Path,
    status: impl std::fmt::Display,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let mut message = format!("zccache start failed for {} ({status})", zccache.display());

    if !stderr.is_empty() {
        message.push_str(":\n");
        message.push_str(&stderr);
    }
    if !stdout.is_empty() {
        if stderr.is_empty() {
            message.push_str(":\n");
        } else {
            message.push('\n');
        }
        message.push_str("zccache stdout:\n");
        message.push_str(&stdout);
    }
    if stderr.is_empty() && stdout.is_empty() {
        message.push_str(" with no stdout/stderr; check fbuild daemon logs for details");
    }

    message
}

/// Prepend zccache explicit-wrap mode to a compiler command line.
///
/// Transforms `["gcc", "-c", "foo.c", ...]` into
/// `["zccache", "wrap", "gcc", "-c", "foo.c", ...]`.
/// If `cache_path` is None, returns the original args unchanged.
pub fn wrap_args(args: &[&str], cache_path: Option<&Path>) -> Vec<String> {
    match cache_path {
        Some(zcc) => {
            let mut wrapped = Vec::with_capacity(args.len() + 2);
            wrapped.push(zcc.to_string_lossy().to_string());
            wrapped.push("wrap".to_string());
            wrapped.extend(args.iter().map(|s| s.to_string()));
            wrapped
        }
        None => args.iter().map(|s| s.to_string()).collect(),
    }
}

/// Return the workspace root to use as the CWD for zccache-wrapped compiles.
///
/// Upstream zccache normalizes cache-key paths relative to the wrapper
/// process CWD. fbuild object files live under `<workspace>/.fbuild/...`, so
/// running the wrapper from `<workspace>` lets identical renamed workspaces
/// share per-TU cache keys even when compiler args contain absolute paths.
pub fn compile_cwd_from_output(output: &Path) -> Option<PathBuf> {
    let mut dir = output.parent()?;
    loop {
        if dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".fbuild"))
        {
            return dir.parent().map(|workspace| {
                canonicalize_existing_path(workspace).unwrap_or_else(|| workspace.to_path_buf())
            });
        }
        dir = dir.parent()?;
    }
}

/// Return a path argument that is stable relative to the zccache compile CWD.
///
/// macOS can canonicalize `/var/...` working directories to `/private/var/...`
/// inside the child process. Canonicalizing absolute compiler arguments before
/// stripping the compile CWD keeps zccache keys workspace-relative across both
/// path spellings.
pub fn path_arg_for_compile_cwd(path: &Path, cwd: &Path) -> String {
    if !path.is_absolute() {
        return path.to_string_lossy().to_string();
    }

    // Both ends must be in the same normal form (stripped of any `\\?\`
    // Windows extended-length prefix) for `strip_prefix` to match, since
    // `canonicalize_existing_path` now strips that prefix from `path`.
    let stable_path = canonicalize_existing_path(path).unwrap_or_else(|| path.to_path_buf());
    let stable_cwd = strip_unc_prefix(cwd.to_path_buf());
    let relative = stable_path
        .strip_prefix(&stable_cwd)
        .unwrap_or(&stable_path)
        .to_string_lossy()
        .to_string();
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    }
}

/// Normalize common path-bearing compiler flags for a zccache CWD.
pub fn normalize_flags_for_compile_cwd(flags: &[String], cwd: &Path) -> Vec<String> {
    let mut normalized = Vec::with_capacity(flags.len());
    let mut next_is_path = false;

    for flag in flags {
        if next_is_path {
            normalized.push(path_arg_for_compile_cwd(Path::new(flag), cwd));
            next_is_path = false;
            continue;
        }

        if flag_takes_path_argument(flag) {
            normalized.push(flag.clone());
            next_is_path = true;
            continue;
        }

        if let Some(value) = flag.strip_prefix("--sysroot=") {
            normalized.push(format!(
                "--sysroot={}",
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }

        if let Some((prefix, value)) = split_joined_path_flag(flag) {
            normalized.push(format!(
                "{}{}",
                prefix,
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }

        normalized.push(flag.clone());
    }

    normalized
}

fn canonicalize_existing_path(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(strip_unc_prefix(canonical));
    }

    let parent = path.parent()?.canonicalize().ok()?;
    let joined = match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    };
    Some(strip_unc_prefix(joined))
}

/// On Windows, `Path::canonicalize` returns paths prefixed with the
/// extended-length namespace marker `\\?\`. That prefix only works with
/// backslash path separators, but the response-file writer rewrites all
/// backslashes to forward slashes (so GCC doesn't interpret them as escape
/// sequences). The result is `//?/C:/…` which neither GCC nor mingw's path
/// search code understands as a valid drive-letter path, so `#include`
/// lookups against canonicalized include dirs silently fail (see FastLED
/// issue #2507 — `soc/soc_caps.h` not found on esp32p4).
///
/// Stripping the prefix here turns `\\?\C:\…` into `C:\…`, which survives
/// the backslash→slash rewrite as the standard `C:/…` form GCC accepts.
/// The trade-off is that long-path support (>260 chars) is lost for the
/// stripped paths, but the cache root is `<home>/.fbuild` which is well
/// under the limit in practice.
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    let s = path.to_string_lossy();
    // Handle both `\\?\C:\…` and (defensively) the already-rewritten `//?/C:/…`.
    if let Some(rest) = s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix("//?/")) {
        return PathBuf::from(rest);
    }
    path
}

fn flag_takes_path_argument(flag: &str) -> bool {
    matches!(
        flag,
        "-I" | "-isystem"
            | "-iquote"
            | "-idirafter"
            | "-include"
            | "-imacros"
            | "-isysroot"
            | "--sysroot"
    )
}

fn split_joined_path_flag(flag: &str) -> Option<(&'static str, &str)> {
    for prefix in [
        "-I",
        "-isystem",
        "-iquote",
        "-idirafter",
        "-include",
        "-imacros",
        "-isysroot",
    ] {
        if let Some(value) = flag.strip_prefix(prefix).filter(|value| !value.is_empty()) {
            return Some((prefix, value));
        }
    }
    None
}

/// Ask zccache whether the watched root changed since the last successful mark.
///
/// Exit code semantics come from `zccache fp check`:
/// - `0`: changed, build work should run
/// - `1`: unchanged, the watched root can be reused
///
/// **Embedded mode (FastLED/fbuild#789 Phase 3 / #792):** when the
/// embedded backend is active, routes through
/// [`crate::zccache_embedded::check_fingerprint_embedded`] which
/// drives the upstream `TwoLayerCache` directly. No `zccache fp
/// check` child process is spawned.
pub fn check_fingerprint(zccache: &Path, watch: &FingerprintWatch) -> Result<FingerprintCheck> {
    #[cfg(feature = "embedded")]
    {
        if let Some(global) = crate::compile_backend::get_global() {
            if matches!(
                &global.backend,
                crate::compile_backend::CompileBackend::Embedded(_)
            ) {
                use crate::zccache_embedded::EmbeddedFingerprintCheck;
                match crate::zccache_embedded::check_fingerprint_embedded(
                    &watch.cache_file,
                    &watch.root,
                    &watch.extensions,
                    &watch.excludes,
                ) {
                    Ok(EmbeddedFingerprintCheck::Changed) => {
                        return Ok(FingerprintCheck::Changed);
                    }
                    Ok(EmbeddedFingerprintCheck::Unchanged) => {
                        return Ok(FingerprintCheck::Unchanged);
                    }
                    Err(err) => {
                        tracing::warn!(
                            "embedded fingerprint check failed for {}: {err}; \
                             falling back to wrapper path",
                            watch.root.display(),
                        );
                        // Fall through to the wrapper-mode body below.
                    }
                }
            }
        }
    }

    if let Some(parent) = watch.cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut args = vec![
        zccache.to_string_lossy().to_string(),
        "fp".to_string(),
        "--cache-file".to_string(),
        watch.cache_file.to_string_lossy().to_string(),
        "check".to_string(),
        "--root".to_string(),
        watch.root.to_string_lossy().to_string(),
    ];
    for ext in &watch.extensions {
        args.push("--ext".to_string());
        args.push(ext.clone());
    }
    for exclude in &watch.excludes {
        args.push("--exclude".to_string());
        args.push(exclude.clone());
    }

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = fbuild_core::subprocess::run_command(&args_ref, None, None, None)?;
    match result.exit_code {
        0 => Ok(FingerprintCheck::Changed),
        1 => Ok(FingerprintCheck::Unchanged),
        code => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "zccache fp check failed for {} (exit={}): {}{}",
            watch.root.display(),
            code,
            result.stderr,
            result.stdout
        ))),
    }
}

/// Mark a previously checked watch as successful.
///
/// **Embedded mode (FastLED/fbuild#789 Phase 3 / #792):** when the
/// embedded backend is active, routes through
/// [`crate::zccache_embedded::mark_fingerprint_success_embedded`].
/// No `zccache fp mark-success` child process is spawned.
pub fn mark_fingerprint_success(zccache: &Path, watch: &FingerprintWatch) -> Result<()> {
    #[cfg(feature = "embedded")]
    {
        if let Some(global) = crate::compile_backend::get_global() {
            if matches!(
                &global.backend,
                crate::compile_backend::CompileBackend::Embedded(_)
            ) {
                match crate::zccache_embedded::mark_fingerprint_success_embedded(
                    &watch.cache_file,
                ) {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        tracing::warn!(
                            "embedded fingerprint mark-success failed for {}: {err}; \
                             falling back to wrapper path",
                            watch.root.display(),
                        );
                        // Fall through to the wrapper-mode body below.
                    }
                }
            }
        }
    }

    if let Some(parent) = watch.cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let args = [
        zccache.to_string_lossy().to_string(),
        "fp".to_string(),
        "--cache-file".to_string(),
        watch.cache_file.to_string_lossy().to_string(),
        "mark-success".to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = fbuild_core::subprocess::run_command(&args_ref, None, None, None)?;
    if result.success() {
        Ok(())
    } else {
        Err(fbuild_core::FbuildError::BuildFailed(format!(
            "zccache fp mark-success failed for {}: {}{}",
            watch.root.display(),
            result.stderr,
            result.stdout
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_cwd_from_output_uses_workspace_before_fbuild() {
        let output = Path::new("/work/project/.fbuild/build/env/release/src/main.o");

        assert_eq!(
            compile_cwd_from_output(output).as_deref(),
            Some(Path::new("/work/project"))
        );
    }

    #[test]
    fn compile_cwd_from_output_returns_none_without_fbuild_component() {
        let output = Path::new("/work/project/build/env/main.o");

        assert!(compile_cwd_from_output(output).is_none());
    }

    #[test]
    fn compile_cwd_from_output_canonicalizes_existing_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        let output = workspace.join(".fbuild/build/main.o");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        // On Windows, canonicalize() yields a `\\?\` extended-length prefix
        // that the response-file writer cannot use (forward-slash rewrite
        // produces `//?/` which gcc rejects). The helper now strips that
        // prefix on Windows, so the expected value must match.
        let expected = strip_unc_prefix(workspace.canonicalize().unwrap());

        assert_eq!(
            compile_cwd_from_output(&output).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_removes_extended_length_marker() {
        let raw = std::path::PathBuf::from(r"\\?\C:\Users\test\.fbuild\cache");
        let stripped = strip_unc_prefix(raw);
        assert_eq!(
            stripped,
            std::path::PathBuf::from(r"C:\Users\test\.fbuild\cache")
        );
    }

    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_is_idempotent_for_already_normal_paths() {
        let raw = std::path::PathBuf::from(r"C:\Users\test\include");
        let stripped = strip_unc_prefix(raw.clone());
        assert_eq!(stripped, raw);
    }

    #[test]
    fn zccache_start_failure_includes_stderr() {
        let message = format_zccache_start_failure(
            Path::new("/tools/zccache"),
            "exit status: 1",
            b"",
            b"zccache[err][D]: cannot start daemon",
        );

        assert!(message.contains("/tools/zccache"));
        assert!(message.contains("exit status: 1"));
        assert!(message.contains("zccache[err][D]: cannot start daemon"));
    }

    #[test]
    fn zccache_start_failure_includes_stdout_when_stderr_empty() {
        let message = format_zccache_start_failure(
            Path::new("/tools/zccache"),
            "exit status: 1",
            b"daemon process 123 exists but not accepting connections",
            b"",
        );

        assert!(message.contains("zccache stdout:"));
        assert!(message.contains("daemon process 123 exists but not accepting connections"));
    }

    #[test]
    fn zccache_start_failure_points_to_logs_when_output_empty() {
        let message =
            format_zccache_start_failure(Path::new("/tools/zccache"), "exit status: 1", b"", b"");

        assert!(message.contains("with no stdout/stderr"));
        assert!(message.contains("fbuild daemon logs"));
    }

    #[test]
    fn detects_zccache_protocol_mismatch_from_stderr_or_stdout() {
        let mismatch =
            "zccache[err][R]: broken connection to daemon: protocol error: protocol version mismatch: expected v16, received v15";

        assert!(output_has_protocol_mismatch("", mismatch));
        assert!(output_has_protocol_mismatch(mismatch, ""));
        assert!(output_has_stale_daemon_error(
            "",
            "zccache[err][R]: lost connection to daemon (no response)"
        ));
        assert!(!output_has_protocol_mismatch(
            "ordinary stdout",
            "ordinary stderr"
        ));
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_workspace_relative_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let source = cwd.join("src/main.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let expected = Path::new("src")
            .join("main.cpp")
            .to_string_lossy()
            .to_string();

        assert_eq!(path_arg_for_compile_cwd(&source, &cwd), expected);
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_dot_for_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();

        assert_eq!(path_arg_for_compile_cwd(&cwd, &cwd), ".");
    }

    #[test]
    fn normalize_flags_for_compile_cwd_rewrites_include_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let include = cwd.join("include");
        let vendor = cwd.join("vendor");
        let sysroot = cwd.join("sysroot");
        std::fs::create_dir_all(&include).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(&sysroot).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let flags = vec![
            "-I".to_string(),
            include.to_string_lossy().to_string(),
            "-I".to_string(),
            cwd.to_string_lossy().to_string(),
            format!("-I{}", vendor.display()),
            format!("-I{}", cwd.display()),
            format!("--sysroot={}", sysroot.display()),
        ];

        assert_eq!(
            normalize_flags_for_compile_cwd(&flags, &cwd),
            vec![
                "-I".to_string(),
                "include".to_string(),
                "-I".to_string(),
                ".".to_string(),
                "-Ivendor".to_string(),
                "-I.".to_string(),
                "--sysroot=sysroot".to_string(),
            ]
        );
    }
}
