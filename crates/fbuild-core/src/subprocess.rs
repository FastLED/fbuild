//! Subprocess runner — async-first (#813).
//!
//! Every subprocess spawn in fbuild flows through this module. The
//! primary surface is now `async fn`-shaped (`run_command`,
//! `run_command_with_stdin`, `run_command_passthrough`); a small set of
//! `_blocking` shims exist as the escape hatch for the handful of sync
//! call sites (CLI diagnostic subcommands, tests, etc.).
//!
//! Internally we spawn via [`tokio::process::Command`] routed through
//! [`crate::platform::process::spawn_tokio_contained`] so the daemon's
//! Job Object / `PR_SET_PDEATHSIG` containment still kills every child
//! when the daemon goes down. When no global containment group has been
//! installed (CLI binary, unit tests) the contained helper falls back
//! to a plain spawn — same coverage as the pre-async implementation.
//!
//! On Windows we still:
//!   * prepend the executable's directory to PATH so GCC's `cc1plus` can
//!     find its sibling DLLs; and
//!   * strip MSYS/MSYS2 env vars that would otherwise poison native
//!     Windows toolchain binaries.
//!
//! The legacy "what running-process gives us" output shape is preserved
//! byte-for-byte: stdout/stderr are returned as `String`s composed of
//! lossy-UTF-8 lines joined by `\n` with a trailing newline when
//! non-empty (see `join_lines`).

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command as TokioCommand};

use crate::platform::process;
use crate::{FbuildError, Result};

/// Default cap applied to every `run_command*` call that passes
/// `timeout: None`. Picked at 15 minutes so even the slowest real
/// production invocation (a cold ESP32 toolchain link + lto) finishes
/// well inside the budget, while a wedged child can no longer hang the
/// daemon thread forever (FastLED/fbuild#807, #802).
///
/// Override via the `FBUILD_SUBPROCESS_DEFAULT_TIMEOUT_SECS` env var:
///   * any positive integer → that many seconds
///   * `0` → no implicit cap (preserve historical `None` behaviour)
///   * unparseable / unset → 900 s (15 min)
///
/// Callers that need a longer, explicit budget should pass
/// `Some(Duration)` — the explicit value always wins over the default.
/// Callers that intentionally want no timeout at all (interactive
/// `pio` passthrough, long QEMU runs) should use
/// [`run_command_no_timeout`] / [`run_command_passthrough_no_timeout`]
/// so the choice is auditable.
const DEFAULT_SUBPROCESS_TIMEOUT_SECS: u64 = 900;

/// Sentinel passed internally to mean "actually run with no timeout".
/// `None` from a public caller is translated into the default cap
/// (see [`resolve_default_timeout`]); only the `*_no_timeout` helpers
/// thread this sentinel through.
const UNBOUNDED_TIMEOUT: Option<Duration> = None;

/// Resolve the effective timeout for a public `run_command*` call.
///
/// * `Some(d)` from the caller → use `d` verbatim (explicit override).
/// * `None` from the caller → apply the configured default cap
///   (controlled by `FBUILD_SUBPROCESS_DEFAULT_TIMEOUT_SECS`).
fn resolve_default_timeout(explicit: Option<Duration>) -> Option<Duration> {
    if let Some(d) = explicit {
        return Some(d);
    }
    default_subprocess_timeout()
}

fn default_subprocess_timeout() -> Option<Duration> {
    static CACHED: OnceLock<Option<Duration>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        match std::env::var("FBUILD_SUBPROCESS_DEFAULT_TIMEOUT_SECS") {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(0) => None, // explicit opt-out: behave like pre-#807
                Ok(secs) => Some(Duration::from_secs(secs)),
                Err(_) => Some(Duration::from_secs(DEFAULT_SUBPROCESS_TIMEOUT_SECS)),
            },
            Err(_) => Some(Duration::from_secs(DEFAULT_SUBPROCESS_TIMEOUT_SECS)),
        }
    })
}

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
    // separators. `NormalizedPath::display_slash()` owns the rewrite
    // (FastLED/fbuild#911 — see the workspace's `ban_manual_slash_normalize`
    // dylint).
    let posix_path = crate::path::NormalizedPath::from(lto_tmp).display_slash();
    Ok(vec![
        ("TMPDIR".to_string(), posix_path.clone()),
        ("TMP".to_string(), posix_path.clone()),
        ("TEMP".to_string(), posix_path),
    ])
}

