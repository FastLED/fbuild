#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans method calls `Command::spawn`, `Command::output`, and
    /// `Command::status` on `std::process::Command` and
    /// `tokio::process::Command` in fbuild production code.
    ///
    /// ### Why is this bad?
    ///
    /// Every child process fbuild launches must go through one of the
    /// blessed wrappers in `crates/fbuild-core/src/`:
    /// `subprocess::run_command` (sync, captures stdout/stderr via
    /// `running-process-core::NativeProcess` so the drain loop can't
    /// deadlock on a full pipe buffer — see #141),
    /// `containment::spawn_contained` /
    /// `containment::tokio_spawn::spawn_contained` (apply Windows Job
    /// Object containment + Linux per-child pgid + originator env), or
    /// `containment::spawn_detached` for the rare case (daemon itself)
    /// where the child must outlive its launcher.
    ///
    /// Bypassing them silently regresses one or more invariants. The
    /// dylint enforces the contract at lint time.
    ///
    /// ### Known problems
    ///
    /// Allowlisting is file-level (`src/allowlist.txt`). The lint does
    /// not detect `#[cfg(test)]` scope programmatically — if a test
    /// file legitimately needs raw spawns, add the file to the
    /// allowlist with a justification comment.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let mut cmd = std::process::Command::new("rustc");
    /// cmd.args(["--version"]);
    /// let output = cmd.output().unwrap();
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let output = fbuild_core::subprocess::run_command(
    ///     &["rustc", "--version"],
    ///     None,
    ///     None,
    ///     None,
    /// )?;
    /// ```
    pub BAN_RAW_SUBPROCESS,
    Deny,
    "ban raw Command::{spawn, output, status} in fbuild production code"
}

/// Each entry is a fully-qualified path to a banned method. Matching is
/// exact. We list `std::process::Command::*` and
/// `tokio::process::Command::*` separately — they are distinct types
/// with distinct DefIds — and intentionally omit other methods on
/// `Command` (e.g. `args`, `env`, `current_dir`) and on `Child` (e.g.
/// `wait_with_output`, `kill`). The bug class is at *spawn time*.
const BANNED_METHOD_PATHS: &[&[&str]] = &[
    &["std", "process", "Command", "spawn"],
    &["std", "process", "Command", "output"],
    &["std", "process", "Command", "status"],
    &["tokio", "process", "Command", "spawn"],
    &["tokio", "process", "Command", "output"],
    &["tokio", "process", "Command", "status"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Only files under `crates/*/src/` are in scope. Other code paths
/// (dylints themselves, build scripts, vendored deps) have their own
/// discipline and shouldn't be linted by this rule.
const SOURCE_PREFIX: &str = "crates/";

impl<'tcx> LateLintPass<'tcx> for BanRawSubprocess {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        // Out-of-scope file → never fires.
        if !normalized.contains(SOURCE_PREFIX) {
            return;
        }

        // Allowlisted file → exempt by configuration.
        if is_allowlisted(&normalized) {
            return;
        }

        // Method call on a `Command`? Resolve to the canonical DefId
        // and compare to each banned path.
        if let ExprKind::MethodCall(_segment, _receiver, _args, _span) = expr.kind {
            if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                for banned in BANNED_METHOD_PATHS {
                    if def_path_equals(cx, def_id, banned) {
                        emit_lint(cx, expr.span, banned);
                        return;
                    }
                }
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_RAW_SUBPROCESS,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` bypasses fbuild's spawn discipline; route through \
                 `fbuild_core::subprocess::run_command` (sync, capture) or \
                 `fbuild_core::containment::spawn_contained` / `spawn_detached` / \
                 `tokio_spawn::spawn_contained` so containment, drain semantics, \
                 and originator-env propagation are applied. If raw spawn is \
                 truly justified for this file, allowlist it in \
                 `dylints/ban_raw_subprocess/src/allowlist.txt`."
            ));
        }),
    );
}

fn source_filename(cx: &LateContext<'_>, span: rustc_span::Span) -> String {
    match cx.sess().source_map().span_to_filename(span) {
        FileName::Real(real_filename) => real_filename
            .local_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real_filename
                    .path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        filename => filename
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    }
}

fn is_allowlisted(normalized: &str) -> bool {
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|allowed| normalized.ends_with(allowed))
}

fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

fn def_path_equals(
    cx: &LateContext<'_>,
    def_id: rustc_hir::def_id::DefId,
    expected: &[&str],
) -> bool {
    let def_path = cx.get_def_path(def_id);
    if def_path.len() != expected.len() {
        return false;
    }
    def_path
        .iter()
        .zip(expected.iter())
        .all(|(actual, expected_segment)| *actual == Symbol::intern(expected_segment))
}
