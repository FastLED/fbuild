#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind, HirId};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{
    symbol::{sym, Symbol},
    FileName, RemapPathScopeComponents,
};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `.unwrap()` method calls inside fbuild *production* code:
    ///   * `crates/fbuild-daemon/src/**` (any depth), and
    ///   * `crates/fbuild-cli/src/cli/**` (any depth).
    ///
    /// The following are exempt:
    ///   * sibling test files (path tail matches `tests.rs`,
    ///     `*_tests.rs`, or `tests_*.rs`), and
    ///   * `.unwrap()` calls inside a `#[cfg(test)] mod` block (the
    ///     lint walks the owning module chain looking for the
    ///     `cfg(test)` attribute).
    ///
    /// ### Why is this bad?
    ///
    /// In the daemon, panics in any code path (HTTP/WebSocket
    /// handler, broker worker, lock manager, build dispatcher) crash
    /// the process, which disconnects every connected client (CLI
    /// build streams, monitor WebSockets, FastLED's `SerialMonitor`,
    /// the FastAPI bridge). Production code must convert errors into
    /// structured HTTP responses, WS error frames, or `Result`
    /// returns instead of panicking.
    ///
    /// In the CLI, panics drop the user into a Rust backtrace
    /// instead of a clean error message and non-zero exit, which is
    /// a poor UX and obscures the actual problem.
    ///
    /// PR #833 fixed the original daemon-handler scope. FastLED/fbuild#844
    /// item 11 widened the scope to all of `fbuild-daemon/src/` and
    /// `fbuild-cli/src/cli/`.
    ///
    /// ### Known problems
    ///
    /// - The lint walks owning modules looking for `#[cfg(test)]`.
    ///   It does NOT detect `#[cfg(test)]` on individual *items*
    ///   (e.g. `#[cfg(test)] fn helper() {}` in a non-test module).
    ///   Such items should be moved under a proper
    ///   `#[cfg(test)] mod tests` instead.
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
    /// post-mortem-friendly message, `.unwrap_or_else(|e| e.into_inner())`
    /// for poison-tolerant `Mutex::lock()`, or convert the error
    /// into a structured response (`?` when the fn returns `Result`).
    pub BAN_UNWRAP_IN_PRODUCTION,
    Deny,
    "ban .unwrap() inside fbuild production code (daemon + cli/cli/**)"
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

/// In-scope path prefixes. A file is in scope if its (normalized,
/// forward-slash) path contains any of these substrings.
const IN_SCOPE_PREFIXES: &[&str] = &[
    "crates/fbuild-daemon/src/",
    "crates/fbuild-cli/src/cli/",
];

impl<'tcx> LateLintPass<'tcx> for BanUnwrapInProduction {
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

/// Walk up the HIR parent chain looking for `#[cfg(test)]`.
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

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_UNWRAP_IN_PRODUCTION,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`.unwrap()` inside fbuild production code (fbuild-daemon/src/** or \
                 fbuild-cli/src/cli/**) can crash the daemon / drop the CLI user into \
                 a Rust backtrace. Convert the error into a structured response \
                 (HTTP 500 / WS error frame), propagate it with `?` if the fn returns \
                 `Result`, use `.unwrap_or_else(|e| e.into_inner())` for poison-tolerant \
                 `Mutex::lock()`, or use `.expect(\"<post-mortem-friendly reason>\")` \
                 with an actionable invariant message. Tests are exempt — either move \
                 the call into a `#[cfg(test)] mod tests { ... }` block or put it in \
                 a sibling `tests.rs` / `*_tests.rs` / `tests_*.rs` file.",
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
    IN_SCOPE_PREFIXES
        .iter()
        .any(|prefix| normalized.contains(prefix))
}

/// A "sibling test file" is one whose base name marks the file itself
/// as test code (not production), matching:
///   * `tests.rs`
///   * `*_tests.rs` (e.g. `websockets_tests.rs`)
///   * `tests_*.rs` (e.g. `tests_npm_cache.rs`, `tests_process.rs`)
fn is_test_file(normalized: &str) -> bool {
    let Some(name) = normalized.rsplit('/').next() else {
        return false;
    };
    if !name.ends_with(".rs") {
        return false;
    }
    if name == "tests.rs" {
        return true;
    }
    let stem = &name[..name.len() - 3]; // strip ".rs"
    stem.ends_with("_tests") || stem.starts_with("tests_")
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
        assert!(in_scope(
            "crates/fbuild-daemon/src/handlers/operations/build.rs"
        ));
        assert!(in_scope("crates/fbuild-daemon/src/handlers/websockets.rs"));
        assert!(in_scope(
            "/anywhere/crates/fbuild-daemon/src/handlers/emulator/qemu_deploy.rs"
        ));
        // Widened scope: all of fbuild-daemon/src/, not just handlers/.
        assert!(in_scope("crates/fbuild-daemon/src/context.rs"));
        assert!(in_scope("crates/fbuild-daemon/src/main.rs"));
        assert!(in_scope("crates/fbuild-daemon/src/broker/session.rs"));
    }

    #[test]
    fn cli_cli_paths_are_in_scope() {
        assert!(in_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(in_scope("crates/fbuild-cli/src/cli/deploy.rs"));
        assert!(in_scope(
            "/anywhere/crates/fbuild-cli/src/cli/clang_tools.rs"
        ));
    }

    #[test]
    fn cli_non_cli_paths_are_out_of_scope() {
        // Only crates/fbuild-cli/src/cli/** is in scope, not the rest of fbuild-cli.
        assert!(!in_scope("crates/fbuild-cli/src/main.rs"));
        assert!(!in_scope("crates/fbuild-cli/src/lib_select.rs"));
        assert!(!in_scope("crates/fbuild-cli/src/mcp/server.rs"));
    }

    #[test]
    fn other_crates_are_out_of_scope() {
        assert!(!in_scope("crates/fbuild-core/src/lib.rs"));
        assert!(!in_scope("crates/fbuild-build/src/compiler.rs"));
        assert!(!in_scope("crates/fbuild-serial/src/crash_decoder.rs"));
    }

    #[test]
    fn sibling_test_files_are_exempt() {
        // Plain `tests.rs`
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/operations/tests.rs"
        ));
        // `*_tests.rs`
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/websockets_tests.rs"
        ));
        // `tests_*.rs`
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/emulator/tests_npm_cache.rs"
        ));
        assert!(is_test_file(
            "crates/fbuild-cli/src/cli/tests_select_runner.rs"
        ));
    }

    #[test]
    fn production_files_are_not_marked_as_test() {
        // Per FastLED/fbuild#844 item 11: the old substring-based test detection
        // accidentally exempted any file with "tests" in its name. The new
        // detection requires the specific suffixes/prefixes above.
        assert!(!is_test_file(
            "crates/fbuild-daemon/src/handlers/operations/build.rs"
        ));
        assert!(!is_test_file(
            "crates/fbuild-daemon/src/handlers/websockets.rs"
        ));
        // A production file that merely contains "tests" but doesn't match
        // the patterns must NOT be exempt.
        assert!(!is_test_file(
            "crates/fbuild-daemon/src/run_tests_pipeline.rs"
        ));
    }
}