/// Env overlay every per-TU compile MUST pass when dispatching through
/// the embedded zccache backend. See FastLED/fbuild#875.
///
/// Why this exists — zccache's `apply_client_env` treats a `Some(env)`
/// payload (even an empty one) as "client provided full env, clear the
/// daemon's inherited env." The previous call site at `compile_source`
/// passed `Vec::new()`, so gcc subprocesses were spawned with a literally
/// empty env. On Windows that broke compilation on the very first TU:
/// without `TMP`/`TEMP`/`USERPROFILE`/`LOCALAPPDATA`, the Windows
/// `GetTempPathW` fallback chain bottoms out at `C:\Windows\` (which
/// regular users can't write to), and gcc fails with
/// `Cannot create temporary file in C:\Windows\: Permission denied`
/// before producing a single `.o`.
///
/// This helper composes the minimum env a Windows compile subprocess
/// needs to find tempfiles + helper binaries (cc1/cc1plus/as) without
/// reintroducing host pollution that would hurt zccache hit rates:
///
/// - `TMPDIR` / `TMP` / `TEMP` → fbuild-owned scratch dir (mirrors
///   [`link_env_for_build`]).
/// - `PATH` → forwarded from the daemon so gcc's driver can find its
///   `cc1`/`as`/`ld` next to itself.
/// - `SystemRoot` → required by `kernel32.dll`'s loader on Windows.
/// - `USERPROFILE` / `LOCALAPPDATA` → `GetTempPathW` fallback chain.
/// - `PATHEXT` / `ComSpec` → shell-style program lookup compatibility.
///
/// On non-Windows hosts only the temp-dir pinning is exposed; gcc finds
/// everything else via the normal Unix conventions whether or not env is
/// forwarded.
///
/// `build_dir` should be the same per-build scratch root that
/// [`link_env_for_build`] uses, so per-build cleanup covers both halves.
pub fn compile_env_for_build(build_dir: &Path) -> std::io::Result<Vec<(String, String)>> {
    let tmp = build_dir.join(".compile-tmp");
    std::fs::create_dir_all(&tmp)?;
    let tmp_str = tmp.to_string_lossy().to_string();

    let mut env: Vec<(String, String)> = vec![
        ("TMPDIR".to_string(), tmp_str.clone()),
        ("TMP".to_string(), tmp_str.clone()),
        ("TEMP".to_string(), tmp_str),
    ];

    // Forward a small allowlist of host env vars so the compiler
    // driver can locate its sibling binaries (cc1/cc1plus/as), resolve
    // DLLs, and use the Windows GetTempPathW fallback chain. Missing
    // vars are silently skipped — they're absent on POSIX hosts and
    // the compile path doesn't need them there.
    const FORWARD: &[&str] = &[
        "PATH",
        "SystemRoot",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "PATHEXT",
        "ComSpec",
    ];
    for key in FORWARD {
        if let Ok(value) = std::env::var(key) {
            env.push(((*key).to_string(), value));
        }
    }
    Ok(env)
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
///
/// Async-first: callers in an async context should `.await` this.
/// Use [`run_command_blocking`] from sync contexts (CLI diagnostic
/// subcommands, tests).
///
/// Timeout policy: passing `timeout: None` applies the workspace
/// default cap (see `DEFAULT_SUBPROCESS_TIMEOUT_SECS` /
/// `FBUILD_SUBPROCESS_DEFAULT_TIMEOUT_SECS`). Pass `Some(Duration)` for
/// an explicit budget. For the (rare) legitimately-unbounded case use
/// [`run_command_no_timeout`].
pub async fn run_command(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    run_command_inner(args, cwd, env, resolve_default_timeout(timeout)).await
}

/// Explicitly-unbounded variant of [`run_command`]. Use ONLY for cases
/// where a timeout would be wrong (interactive `pio` passthrough, long
/// QEMU runs). Every call site is auditable — `grep` for
/// `run_command_no_timeout`.
pub async fn run_command_no_timeout(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
) -> Result<ToolOutput> {
    run_command_inner(args, cwd, env, UNBOUNDED_TIMEOUT).await
}

async fn run_command_inner(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }
    let mut cmd = build_command(
        args, cwd, env, /*capture=*/ true, /*stdin_piped=*/ false,
    )?;
    let child = process::spawn_tokio_contained(&mut cmd).map_err(|e| spawn_err(args, e))?;
    wait_and_capture(child, args, timeout).await
}

