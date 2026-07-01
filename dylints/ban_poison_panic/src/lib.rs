#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, Expr, ExprKind, HirId};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty;
use rustc_span::{
    symbol::{sym, Symbol},
    FileName, RemapPathScopeComponents,
};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Flags `.unwrap()` / `.expect(...)` on the `LockResult` /
    /// `TryLockResult` returned by `std::sync::Mutex::lock`,
    /// `std::sync::RwLock::read`, and `std::sync::RwLock::write`.
    ///
    /// ### Why is this bad?
    ///
    /// `LockResult::Err(_)` indicates the lock was poisoned because
    /// a previous holder panicked. `.unwrap()` then panics again —
    /// cascading the original panic across every other lock holder.
    /// In a daemon process that's an outage. Either:
    ///
    /// 1. switch to `tokio::sync::Mutex` / `RwLock` (they have no
    ///    poison concept and integrate with the reactor), or
    /// 2. handle the poison: `.unwrap_or_else(|e| e.into_inner())`.
    ///
    /// FastLED/fbuild#844 (bridge sweep, lint 9). Per user directive
    /// this ships with **zero allowlist**.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let guard = my_mutex.lock().unwrap(); // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// // Preferred: switch the type:
    /// let guard = my_mutex.lock().await; // tokio::sync::Mutex
    ///
    /// // Or handle poison explicitly:
    /// let guard = my_mutex.lock().unwrap_or_else(|e| e.into_inner());
    /// ```
    pub BAN_POISON_PANIC,
    Deny,
    "ban .unwrap()/.expect() on std::sync::Mutex/RwLock lock guards"
}

/// `.unwrap()` / `.expect()` on `Result`.
const UNWRAP_PATHS: &[&[&str]] = &[
    &["core", "result", "Result", "unwrap"],
    &["std", "result", "Result", "unwrap"],
    &["core", "result", "Result", "expect"],
    &["std", "result", "Result", "expect"],
];

/// Lock-returning methods we treat as "poison-prone receivers".
const LOCK_METHODS: &[&[&str]] = &[
    &["std", "sync", "poison", "mutex", "Mutex", "lock"],
    &["std", "sync", "mutex", "Mutex", "lock"],
    &["std", "sync", "poison", "rwlock", "RwLock", "read"],
    &["std", "sync", "poison", "rwlock", "RwLock", "write"],
    &["std", "sync", "rwlock", "RwLock", "read"],
    &["std", "sync", "rwlock", "RwLock", "write"],
    // try_* variants return TryLockResult which has the same poison surface.
    &["std", "sync", "poison", "mutex", "Mutex", "try_lock"],
    &["std", "sync", "mutex", "Mutex", "try_lock"],
];

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanPoisonPanic {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) || is_test_file(&normalized) {
            return;
        }
        if owned_by_cfg_test_module(cx, expr.hir_id) {
            return;
        }

        // We're looking for `<receiver>.unwrap()` / `.expect(...)` where
        // `<receiver>` is a `LockResult` / `TryLockResult` produced by
        // one of `LOCK_METHODS`.
        let ExprKind::MethodCall(_seg, recv, _args, _) = expr.kind else {
            return;
        };
        let Some(unwrap_def) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
            return;
        };
        if !UNWRAP_PATHS.iter().any(|p| def_path_equals(cx, unwrap_def, p)) {
            return;
        }

        // The receiver must itself be a method-call to one of LOCK_METHODS.
        if !receiver_is_lock_call(cx, recv) {
            return;
        }

        emit_lint(cx, expr.span);
    }
}

fn receiver_is_lock_call(cx: &LateContext<'_>, recv: &Expr<'_>) -> bool {
    match recv.kind {
        ExprKind::MethodCall(_, _, _, _) => {
            let Some(def_id) = cx.typeck_results().type_dependent_def_id(recv.hir_id) else {
                return false;
            };
            LOCK_METHODS.iter().any(|p| def_path_equals(cx, def_id, p))
        }
        // `Mutex::lock(&m).unwrap()` — qualified-path call.
        ExprKind::Call(func, _) => {
            if let ExprKind::Path(ref qpath) = func.kind {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, func.hir_id) {
                    return LOCK_METHODS.iter().any(|p| def_path_equals(cx, def_id, p));
                }
            }
            false
        }
        _ => {
            // Fallback by *type*: if the receiver's type resolves to
            // `LockResult<...>` we still flag, even if the syntactic
            // shape is unusual (e.g. assigned via a `let`).
            let ty = cx.typeck_results().expr_ty_adjusted(recv);
            ty_is_lock_result(ty)
        }
    }
}

fn ty_is_lock_result(ty: ty::Ty<'_>) -> bool {
    let s = format!("{ty:?}");
    s.contains("LockResult") || s.contains("TryLockResult")
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_POISON_PANIC,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`.unwrap()`/`.expect()` on a `LockResult` cascades the \
                 original lock-holder panic across every other lock holder. \
                 Either switch to `tokio::sync::Mutex` / `RwLock` (no poison \
                 concept, integrates with the reactor) or handle the poison \
                 with `.unwrap_or_else(|e| e.into_inner())`. See \
                 FastLED/fbuild#844.",
            );
        }),
    );
}

fn owned_by_cfg_test_module(cx: &LateContext<'_>, hir_id: HirId) -> bool {
    std::iter::once(hir_id)
        .chain(cx.tcx.hir_parent_id_iter(hir_id))
        .any(|id| {
            cx.tcx.hir_attrs(id).iter().any(attr_is_cfg_test) || is_test_module_node(cx, id)
        })
}

fn is_test_module_node(cx: &LateContext<'_>, hir_id: HirId) -> bool {
    let rustc_hir::Node::Item(item) = cx.tcx.hir_node(hir_id) else {
        return false;
    };
    let rustc_hir::ItemKind::Mod(ident, _) = item.kind else {
        return false;
    };
    let name = ident.name.as_str();
    name == "tests" || name.ends_with("_tests") || name.ends_with("_test")
}

fn attr_is_cfg_test(attr: &rustc_hir::Attribute) -> bool {
    if !attr.has_name(sym::cfg) {
        return false;
    }
    let Some(list) = attr.meta_item_list() else {
        return false;
    };
    list.iter().any(|nested| {
        nested
            .ident()
            .map(|id| id.as_str() == "test")
            .unwrap_or(false)
    })
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

fn is_test_file(normalized: &str) -> bool {
    if normalized.contains("/tests/") {
        return true;
    }
    let Some(name) = normalized.rsplit('/').next() else {
        return false;
    };
    name.contains("tests") && name.ends_with(".rs")
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
