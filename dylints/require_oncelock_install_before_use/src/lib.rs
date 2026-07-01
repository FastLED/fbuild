#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, Expr, ExprKind, HirId};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{
    symbol::{sym, Symbol},
    FileName, RemapPathScopeComponents,
};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::sync::OnceLock::set` and `OnceLock::set_blocking` in
    /// production code EXCEPT in:
    ///
    /// - `**/main.rs` (binary entry points)
    /// - `**/src/bin/**` (alternate binary entry points)
    /// - `**/tests/**` (integration tests)
    /// - any module annotated `#[cfg(test)]`
    ///
    /// ### Why is this bad?
    ///
    /// `OnceLock` is process-global single-assignment state. The only
    /// safe way to install a value is from a tightly-controlled call
    /// site that runs *before* any reader gets a chance — in practice,
    /// `main()` (or test setup). Any other site implies the
    /// order-of-init bug class: a reader has already filled the lock
    /// via `get_or_init` and the late `set` silently no-ops.
    ///
    /// FastLED/fbuild#840. The motivating analog from the Python
    /// fbuild was `paths.py`'s import-time `OnceLock`-equivalent
    /// capture of `FBUILD_DEV_MODE` running before `cli.py` mutated
    /// the variable. The Rust shape of the same bug is: someone calls
    /// `MY_GLOBAL.set(...)` from a non-`main` module, but another
    /// caller already populated the lock via `get_or_init` (or even
    /// just `.get()` early-readers triggered a load through some other
    /// path), so the late `.set(...)` returns `Err` and the wrong
    /// value sticks.
    ///
    /// Use `get_or_init` (lazy, idempotent), or install the value
    /// inside `main()` before any module reads it.
    ///
    /// ### Limitation (convention-based, not dataflow)
    ///
    /// This lint catches the *attempt* at the call site. It does NOT
    /// prove `get_or_init` ran first — that requires MIR-level
    /// dataflow across the whole crate graph and is tracked as a
    /// follow-up on #840 ("MIR for completeness later"). The
    /// convention is the fastest-landing prevention available today.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-something/src/foo.rs — banned
    /// static GLOBAL: OnceLock<Config> = OnceLock::new();
    /// pub fn configure(cfg: Config) {
    ///     let _ = GLOBAL.set(cfg); // banned outside main()
    /// }
    /// ```
    ///
    /// Use instead: install the value in `main()` before any module
    /// touches `GLOBAL.get()`, or rewrite the API around
    /// `get_or_init(|| ...)` so installation is lazy + idempotent.
    pub REQUIRE_ONCELOCK_INSTALL_BEFORE_USE,
    Deny,
    "require std::sync::OnceLock::set/set_blocking install sites to live in binary entry points (FastLED/fbuild#840)"
}

const BANNED_PATHS: &[&[&str]] = &[
    &["std", "sync", "OnceLock", "set"],
    &["std", "sync", "OnceLock", "set_blocking"],
];

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for RequireOncelockInstallBeforeUse {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_entry_point(&normalized) || is_test_file(&normalized) {
            return;
        }
        // `#[cfg(test)]` module walk: a `.set(..)` inside a unit-test
        // module is fine even when the surrounding file is production.
        if owned_by_cfg_test_module(cx, expr.hir_id) {
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
            ExprKind::MethodCall(_, _receiver, _, _) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
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
        REQUIRE_ONCELOCK_INSTALL_BEFORE_USE,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` is only allowed in `main.rs`, `src/bin/*.rs`, \
                 `#[cfg(test)]` modules, or `tests/**`. `OnceLock` is \
                 process-global single-assignment state — installing it \
                 from any other site is an order-of-init hazard (a reader \
                 may have already populated the lock via `get_or_init` \
                 and the late `.set(...)` silently no-ops). Either move \
                 the install to `main()` before any reader runs, or \
                 rewrite around `get_or_init(|| ...)` so installation is \
                 lazy and idempotent. See FastLED/fbuild#840."
            ));
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

fn is_entry_point(normalized: &str) -> bool {
    // `**/main.rs` or `**/src/bin/**`.
    normalized.ends_with("/main.rs")
        || normalized.contains("/src/bin/")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_points_are_exempt() {
        assert!(is_entry_point("crates/fbuild-cli/src/main.rs"));
        assert!(is_entry_point("crates/fbuild-daemon/src/bin/containment_harness.rs"));
        assert!(!is_entry_point("crates/fbuild-packages/src/library/library_manager.rs"));
    }

    #[test]
    fn test_files_are_exempt() {
        assert!(is_test_file("crates/fbuild-cli/tests/integration.rs"));
        assert!(is_test_file("crates/fbuild-core/src/subprocess_tests.rs"));
        assert!(!is_test_file("crates/fbuild-cli/src/main.rs"));
    }
}