/// Run an external command, feed `stdin_bytes` to its stdin, and
/// capture stdout+stderr. Used by tools that operate on a payload
/// piped through a filter (e.g. `c++filt`, `clang-format`).
///
/// Concurrency-safe: stdin write, stdout drain, and stderr drain all
/// run on the tokio runtime concurrently — no risk of the Windows
/// pipe-buffer deadlock that hits when a multi-hundred-KB symbol
/// payload saturates the stdout pipe before stdin EOF.
///
/// Timeout policy: same as [`run_command`] — `None` applies the
/// workspace default cap.
pub async fn run_command_with_stdin(
    args: &[&str],
    stdin_bytes: &[u8],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    run_command_with_stdin_inner(
        args,
        stdin_bytes,
        cwd,
        env,
        resolve_default_timeout(timeout),
    )
    .await
}

async fn run_command_with_stdin_inner(
    args: &[&str],
    stdin_bytes: &[u8],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }
    let mut cmd = build_command(
        args, cwd, env, /*capture=*/ true, /*stdin_piped=*/ true,
    )?;
    let mut child = process::spawn_tokio_contained(&mut cmd).map_err(|e| spawn_err(args, e))?;

    // Take the stdin handle and concurrently write the payload while
    // tokio drains stdout/stderr in the background. Dropping `stdin`
    // closes the pipe (signals EOF) before we wait for the exit.
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = stdin_bytes.to_vec();
        let args_owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        // Spawn the writer as a sibling task so the read side of the
        // pipe can drain concurrently when we `wait_with_output` below.
        let write_task = tokio::spawn(async move {
            if !bytes.is_empty() {
                stdin.write_all(&bytes).await?;
            }
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, std::io::Error>(args_owned)
        });

        let output = wait_and_capture(child, args, timeout).await;

        // Surface a stdin-write error only if the command itself
        // succeeded — otherwise the command's own error is more useful.
        match write_task.await {
            Ok(Ok(_)) => output,
            Ok(Err(e)) => match output {
                Ok(_) => Err(FbuildError::Other(format!(
                    "stdin write to {:?} failed: {}",
                    args, e
                ))),
                Err(orig) => Err(orig),
            },
            Err(join_err) => match output {
                Ok(_) => Err(FbuildError::Other(format!(
                    "stdin writer task for {:?} panicked: {}",
                    args, join_err
                ))),
                Err(orig) => Err(orig),
            },
        }
    } else {
        // No stdin handle (extremely unlikely with `stdin(Stdio::piped())`)
        // — just wait for completion.
        wait_and_capture(child, args, timeout).await
    }
}

/// Run an external command with inherited stdin/stdout/stderr (no
/// capture). Intended for pass-through cases like the `pio` CLI
/// delegation where users expect the tool's live output.
///
/// Returns the exit code.
///
/// Timeout policy: same as [`run_command`]. Genuinely-unbounded
/// passthrough callers should use [`run_command_passthrough_no_timeout`].
pub async fn run_command_passthrough(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<i32> {
    run_command_passthrough_inner(args, cwd, env, resolve_default_timeout(timeout)).await
}

/// Explicitly-unbounded variant of [`run_command_passthrough`]. Use
/// ONLY for interactive CLI passthrough (e.g. `pio` delegation) where
/// the user, not fbuild, decides when the command is done.
pub async fn run_command_passthrough_no_timeout(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
) -> Result<i32> {
    run_command_passthrough_inner(args, cwd, env, UNBOUNDED_TIMEOUT).await
}

async fn run_command_passthrough_inner(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<i32> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }
    let mut cmd = build_command(
        args, cwd, env, /*capture=*/ false, /*stdin_piped=*/ false,
    )?;
    let mut child = process::spawn_tokio_contained(&mut cmd).map_err(|e| spawn_err(args, e))?;
    let status = match wait_with_timeout(&mut child, timeout).await? {
        Some(status) => status,
        None => {
            let _ = child.kill().await;
            return Err(FbuildError::Timeout(format!(
                "command timed out after {}s",
                timeout.map(|d| d.as_secs()).unwrap_or(0)
            )));
        }
    };
    Ok(exit_code_from(status))
}

// ---------------------------------------------------------------------------
// Sync bridges for one-shot callers (diagnostic CLI subcommands, tests).
// ---------------------------------------------------------------------------

/// Blocking variant of [`run_command`] for sync call sites.
///
/// Drives the async subprocess path from sync call sites.
///
/// Runs the future on a dedicated OS thread with a small current-thread tokio
/// runtime. Keeping the runtime on a fresh thread avoids nested-runtime panics
/// when daemon call sites use this sync bridge from async execution paths.
pub fn run_command_blocking(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    block_on(run_command(args, cwd, env, timeout))
}

