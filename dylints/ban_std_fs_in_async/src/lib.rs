#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::fs::*` calls in scopes where async is the rule —
    /// currently `crates/fbuild-daemon/src/**` production code. The
    /// matching bridge is `fbuild_core::fs::*` (re-export of
    /// `tokio::fs`) plus the `spawn_blocking` escape hatch for the
    /// rare case where a synchronous filesystem call from inside
    /// async is genuinely correct.
    ///
    /// ### Why is this bad?
    ///
    /// `std::fs::*` blocks the OS thread. Inside the tokio runtime
    /// that's a worker — every other task waiting on that worker
    /// stalls until the I/O completes. Multi-MB reads of toolchain
    /// caches can pin a worker for seconds.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 2").
    ///
    /// ### Known problems
    ///
    /// Detecting "is this expression reachable from an `async fn`"
    /// from inside dylint is non-trivial (a free function called
    /// from both sync and async contexts looks the same to the lint).
    /// Phase 1 ships a file-path-scoped variant that bans `std::fs::*`
    /// in `crates/fbuild-daemon/src/**` (where async is the
    /// convention). A follow-up will broaden detection workspace-wide
    /// via HIR analysis (TODO in this file).
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-daemon/src/handlers/build.rs
    /// async fn handle_build() {
    ///     let data = std::fs::read(path).unwrap(); // banned
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let data = fbuild_core::fs::read(path).await.unwrap();
    /// ```
    pub BAN_STD_FS_IN_ASYNC,
    Deny,
    "ban std::fs::* in scopes where async is the convention"
}

/// `std::fs` items that block the calling thread. Matched on the
/// canonical DefId path so `std::fs::*` resolves regardless of
/// `use std::fs` aliasing.
const BANNED_PATHS: &[&[&str]] = &[
    &["std", "fs", "canonicalize"],
    &["std", "fs", "copy"],
    &["std", "fs", "create_dir"],
    &["std", "fs", "create_dir_all"],
    &["std", "fs", "hard_link"],
    &["std", "fs", "metadata"],
    &["std", "fs", "read"],
    &["std", "fs", "read_dir"],
    &["std", "fs", "read_link"],
    &["std", "fs", "read_to_string"],
    &["std", "fs", "remove_dir"],
    &["std", "fs", "remove_dir_all"],
    &["std", "fs", "remove_file"],
    &["std", "fs", "rename"],
    &["std", "fs", "set_permissions"],
    &["std", "fs", "symlink_metadata"],
    &["std", "fs", "write"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// File-path scope. Phase 1 covers fbuild-daemon production code,
/// where async is the rule. Phase 2 (TODO via HIR analysis) widens
/// to "anywhere inside an `async fn`".
const SCOPE_PREFIX: &str = "crates/fbuild-daemon/src/";

impl<'tcx> LateLintPass<'tcx> for BanStdFsInAsync {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_scope(&normalized) {
            return;
        }
        if is_test_file(&normalized) || is_allowlisted(&normalized) {
            return;
        }

        match expr.kind {
            ExprKind::Path(ref qpath) => {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
                }
            }
            ExprKind::Call(ref func, _) => {
                if let ExprKind::Path(ref qpath) = func.kind {
                    if let Res::Def(_, def_id) = cx.qpath_res(qpath, func.hir_id) {
                        check_def_id(cx, expr.span, def_id);
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_def_id(cx: &LateContext<'_>, span: rustc_span::Span, def_id: rustc_hir::def_id::DefId) {
    for banned in BANNED_PATHS {
        if def_path_equals(cx, def_id, banned) {
            emit_lint(cx, span, banned);
            return;
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_STD_FS_IN_ASYNC,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` blocks the tokio worker. Use `fbuild_core::fs::*` \
                 (async) or wrap in `tokio::task::spawn_blocking` if a \
                 synchronous filesystem call is genuinely required. See \
                 FastLED/fbuild#844."
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

fn in_scope(normalized: &str) -> bool {
    normalized.contains(SCOPE_PREFIX)
}

fn is_test_file(normalized: &str) -> bool {
    let Some(name) = normalized.rsplit('/').next() else {
        return false;
    };
    name.contains("tests") && name.ends_with(".rs")
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
    fn daemon_paths_are_in_scope() {
        assert!(in_scope("crates/fbuild-daemon/src/context.rs"));
        assert!(in_scope("crates/fbuild-daemon/src/handlers/build.rs"));
    }

    #[test]
    fn other_crates_are_out_of_scope_for_now() {
        // Phase 1: only fbuild-daemon. Phase 2 widens via HIR analysis.
        assert!(!in_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(!in_scope("crates/fbuild-build/src/orchestrator.rs"));
    }
}
