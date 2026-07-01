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
    /// Bans `tokio::runtime::Runtime::new`,
    /// `tokio::runtime::Builder::*`, and friends in production code
    /// EXCEPT in:
    ///
    /// - `**/main.rs` (binary entry points)
    /// - `**/src/bin/**` (alternate binary entry points)
    /// - `**/tests/**` (integration tests)
    /// - any module annotated `#[cfg(test)]`
    /// - the workspace root `examples/` / `benches/`
    ///
    /// ### Why is this bad?
    ///
    /// Spinning up a fresh runtime mid-program is almost always a
    /// shape mismatch — the calling site is already inside the
    /// daemon's tokio runtime, so the new runtime fights for the same
    /// OS-thread pool and the `block_on` call panics if the outer
    /// future is on a current-thread executor. The library audit in
    /// #844 found 8 such sites; all 8 should restructure to either
    /// (a) be async themselves, (b) take a runtime handle from their
    /// caller, or (c) explicitly use `tokio::task::spawn_blocking`.
    ///
    /// FastLED/fbuild#844 (bridge sweep, lint 8). Per user directive
    /// this ships with **zero allowlist**.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-packages/src/library/library_manager.rs
    /// fn install_library(...) -> Result<()> {
    ///     let rt = tokio::runtime::Runtime::new()?; // banned
    ///     rt.block_on(async { ... })
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// pub async fn install_library(...) -> Result<()> {
    ///     // ... call .await directly
    /// }
    /// ```
    pub BAN_RUNTIME_NEW_OUTSIDE_MAIN,
    Deny,
    "ban tokio::runtime::Runtime construction outside binary entry points"
}

const BANNED_PATHS: &[&[&str]] = &[
    &["tokio", "runtime", "Runtime", "new"],
    &["tokio", "runtime", "Builder", "new_current_thread"],
    &["tokio", "runtime", "Builder", "new_multi_thread"],
];

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanRuntimeNewOutsideMain {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_entry_point(&normalized) || is_test_file(&normalized) {
            return;
        }
        // `#[cfg(test)]` module walk: a runtime built in a unit-test
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
        BAN_RUNTIME_NEW_OUTSIDE_MAIN,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` is only allowed in `main.rs`, `src/bin/*.rs`, \
                 `#[cfg(test)]` modules, or `tests/**`. Restructure the \
                 calling function to be `async fn` (and await directly), \
                 accept a `tokio::runtime::Handle` from its caller, or use \
                 `tokio::task::spawn_blocking`. See FastLED/fbuild#844."
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
