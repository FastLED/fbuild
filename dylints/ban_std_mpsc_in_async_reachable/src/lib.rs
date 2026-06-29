#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, AmbigArg, Expr, ExprKind, Ty, TyKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::sync::mpsc::*` (channel, Sender, Receiver,
    /// SyncSender, sync_channel) in fbuild production code.
    ///
    /// ### Why is this bad?
    ///
    /// `std::sync::mpsc::Receiver::recv` blocks the calling OS
    /// thread. From inside async that's a worker — every other task
    /// on that worker stalls until a message arrives. The tokio
    /// equivalent (`tokio::sync::mpsc`) integrates with the reactor
    /// and is awaitable.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 4"). Per user
    /// directive this ships with **zero allowlist**.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let (tx, rx) = std::sync::mpsc::channel(); // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let (tx, rx) = fbuild_core::channel::bounded(64);
    /// // or unbounded:
    /// let (tx, rx) = fbuild_core::channel::unbounded();
    /// ```
    pub BAN_STD_MPSC_IN_ASYNC_REACHABLE,
    Deny,
    "ban std::sync::mpsc::* in fbuild production code"
}

/// Module prefix we match on (any item under `std::sync::mpsc::`).
const MPSC_PREFIX: &[&str] = &["std", "sync", "mpsc"];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanStdMpscInAsyncReachable {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        let filename = source_filename(cx, ty.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if let TyKind::Path(qpath) = ty.kind {
            let res = cx.qpath_res(&qpath, ty.hir_id);
            if res_is_under_mpsc(cx, res) {
                emit_lint(cx, ty.span);
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if let ExprKind::Path(qpath) = expr.kind {
            let res = cx.qpath_res(&qpath, expr.hir_id);
            if res_is_under_mpsc(cx, res) {
                emit_lint(cx, expr.span);
            }
        }
    }
}

fn res_is_under_mpsc(cx: &LateContext<'_>, res: Res) -> bool {
    match res {
        Res::Def(_, def_id) => def_path_starts_with(cx, def_id, MPSC_PREFIX),
        _ => false,
    }
}

fn def_path_starts_with(
    cx: &LateContext<'_>,
    def_id: rustc_hir::def_id::DefId,
    prefix: &[&str],
) -> bool {
    let def_path = cx.get_def_path(def_id);
    def_path.len() >= prefix.len()
        && def_path
            .iter()
            .take(prefix.len())
            .zip(prefix.iter())
            .all(|(actual, expected)| *actual == Symbol::intern(expected))
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_STD_MPSC_IN_ASYNC_REACHABLE,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`std::sync::mpsc::*` blocks the OS thread on `.recv()`. \
                 Use `fbuild_core::channel::{bounded, unbounded}` (tokio mpsc) \
                 so the channel integrates with the reactor and `.recv().await` \
                 yields to the runtime. See FastLED/fbuild#844.",
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
