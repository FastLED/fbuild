#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, ExpnKind, FileName, MacroKind, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `println!`, `eprintln!`, `print!`, `eprint!` invocations
    /// inside `crates/fbuild-cli/src/**` and `crates/fbuild-build/src/**`,
    /// EXCEPT in `crates/fbuild-cli/src/output.rs` (which IS the bridge).
    ///
    /// ### Why is this bad?
    ///
    /// `println!`/`eprintln!` bypass the workspace's verbosity and
    /// color discipline. The bridge `fbuild_cli::output::*` is
    /// `tracing`-backed so `--quiet`, `--verbose`, and
    /// `--color={auto,always,never}` all flow through one level
    /// filter.
    ///
    /// `result()` (the one exception) keeps the final-answer line on
    /// `println!` so pipe redirection still works — but it's gated
    /// behind the bridge.
    ///
    /// FastLED/fbuild#844 (bridge sweep, lint 12). Per user directive
    /// the allowlist is empty except for the bridge module itself.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-cli/src/cli/build.rs
    /// println!("Building env {env}"); // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// use crate::output;
    /// output::progress(format!("Building env {env}"));
    /// // for the final answer:
    /// output::result(format!("OK"));
    /// ```
    pub BAN_PRINT_IN_PRODUCTION,
    Deny,
    "ban println!/eprintln!/print!/eprint! in CLI + build production code"
}

/// Macro expansions we ban. We detect by the macro's def path.
const BANNED_MACRO_NAMES: &[&str] = &["println", "eprintln", "print", "eprint"];

/// Underlying `_print` / `_eprint` calls (the macros expand to these).
const PRINT_DEF_PATHS: &[&[&str]] = &[
    &["std", "io", "stdio", "_print"],
    &["std", "io", "stdio", "_eprint"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// File-path scope. ONLY these two prefixes are linted.
const SCOPES: &[&str] = &[
    "crates/fbuild-cli/src/",
    "crates/fbuild-build/src/",
];

/// The bridge IS exempt — it's the module everything else is migrating to.
const BRIDGE_MODULES: &[&str] = &["crates/fbuild-cli/src/output.rs"];

impl<'tcx> LateLintPass<'tcx> for BanPrintInProduction {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_scope(&normalized) {
            return;
        }
        if is_bridge_module(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if is_test_file(&normalized) {
            return;
        }

        // Two detection paths:
        // 1. Span's macro expansion is one of `println!`/`eprintln!`/…
        // 2. The call expression resolves to `std::io::_print` / `_eprint`.
        if let Some(name) = macro_name(expr.span) {
            if BANNED_MACRO_NAMES.contains(&name.as_str()) {
                emit_lint(cx, expr.span, &name);
                return;
            }
        }

        if let ExprKind::Call(ref func, _) = expr.kind {
            if let ExprKind::Path(ref qpath) = func.kind {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, func.hir_id) {
                    for banned in PRINT_DEF_PATHS {
                        if def_path_equals(cx, def_id, banned) {
                            emit_lint(cx, expr.span, "println-family");
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn macro_name(span: rustc_span::Span) -> Option<String> {
    let expn = span.ctxt().outer_expn_data();
    match expn.kind {
        ExpnKind::Macro(MacroKind::Bang, name) => Some(name.to_string()),
        _ => None,
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, what: &str) {
    let what = what.to_string();
    cx.opt_span_lint(
        BAN_PRINT_IN_PRODUCTION,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{what}!` bypasses fbuild's verbosity + color discipline. \
                 Use `crate::output::{{progress, result, warn, error, debug}}` \
                 (in fbuild-cli) or `tracing::{{info, debug, warn, error}}` \
                 (in fbuild-build) instead. See FastLED/fbuild#844."
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
    SCOPES.iter().any(|s| normalized.contains(s))
}

fn is_bridge_module(normalized: &str) -> bool {
    BRIDGE_MODULES
        .iter()
        .any(|bridge| normalized.ends_with(bridge))
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
    fn scope_matches_cli_and_build() {
        assert!(in_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(in_scope("crates/fbuild-build/src/orchestrator.rs"));
        assert!(!in_scope("crates/fbuild-daemon/src/context.rs"));
    }

    #[test]
    fn bridge_module_is_exempt() {
        assert!(is_bridge_module("crates/fbuild-cli/src/output.rs"));
        assert!(!is_bridge_module("crates/fbuild-cli/src/cli/build.rs"));
    }
}
