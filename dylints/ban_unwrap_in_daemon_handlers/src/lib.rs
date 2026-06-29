#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind, HirId};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `.unwrap()` method calls inside files under
    /// `crates/fbuild-daemon/src/handlers/` (any depth), with two
    /// exemptions:
    ///   * test files (path tail matches `*tests*.rs`), and
    ///   * `.unwrap()` calls inside a `#[cfg(test)] mod` block (the
    ///     lint walks the owning module chain looking for the
    ///     `cfg(test)` attribute).
    ///
    /// ### Why is this bad?
    ///
    /// Panics in HTTP/WebSocket handler paths crash the daemon
    /// process, which disconnects every connected client (CLI build
    /// streams, monitor WebSockets, FastLED's `SerialMonitor`, the
    /// FastAPI bridge). Handlers must convert errors into structured
    /// HTTP responses or WS error frames, not panic.
    ///
    /// PR #833 already fixed the 10 known pre-existing `.unwrap()`
    /// violations in daemon handlers; this lint locks in that state
    /// so new violations can't sneak in.
    ///
    /// ### Known problems
    ///
    /// - The lint walks owning modules looking for `#[cfg(test)]`.
    ///   It does NOT detect `#[cfg(test)]` on individual *items*
    ///   (e.g. `#[cfg(test)] fn helper() {}` in a non-test module).
    ///   Such items are rare in handler code and should be moved
    ///   under a proper `#[cfg(test)] mod tests` instead.
    /// - The lint resolves the *method* DefId to `Option::unwrap` /
    ///   `Result::unwrap`. Custom types that define their own
    ///   `unwrap()` method are not affected.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-daemon/src/handlers/websockets.rs
    /// async fn handler(req: Request) -> Response {
    ///     let body = serde_json::to_string(&payload).unwrap(); // banned
    ///     ...
    /// }
    /// ```
    ///
    /// Use instead: `.unwrap_or_default()`, `.expect("…")` with a
    /// post-mortem-friendly message, or convert the error into a
    /// structured response.
    pub BAN_UNWRAP_IN_DAEMON_HANDLERS,
    Deny,
    "ban .unwrap() inside fbuild-daemon HTTP/WS handler code"
}

/// Methods we ban. Matched on the canonical DefId path returned by
/// `type_dependent_def_id` — so `.unwrap()` on any `Option` /
/// `Result` resolves correctly regardless of how the type was named
/// in source.
const BANNED_METHOD_PATHS: &[&[&str]] = &[
    &["core", "option", "Option", "unwrap"],
    &["std", "option", "Option", "unwrap"],
    &["core", "result", "Result", "unwrap"],
    &["std", "result", "Result", "unwrap"],
];

/// Scope: any file path matching this prefix is in scope.
const HANDLERS_DIR: &str = "crates/fbuild-daemon/src/handlers/";

impl<'tcx> LateLintPass<'tcx> for BanUnwrapInDaemonHandlers {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_scope(&normalized) || is_test_file(&normalized) {
            return;
        }

        let ExprKind::MethodCall(_, _, _, _) = expr.kind else {
            return;
        };

        let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
            return;
        };

        let mut is_banned = false;
        for banned in BANNED_METHOD_PATHS {
            if def_path_equals(cx, def_id, banned) {
                is_banned = true;
                break;
            }
        }
        if !is_banned {
            return;
        }

        if owned_by_cfg_test_module(cx, expr.hir_id) {
            return;
        }

        emit_lint(cx, expr.span);
    }
}

/// Walk up the HIR owner chain looking for a module annotated with
/// `#[cfg(test)]`. We deliberately only look at module items —
/// `#[cfg(test)]` on a function is the right wrong-shape to nudge
/// the developer into `mod tests { ... }`.
fn owned_by_cfg_test_module(cx: &LateContext<'_>, hir_id: HirId) -> bool {
    let mut current = cx.tcx.hir_get_parent_item(hir_id);
    loop {
        // `is_in_test_module` via attribute scan:
        let attrs = cx.tcx.hir_attrs(current.into());
        for attr in attrs {
            if attr_is_cfg_test(attr) {
                return true;
            }
        }
        let parent = cx.tcx.hir_get_parent_item(current.into());
        if parent == current {
            return false;
        }
        current = parent;
    }
}

fn attr_is_cfg_test(attr: &rustc_hir::Attribute) -> bool {
    let Some(meta) = attr.meta() else {
        return false;
    };
    let path = meta.path();
    if path.segments.len() != 1 || path.segments[0].ident.as_str() != "cfg" {
        return false;
    }
    let Some(list) = meta.meta_item_list() else {
        return false;
    };
    list.iter().any(|nested| {
        nested
            .ident()
            .map(|id| id.as_str() == "test")
            .unwrap_or(false)
    })
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_UNWRAP_IN_DAEMON_HANDLERS,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`.unwrap()` inside an fbuild-daemon handler can crash the daemon and \
                 disconnect every connected client. Convert the error into a structured \
                 response (HTTP 500 / WS error frame) or use `.unwrap_or_default()` / \
                 `.expect(\"<post-mortem-friendly reason>\")`. Tests are exempt — either \
                 move the call into a `#[cfg(test)] mod tests { ... }` block or rename \
                 the file to `*tests*.rs`.",
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

fn in_scope(normalized: &str) -> bool {
    normalized.contains(HANDLERS_DIR)
}

fn is_test_file(normalized: &str) -> bool {
    let Some(name) = normalized.rsplit('/').next() else {
        return false;
    };
    // Match files whose base name contains "tests" (e.g.
    // `websockets_tests.rs`, `tests_npm_cache.rs`,
    // `tests_process.rs`, `tests_select_runner.rs`, `tests_outcome.rs`,
    // `tests.rs`). A plain `tests` substring is enough — the handler
    // tree doesn't use `tests` as a prefix for non-test code.
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
    fn handler_paths_are_in_scope() {
        assert!(in_scope(
            "crates/fbuild-daemon/src/handlers/operations/build.rs"
        ));
        assert!(in_scope("crates/fbuild-daemon/src/handlers/websockets.rs"));
        assert!(in_scope(
            "/anywhere/crates/fbuild-daemon/src/handlers/emulator/qemu_deploy.rs"
        ));
    }

    #[test]
    fn non_handler_paths_are_out_of_scope() {
        assert!(!in_scope("crates/fbuild-daemon/src/context.rs"));
        assert!(!in_scope("crates/fbuild-daemon/src/main.rs"));
        assert!(!in_scope("crates/fbuild-cli/src/cli/build.rs"));
    }

    #[test]
    fn test_files_are_exempt_by_name() {
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/websockets_tests.rs"
        ));
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/operations/tests.rs"
        ));
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/emulator/tests_npm_cache.rs"
        ));
        assert!(!is_test_file(
            "crates/fbuild-daemon/src/handlers/operations/build.rs"
        ));
        assert!(!is_test_file(
            "crates/fbuild-daemon/src/handlers/websockets.rs"
        ));
    }
}