/// How many times [`run_command_blocking_retrying_exec_busy`] will try to
/// spawn before giving up.
///
/// Three is enough for the window this exists to cross. The race is another
/// thread holding a writable descriptor across a `fork`, which lasts as long
/// as it takes that child to reach `exec` — microseconds, not milliseconds.
const EXEC_BUSY_ATTEMPTS: u32 = 3;

/// Base backoff between spawn attempts; multiplied by the attempt number.
const EXEC_BUSY_BACKOFF: Duration = Duration::from_millis(25);

/// Whether an OS spawn error is "the file is open for writing somewhere".
///
/// `ETXTBSY` on Unix. The kernel refuses to `exec` a file while any process
/// holds a writable descriptor for it — and "any process" includes a `fork`ed
/// child of this one that has not reached `exec` yet, which is how the race
/// happens without anyone writing to the file at all.
pub fn is_exec_busy(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::ExecutableFileBusy
}

/// [`run_command_blocking`], but retrying a spawn that fails with `ETXTBSY`.
///
/// Opt-in rather than the default for every subprocess in fbuild, because
/// retrying an exec is a behavior change and only some callers have the
/// write-then-exec shape that provokes it: a file this process just created
/// or extracted and is now running.
///
/// Only `ETXTBSY` is retried. Every other spawn error is returned on the
/// first attempt — a missing binary should fail immediately, not three times
/// slowly.
///
/// FastLED/fbuild#1366.
pub fn run_command_blocking_retrying_exec_busy(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    block_on(run_command_retrying_exec_busy(args, cwd, env, timeout))
}

/// Async form of [`run_command_blocking_retrying_exec_busy`].
pub async fn run_command_retrying_exec_busy(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    if args.is_empty() {
        return Err(FbuildError::Other("empty command".to_string()));
    }
    let timeout = resolve_default_timeout(timeout);
    let mut attempt = 1;
    loop {
        let mut cmd = build_command(
            args, cwd, env, /*capture=*/ true, /*stdin_piped=*/ false,
        )?;
        match process::spawn_tokio_contained(&mut cmd) {
            Ok(child) => return wait_and_capture(child, args, timeout).await,
            Err(error) if is_exec_busy(&error) && attempt < EXEC_BUSY_ATTEMPTS => {
                // `tokio::time::sleep`, not `std::thread::sleep`: this runs on
                // a tokio worker and blocking it would stall every other task
                // on that thread (FastLED/fbuild#844).
                tokio::time::sleep(EXEC_BUSY_BACKOFF * attempt).await;
                attempt += 1;
            }
            Err(error) => return Err(spawn_err(args, error)),
        }
    }
}

/// Blocking variant of [`run_command_with_stdin`].
pub fn run_command_with_stdin_blocking(
    args: &[&str],
    stdin_bytes: &[u8],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    block_on(run_command_with_stdin(args, stdin_bytes, cwd, env, timeout))
}

/// Blocking variant of [`run_command_passthrough`].
pub fn run_command_passthrough_blocking(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> Result<i32> {
    block_on(run_command_passthrough(args, cwd, env, timeout))
}

fn block_on<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect(
                        "failed to build single-threaded tokio runtime for blocking subprocess call",
                    );
                rt.block_on(fut)
            })
            .join()
            .expect("blocking subprocess runtime thread panicked")
    })
}

// ---------------------------------------------------------------------------
// Shared async plumbing
// ---------------------------------------------------------------------------

async fn wait_and_capture(
    child: Child,
    args: &[&str],
    timeout: Option<Duration>,
) -> Result<ToolOutput> {
    let wait_fut = child.wait_with_output();
    let output = match timeout {
        Some(d) => match tokio::time::timeout(d, wait_fut).await {
            Ok(res) => {
                res.map_err(|e| FbuildError::Other(format!("command {:?} failed: {}", args, e)))?
            }
            Err(_) => {
                return Err(FbuildError::Timeout(format!(
                    "command timed out after {}s",
                    d.as_secs()
                )));
            }
        },
        None => wait_fut
            .await
            .map_err(|e| FbuildError::Other(format!("command {:?} failed: {}", args, e)))?,
    };
    let exit_code = exit_code_from(output.status);
    Ok(ToolOutput {
        stdout: bytes_to_lines_string(&output.stdout),
        stderr: bytes_to_lines_string(&output.stderr),
        exit_code,
    })
}

