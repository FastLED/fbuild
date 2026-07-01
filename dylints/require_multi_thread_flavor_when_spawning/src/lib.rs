#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_ast::ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::{
    def::Res,
    def_id::LocalDefId,
    intravisit::{walk_expr, Visitor},
    Attribute, Body, Expr, ExprKind, FnDecl,
};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::hir::nested_filter;
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Flags `async fn` items annotated with `#[tokio::test]` (or
    /// `#[tokio::test(...)]` *without* `flavor = "multi_thread"`)
    /// whose body calls `tokio::spawn(...)` anywhere.
    ///
    /// ### Why is this bad?
    ///
    /// The default `tokio::test` flavor is `current_thread`, which
    /// runs every spawned task on the same executor thread as the
    /// test itself. If the test awaits something the spawned task
    /// produces (e.g. a channel send), the test deadlocks under load
    /// — and runs serially otherwise. That's the exact wrong shape
    /// for testing concurrent code.
    ///
    /// The fix is one line:
    ///
    /// ```rust,ignore
    /// #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    /// async fn my_test() {
    ///     tokio::spawn(async { ... });
    ///     // ...
    /// }
    /// ```
    ///
    /// ### Known problems
    ///
    /// - Only the function body is inspected lexically — the lint
    ///   does not follow calls into helper functions that themselves
    ///   call `tokio::spawn`. If you wrap spawn in a helper, the test
    ///   itself will not be flagged.
    /// - The lint trusts the attribute's literal `flavor` string —
    ///   it does not resolve constants or macros.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// #[tokio::test]                  // ← flagged: missing flavor
    /// async fn test_streaming() {
    ///     let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    ///     tokio::spawn(async move { tx.send(()).await.unwrap(); });
    ///     rx.recv().await;
    /// }
    /// ```
    pub REQUIRE_MULTI_THREAD_FLAVOR_WHEN_SPAWNING,
    Deny,
    "require tokio::test(flavor = \"multi_thread\") when the test body calls tokio::spawn"
}

const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for RequireMultiThreadFlavorWhenSpawning {
    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: rustc_hir::intravisit::FnKind<'tcx>,
        _decl: &'tcx FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        span: Span,
        def_id: LocalDefId,
    ) {
        // Only async fns are candidates — `#[tokio::test]` only goes
        // on async fns.
        if !is_async_fn(&kind) {
            return;
        }

        let filename = source_filename(cx, span);
        let normalized = normalize_slashes(&filename);
        if is_allowlisted(&normalized) {
            return;
        }

        let hir_id = cx.tcx.local_def_id_to_hir_id(def_id);
        let attrs = cx.tcx.hir_attrs(hir_id);

        let Some(test_attr) = find_tokio_test_attr(attrs) else {
            return;
        };

        if attr_has_multi_thread_flavor(test_attr) {
            return;
        }

        // Walk the body looking for `tokio::spawn(...)` calls.
        let mut visitor = SpawnFinder {
            cx,
            found_at: None,
        };
        visitor.visit_expr(body.value);

        if let Some(spawn_span) = visitor.found_at {
            emit_lint(cx, span, spawn_span);
        }
    }
}

fn is_async_fn(kind: &rustc_hir::intravisit::FnKind<'_>) -> bool {
    use rustc_hir::intravisit::FnKind;
    let header = match kind {
        FnKind::ItemFn(_, _, header) => header,
        FnKind::Method(_, sig) => &sig.header,
        FnKind::Closure => return false,
    };
    matches!(
        header.asyncness,
        rustc_hir::IsAsync::Async(_)
    )
}

/// Locate an attribute whose path ends in `test` and whose preceding
/// segments contain `tokio`. We accept `#[tokio::test]`,
/// `#[::tokio::test]`, and `#[tokio::test(...)]`.
fn find_tokio_test_attr(attrs: &[Attribute]) -> Option<&Attribute> {
    for attr in attrs {
        let path = attr.path();
        if !path
            .last()
            .map(|symbol| symbol.as_str() == "test")
            .unwrap_or(false)
        {
            continue;
        }
        if path.iter().any(|symbol| symbol.as_str() == "tokio") {
            return Some(attr);
        }
    }
    None
}

fn attr_has_multi_thread_flavor(attr: &Attribute) -> bool {
    let Some(list) = attr.meta_item_list() else {
        return false;
    };
    for nested in list {
        let Some(item) = nested.meta_item() else {
            continue;
        };
        let name = item
            .path
            .segments
            .last()
            .map(|s| s.ident.as_str())
            .unwrap_or("");
        if name != "flavor" {
            continue;
        }
        // Read the literal value.
        let Some(value) = item.name_value_literal() else {
            continue;
        };
        if let LitKind::Str(sym, _) = value.kind {
            if sym.as_str() == "multi_thread" {
                return true;
            }
        }
    }
    false
}

struct SpawnFinder<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    found_at: Option<Span>,
}

impl<'tcx> Visitor<'tcx> for SpawnFinder<'_, 'tcx> {
    type NestedFilter = nested_filter::OnlyBodies;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.cx.tcx
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if self.found_at.is_some() {
            return;
        }
        if let ExprKind::Call(callee, _) = expr.kind {
            if let ExprKind::Path(ref qpath) = callee.kind {
                if let Res::Def(_, def_id) = self.cx.qpath_res(qpath, callee.hir_id) {
                    if def_path_equals(self.cx, def_id, &["tokio", "task", "spawn"])
                        || def_path_equals(self.cx, def_id, &["tokio", "spawn"])
                    {
                        self.found_at = Some(expr.span);
                        return;
                    }
                }
            }
        }
        walk_expr(self, expr);
    }
}

fn emit_lint(cx: &LateContext<'_>, fn_span: Span, spawn_span: Span) {
    cx.opt_span_lint(
        REQUIRE_MULTI_THREAD_FLAVOR_WHEN_SPAWNING,
        Some(fn_span),
        DiagDecorator(move |diag| {
            diag.primary_message(
                "`#[tokio::test]` on an async fn that calls `tokio::spawn` — the default \
                 `current_thread` flavor runs spawned tasks on the same thread as the \
                 test, so any cross-task await deadlocks. Add \
                 `#[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]` (or move \
                 the spawn out of the test body). If this test is genuinely fine on \
                 `current_thread`, allowlist the file in \
                 `dylints/require_multi_thread_flavor_when_spawning/src/allowlist.txt` \
                 with a one-line reason.",
            );
            diag.span_help(spawn_span, "the `tokio::spawn` call lives here");
        }),
    );
}

fn source_filename(cx: &LateContext<'_>, span: Span) -> String {
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
