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
    /// Bans `std::thread::sleep` in fbuild production code.
    ///
    /// ### Why is this bad?
    ///
    /// `std::thread::sleep` blocks the OS thread. Inside the tokio
    /// runtime that's a worker — every other task on that worker
    /// stalls until the sleep returns. Even outside async contexts,
    /// the consistent convention is `tokio::time::sleep` so
    /// switching a function from sync to async doesn't silently
    /// introduce a worker-blocking call.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 3"). Per user
    /// directive this ships with **zero allowlist** — every
    /// `std::thread::sleep` migrates in Phase 2.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// std::thread::sleep(Duration::from_millis(100)); // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// fbuild_core::time::sleep(Duration::from_millis(100)).await;
    /// // or, with a named constant:
    /// fbuild_core::time::sleep(fbuild_core::time::POLL_100MS).await;
    /// ```
    pub BAN_STD_THREAD_SLEEP,
    Deny,
    "ban std::thread::sleep — use fbuild_core::time::sleep instead"
}

const BANNED_PATHS: &[&[&str]] = &[&["std", "thread", "sleep"], &["std", "thread", "sleep_ms"]];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanStdThreadSleep {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_allowlisted(&normalized) {
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
            emit_lint(cx, span);
            return;
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_STD_THREAD_SLEEP,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`std::thread::sleep` blocks the OS thread (or a tokio worker). \
                 Use `fbuild_core::time::sleep(...).await` so the workspace has \
                 one consistent sleep surface — and so switching the calling \
                 function from sync to async doesn't silently introduce a \
                 worker-blocking call. See FastLED/fbuild#844.",
            );
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
    fn production_scope_matches() {
        assert!(in_production_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(!in_production_scope("crates/fbuild-cli/tests/integration.rs"));
    }
}