async fn wait_with_timeout(
    child: &mut Child,
    timeout: Option<Duration>,
) -> Result<Option<std::process::ExitStatus>> {
    match timeout {
        Some(d) => match tokio::time::timeout(d, child.wait()).await {
            Ok(res) => res
                .map(Some)
                .map_err(|e| FbuildError::Other(format!("wait failed: {}", e))),
            Err(_) => Ok(None),
        },
        None => child
            .wait()
            .await
            .map(Some)
            .map_err(|e| FbuildError::Other(format!("wait failed: {}", e))),
    }
}

fn exit_code_from(status: std::process::ExitStatus) -> i32 {
    process::exit_code(status)
}

fn build_command(
    args: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(&str, &str)]>,
    capture: bool,
    stdin_piped: bool,
) -> Result<TokioCommand> {
    let mut cmd = TokioCommand::new(args[0]);
    if args.len() > 1 {
        cmd.args(&args[1..]);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    // Build the env overlay (Windows PATH rewriting / MSYS stripping,
    // and any explicit overlay vars). When `compute_env` returns `None`
    // the child inherits the parent env verbatim.
    if let Some(env_vec) = compute_env(args[0], env) {
        cmd.env_clear();
        for (k, v) in env_vec {
            cmd.env(k, v);
        }
    }

    // Stdio wiring.
    if capture {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    }
    if stdin_piped {
        cmd.stdin(Stdio::piped());
    } else if capture {
        // Same shape as the pre-async StdinMode::Null: detach stdin so
        // captured children don't accidentally read from the parent
        // terminal.
        cmd.stdin(Stdio::null());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    Ok(cmd)
}

fn spawn_err(args: &[&str], e: std::io::Error) -> FbuildError {
    FbuildError::Other(format!("failed to spawn {:?}: {}", args, e))
}

/// Format captured bytes the same way the old running-process path did:
/// split into lossy-UTF-8 lines (stripping CR/LF), join with '\n', and
/// add a trailing newline when non-empty. Downstream parsers rely on
/// this exact shape (most call `.trim()` / `.lines()` but a few search
/// for substrings — see #141).
fn bytes_to_lines_string(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let lossy = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(lossy.len());
    let mut first = true;
    for line in lossy.split('\n') {
        // The split by '\n' already drops trailing '\n'; strip a
        // trailing '\r' so CRLF input is collapsed to '\n'.
        let line = line.strip_suffix('\r').unwrap_or(line);
        if first {
            first = false;
        } else {
            out.push('\n');
        }
        out.push_str(line);
    }
    // The old `join_lines` always pushed a trailing newline when the
    // input was non-empty. Preserve that.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    } else if out.is_empty() {
        // Edge case: all input was a single empty line — match the old
        // behaviour which would return empty.
        return String::new();
    }
    out
}

/// PATH env overlay for a spawn whose program may be a bare tool name
/// (FastLED/fbuild#1234).
///
/// Absolute and relative-path programs don't consult PATH at spawn time,
/// so this returns `None` for them — provisioned tools stay untouched. A
/// bare name (a single normal path component: no separators, no Windows
/// drive prefix, no `.`/`..`) combined with a forwarded CLI caller PATH
/// yields a `[("PATH", ...)]` overlay for the [`run_command`] family, so
/// the spawn resolves against the caller's environment instead of the
/// long-lived daemon's boot-time PATH.
pub fn bare_name_path_overlay<'a>(
    program: &str,
    caller_path: Option<&'a str>,
) -> Option<Vec<(&'static str, &'a str)>> {
    let mut components = Path::new(program).components();
    let is_bare_name = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !is_bare_name {
        return None;
    }
    caller_path.map(|path| vec![("PATH", path)])
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
    process::command_environment(program, overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_exec_busy` must key on the OS condition, not on a message.
    ///
    /// Cheap and platform-independent: the classifier is what decides whether
    /// a spawn failure gets retried, so retrying the wrong error (a missing
    /// binary, say) would turn one fast failure into three slow ones.
    #[test]
    fn only_executable_file_busy_is_retryable() {
        use std::io::{Error, ErrorKind};
        assert!(is_exec_busy(&Error::from(ErrorKind::ExecutableFileBusy)));
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidInput,
            ErrorKind::WouldBlock,
        ] {
            assert!(
                !is_exec_busy(&Error::from(kind)),
                "{kind:?} must not be retried"
            );
        }
    }

    /// Whether this host enforces `ETXTBSY` on `exec`.
    ///
    /// Linux only, and that is not pedantry: Darwin lets a plain
    /// open-for-write coexist with `execve`, so the same code that reliably
    /// fails on Linux succeeds on macOS. Gating on `unix` would have made
    /// these tests fail there for a reason that is not a bug.
    fn host_enforces_exec_busy() -> bool {
        crate::platform::host::current().os() == crate::platform::host::HostOs::Linux
    }

    /// Build an executable no-op script and return its path.
    ///
    /// Uses the neutral `platform::fs` facade rather than a per-OS
    /// permissions extension trait, so this file stays free of raw host
    /// mechanics (the platform-boundary ledger, #1306).
    fn write_runnable_script(dir: &std::path::Path, name: &str) -> std::io::Result<String> {
        use std::io::Write;
        let script = dir.join(name);
        {
            let mut file = std::fs::File::create(&script)?;
            file.write_all(
                b"#!/bin/sh
exit 0
",
            )?;
        }
        crate::platform::fs::set_executable(&script)?;
        Ok(script.to_string_lossy().into_owned())
    }

    /// A held write handle blocks `exec`, and no amount of retrying helps.
    ///
    /// This is the mechanism behind FastLED/fbuild#1366 made reproducible.
    /// Linux refuses to `exec` a file while *any* process holds it open for
    /// writing — including this one — so the flake, which in CI came from a
    /// sibling thread's `fork` inheriting the descriptor, needs no thread
    /// timing to reproduce. Fully deterministic: the handle outlives the whole
    /// retry budget.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_write_handle_blocks_exec_for_the_whole_retry_budget() {
        if !host_enforces_exec_busy() {
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write_runnable_script(tmp.path(), "held_open.sh").expect("script");

        let held = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen for write");

        let result = run_command_retrying_exec_busy(&[path.as_str()], None, None, None).await;
        assert!(
            result.is_err(),
            "exec must fail while a writable handle is open, got {result:?}"
        );
        drop(held);
    }

    /// The retry earns its place: a handle released mid-window lets a later
    /// attempt through, where a single attempt would have failed.
    ///
    /// This is the property that actually fixes #1366 — the CI race window is
    /// the microseconds between another thread's `fork` and its `exec`, so a
    /// second attempt is all it takes. The 10 ms hold is deliberately tiny
    /// against the ~75 ms retry budget: a loaded runner can delay the release
    /// several times over and this still passes, so it cannot become the flake
    /// it exists to prevent.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_handle_released_mid_window_lets_the_retry_through() {
        if !host_enforces_exec_busy() {
            return;
        }
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = write_runnable_script(tmp.path(), "released.sh").expect("script");

        let held = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen for write");
        // A tokio task rather than a thread with `std::thread::sleep`, which
        // the workspace bans for blocking a runtime worker (#844).
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(held);
        });

        let result = run_command_retrying_exec_busy(&[path.as_str()], None, None, None).await;
        assert!(
            result.is_ok(),
            "a retry must outlast a transient writable handle, got {result:?}"
        );
    }

    #[tokio::test]
    async fn run_echo() {
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo hello"]
        } else {
            vec!["echo", "hello"]
        };
        let result = run_command(&args, None, None, None).await.unwrap();
        assert!(result.success());
        assert!(result.stdout.trim().contains("hello"));
    }

    #[tokio::test]
    async fn run_nonexistent_command() {
        let result = run_command(&["nonexistent_command_xyz"], None, None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_empty_args() {
        let result = run_command(&[], None, None, None).await;
        assert!(result.is_err());
    }

    /// FastLED/fbuild#1234: a bare tool name plus a forwarded caller PATH
    /// yields a PATH overlay; anything already resolved to a path — where
    /// the OS never consults PATH — must stay untouched.
    #[test]
    fn bare_name_path_overlay_applies_only_to_bare_names() {
        assert_eq!(
            bare_name_path_overlay("esptool", Some("/opt/tools/bin")),
            Some(vec![("PATH", "/opt/tools/bin")])
        );
        assert_eq!(
            bare_name_path_overlay("avrdude.exe", Some("/opt/tools/bin")),
            Some(vec![("PATH", "/opt/tools/bin")])
        );
        // Bare name without a caller PATH: keep legacy daemon-env behavior.
        assert_eq!(bare_name_path_overlay("esptool", None), None);
        // Absolute and relative paths never trigger a PATH lookup.
        let absolute = if crate::platform::host::is_windows() {
            crate::path::NormalizedPath::from(r"C:\tools\esptool")
        } else {
            crate::path::NormalizedPath::from("/usr/local/bin")
        }
        .join(crate::platform::executable::native_name("esptool"));
        assert_eq!(
            bare_name_path_overlay(&absolute.to_string_lossy(), Some("/opt/bin")),
            None
        );
        assert_eq!(bare_name_path_overlay("./esptool", Some("/opt/bin")), None);
        assert_eq!(
            bare_name_path_overlay("tools/esptool", Some("/opt/bin")),
            None
        );
    }

    /// FastLED/fbuild#1219: a `("PATH", ...)` env overlay must be the PATH
    /// the child actually sees. On Windows the parent env usually spells
    /// the variable "Path"; without the case-insensitive dedup in
    /// `compute_env` both spellings reach `Command::env` and the overlay
    /// value is silently discarded.
    #[tokio::test]
    async fn env_overlay_path_reaches_child_over_case_variant() {
        // `cmd` resolves via the OS system-dir fallback and `/bin/sh` is
        // absolute, so neither spawn depends on the clobbered PATH.
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo %PATH%"]
        } else {
            vec!["/bin/sh", "-c", "echo \"$PATH\""]
        };
        let overlay = [("PATH", "fbuild-1219-overlay-marker")];
        let result = run_command(&args, None, Some(&overlay), None)
            .await
            .unwrap();
        assert!(result.success());
        assert_eq!(result.stdout.trim(), "fbuild-1219-overlay-marker");
    }

    /// End-to-end proof for FastLED/fbuild#1219: a PATH overlay must make
    /// `run_command` resolve a bare executable name from the overlay's
    /// directory — not just deliver the PATH value to the child. Uses a
    /// uniquely named probe executable staged into a TempDir so ambient
    /// PATH can never satisfy the lookup.
    #[tokio::test]
    async fn run_command_resolves_bare_name_from_overlay_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        let probe = crate::platform::process::create_path_probe(dir).expect("create path probe");
        let probe_path = probe.path;

        let dir_str = dir.to_string_lossy();
        let overlay = [("PATH", dir_str.as_ref())];
        let bare_args: Vec<&str> = probe.bare_args.iter().map(String::as_str).collect();

        // Bare name + overlay → resolved from the overlay PATH.
        let result = run_command(
            &bare_args,
            None,
            Some(&overlay),
            Some(Duration::from_secs(30)),
        )
        .await
        .expect("bare-name spawn must resolve via the overlay PATH");
        assert!(result.success(), "got: {result:?}");
        assert!(
            result.stdout.contains("overlay-marker"),
            "stdout was {:?}",
            result.stdout
        );

        // Absolute path, no overlay → normal operation is unaffected.
        let probe_str = probe_path.to_string_lossy();
        let abs_args: Vec<&str> = if crate::platform::host::is_windows() {
            vec![probe_str.as_ref(), "/C", "echo overlay-marker"]
        } else {
            vec![probe_str.as_ref()]
        };
        let result = run_command(&abs_args, None, None, Some(Duration::from_secs(30)))
            .await
            .expect("absolute-path spawn works without any overlay");
        assert!(result.success(), "got: {result:?}");
        assert!(
            result.stdout.contains("overlay-marker"),
            "stdout was {:?}",
            result.stdout
        );

        // Bare name WITHOUT the overlay → not on ambient PATH → spawn
        // fails, proving the first resolution came from the overlay.
        let result = run_command(&bare_args, None, None, Some(Duration::from_secs(30))).await;
        assert!(
            result.is_err(),
            "bare name must not resolve from ambient PATH: {result:?}"
        );
    }

    #[test]
    fn run_command_blocking_works_from_sync_context() {
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo blocking"]
        } else {
            vec!["echo", "blocking"]
        };
        let result = run_command_blocking(&args, None, None, None).unwrap();
        assert!(result.success());
        assert!(result.stdout.trim().contains("blocking"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_command_blocking_works_from_tokio_worker() {
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo blocking-runtime"]
        } else {
            vec!["echo", "blocking-runtime"]
        };

        let result = tokio::spawn(async move { run_command_blocking(&args, None, None, None) })
            .await
            .expect("worker task must not panic")
            .expect("blocking subprocess call");

        assert!(result.success());
        assert!(result.stdout.trim().contains("blocking-runtime"));
    }

    #[test]
    fn run_command_blocking_works_inside_current_thread_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo current-thread-runtime"]
        } else {
            vec!["echo", "current-thread-runtime"]
        };

        let result = rt
            .block_on(async { run_command_blocking(&args, None, None, None) })
            .expect("blocking subprocess call");

        assert!(result.success());
        assert!(result.stdout.trim().contains("current-thread-runtime"));
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

    // FastLED/fbuild#875 regression: every compile spawn must carry
    // TMPDIR/TMP/TEMP pointing at an fbuild-owned dir so the gcc/clang
    // driver can write its preprocess / temp .o files without falling
    // back to a host path. On Windows that fallback bottoms out at
    // `C:\Windows\` and breaks the very first TU.
    #[test]
    fn compile_env_for_build_pins_tmp_keys_to_build_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = compile_env_for_build(tmp.path()).expect("compile_env_for_build");

        let compile_tmp = tmp.path().join(".compile-tmp");
        assert!(
            compile_tmp.is_dir(),
            ".compile-tmp must be created as a directory"
        );

        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for required in ["TMPDIR", "TMP", "TEMP"] {
            assert!(keys.contains(&required), "missing {required}; got {keys:?}");
        }

        for (k, v) in &env {
            if matches!(k.as_str(), "TMPDIR" | "TMP" | "TEMP") {
                // Production emits values via `NormalizedPath::display_slash()`
                // so `v` is already slash-normalized on every platform.
                assert!(
                    v.ends_with(".compile-tmp"),
                    "{k}={v:?} must point at the build_dir's .compile-tmp"
                );
            }
        }
    }

    #[test]
    fn compile_env_for_build_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = compile_env_for_build(tmp.path()).expect("first call");
        let second = compile_env_for_build(tmp.path()).expect("second call (dir already exists)");
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

    #[tokio::test]
    async fn run_captures_stderr() {
        // Verify that stderr is captured independently from stdout.
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo err 1>&2"]
        } else {
            vec!["sh", "-c", "echo err 1>&2"]
        };
        let result = run_command(&args, None, None, None).await.unwrap();
        assert!(result.success());
        assert!(result.stderr.contains("err"), "got: {:?}", result);
    }

    #[test]
    fn default_subprocess_timeout_constant_is_sane() {
        // Pinned by #807: the implicit cap must be long enough that
        // legitimate cold-cache builds finish (15 min is the agreed
        // budget from #802) and short enough that a wedged child does
        // not hang the daemon indefinitely.
        const _: () = assert!(DEFAULT_SUBPROCESS_TIMEOUT_SECS >= 60);
        const _: () = assert!(DEFAULT_SUBPROCESS_TIMEOUT_SECS <= 60 * 60);
    }

    #[test]
    fn resolve_default_timeout_passes_explicit_through() {
        let explicit = Duration::from_secs(5);
        assert_eq!(resolve_default_timeout(Some(explicit)), Some(explicit));
    }

    #[tokio::test]
    async fn run_command_default_timeout_fires_when_overridden_short() {
        // Bypass the cached `default_subprocess_timeout()` (which reads
        // the env var only once per process) by going through the
        // `_inner` path with a short explicit cap — that exercises the
        // same wait/kill code path the default would take.
        //
        // Use a portable long-running command:
        //   * Windows: `ping -n 30 127.0.0.1` waits ~29 s (sends 30
        //     pings 1 s apart) — far longer than our 200 ms cap.
        //   * Unix: `sleep 30`.
        let args = if crate::platform::host::is_windows() {
            vec!["ping", "-n", "30", "127.0.0.1"]
        } else {
            vec!["sleep", "30"]
        };
        let result = run_command_inner(&args, None, None, Some(Duration::from_millis(200))).await;
        match result {
            Err(FbuildError::Timeout(_)) => {} // expected
            other => panic!("expected Timeout, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn run_command_no_timeout_runs_fast_command() {
        // Audit-helper API still has to work for the legit unbounded
        // case — verify it returns Ok for a command that finishes
        // promptly.
        let args = if crate::platform::host::is_windows() {
            vec!["cmd", "/C", "echo no-timeout"]
        } else {
            vec!["echo", "no-timeout"]
        };
        let result = run_command_no_timeout(&args, None, None).await.unwrap();
        assert!(result.success());
        assert!(result.stdout.trim().contains("no-timeout"));
    }

    #[tokio::test]
    async fn run_command_with_stdin_pipes_payload() {
        // Round-trip: feed stdin → expect it back on stdout. `cat` on
        // unix, `findstr` on windows (matches everything via /R ".*").
        let args = if crate::platform::host::is_windows() {
            vec!["findstr", "/R", ".*"]
        } else {
            vec!["cat"]
        };
        let result = run_command_with_stdin(&args, b"hello world\n", None, None, None)
            .await
            .unwrap();
        assert!(result.success(), "got: {:?}", result);
        assert!(
            result.stdout.contains("hello world"),
            "stdout was {:?}",
            result.stdout
        );
    }
}
