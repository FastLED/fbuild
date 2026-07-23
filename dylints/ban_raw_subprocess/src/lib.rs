#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind, def::Res};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, symbol::Symbol};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `Command::spawn`, `Command::output`, and `Command::status`
    /// on `std::process::Command` and `tokio::process::Command` in
    /// fbuild production code (files under `crates/*/src/`).
    ///
    /// Catches both call shapes:
    ///   * Method-call:  `cmd.spawn()` / `cmd.output()` / `cmd.status()`
    ///   * Qualified path call:
    ///     `std::process::Command::spawn(&mut cmd)` /
    ///     `tokio::process::Command::output(&mut cmd)` etc.
    ///
    /// ### Why is this bad?
    ///
    /// Every child process fbuild launches must go through one of the
    /// blessed wrappers in `crates/fbuild-core/src/`:
    /// `subprocess::run_command` (sync, captures stdout/stderr via
    /// `running-process::NativeProcess` so the drain loop can't
    /// deadlock on a full pipe buffer — see #141),
    /// `containment::spawn_contained` /
    /// `containment::tokio_spawn::spawn_contained` (apply Windows Job
    /// Object containment + Linux per-child pgid + originator env), or
    /// `containment::spawn_detached` for the rare case where the child
    /// must outlive its launcher (daemon bootstrap).
    ///
    /// Bypassing them silently regresses one or more invariants. The
    /// dylint enforces the contract at lint time so the requirement
    /// can't drift back as new code is added.
    ///
    /// ### Known problems
    ///
    /// Allowlisting is file-level via `src/allowlist.txt`. The lint
    /// does not detect `#[cfg(test)]` scope programmatically — if a
    /// test file under `crates/*/src/` legitimately needs raw spawns
    /// (e.g. inline unit tests in the wrapper itself), add the file to
    /// the allowlist with a justification comment.
    ///
    /// Files outside `crates/*/src/` (integration tests under
    /// `crates/*/tests/`, examples under `crates/*/examples/`, benches
    /// under `crates/*/benches/`, build scripts, the `ci/` and
    /// `dylints/` trees) are out-of-scope by design.
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

/// Production-code scope. Only files whose path contains BOTH
/// `crates/` and `/src/` are linted. This intentionally excludes
/// `crates/*/tests/`, `crates/*/examples/`, `crates/*/benches/`,
/// `ci/`, `dylints/`, build scripts, and any other non-production
/// path. See #264 (CR blocker #2) for the prior over-broad scope bug.
const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanRawSubprocess {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        // Out-of-scope file → never fires.
        if !in_production_scope(&normalized) {
            return;
        }

        // Allowlisted file → exempt by configuration.
        if is_allowlisted(&normalized) {
            return;
        }

        match expr.kind {
            // Method-call shape: `cmd.spawn()` / `cmd.output()` /
            // `cmd.status()`. `type_dependent_def_id` returns the
            // canonical DefId of the resolved method, so re-exports
            // and aliases still resolve to the canonical
            // `std::process::Command::spawn` etc.
            ExprKind::MethodCall(_, _, _, _) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
                }
            }
            // Qualified-path call shape: `std::process::Command::spawn(&mut cmd)` /
            // `tokio::process::Command::output(&mut cmd)` / `<Command>::status(&mut cmd)`.
            // CR blocker #3 from #264: the prior prototype only
            // handled `ExprKind::MethodCall` and missed this entire
            // call shape. `qpath_res` is the resolver for any path
            // expression — including ones written as fully-qualified
            // calls and ones with explicit `<Type>::method` syntax.
            ExprKind::Path(ref qpath) => {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
                }
            }
            _ => {}
        }
    }
}

fn check_def_id(cx: &LateContext<'_>, span: rustc_span::Span, def_id: rustc_hir::def_id::DefId) {
    for banned in BANNED_METHOD_PATHS {
        if def_path_equals(cx, def_id, banned) {
            emit_lint(cx, span, banned);
            return;
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
                 `dylints/ban_raw_subprocess/src/allowlist.txt` with a one-line \
                 reason."
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

fn in_production_scope(normalized: &str) -> bool {
    // Require BOTH "crates/" and a subsequent "/src/" segment. The
    // `/src/` lookup is anchored after the `crates/` hit so a file
    // like `ci/src/foo.rs` wouldn't accidentally match.
    let Some(crates_at) = normalized.find(CRATES_PREFIX) else {
        return false;
    };
    let after_crates = &normalized[crates_at + CRATES_PREFIX.len()..];
    after_crates.contains(SRC_SEGMENT)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_production_scope_matches_src_files() {
        assert!(in_production_scope("crates/fbuild-core/src/subprocess.rs"));
        assert!(in_production_scope(
            "/home/runner/work/fbuild/crates/fbuild-cli/src/cli/build.rs"
        ));
        assert!(in_production_scope(
            "C:/Users/x/dev/fbuild/crates/fbuild-build/src/teensy/orchestrator.rs"
        ));
    }

    #[test]
    fn in_production_scope_rejects_non_src() {
        assert!(!in_production_scope(
            "crates/fbuild-cli/tests/lib_select.rs"
        ));
        assert!(!in_production_scope("crates/fbuild-build/examples/demo.rs"));
        assert!(!in_production_scope(
            "crates/fbuild-core/benches/bench_subprocess.rs"
        ));
        assert!(!in_production_scope("ci/find_direct_subprocess.py"));
        assert!(!in_production_scope(
            "dylints/ban_raw_subprocess/src/lib.rs"
        ));
        assert!(!in_production_scope("build.rs"));
    }

    #[test]
    fn is_allowlisted_matches_path_suffix() {
        // The real allowlist has its own entries; this just sanity-checks the
        // suffix-match logic against a small synthetic example.
        assert!(crate::is_allowlisted(
            "/anywhere/crates/fbuild-core/src/containment.rs"
        ));
    }
}
