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
    /// Bans direct `reqwest::Client::new`, `reqwest::ClientBuilder`,
    /// `reqwest::get`, and `reqwest::blocking::get` outside the fbuild
    /// HTTP bridge (`crates/fbuild-core/src/http.rs` and the
    /// forward-compat re-export at `crates/fbuild-packages/src/http.rs`).
    ///
    /// ### Why is this bad?
    ///
    /// Every HTTP call in fbuild must go through
    /// `fbuild_core::http::client()` so the workspace shares one
    /// configured timeout matrix, one TLS configuration, and one
    /// reqwest dependency surface. Direct construction sites silently
    /// regress those invariants.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 1").
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let client = reqwest::Client::new(); // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let client = fbuild_core::http::client();
    /// ```
    pub BAN_BARE_REQWEST,
    Deny,
    "ban direct reqwest::Client construction outside the fbuild HTTP bridge"
}

/// Fully-qualified paths to banned items.
const BANNED_PATHS: &[&[&str]] = &[
    &["reqwest", "Client", "new"],
    &["reqwest", "ClientBuilder", "new"],
    &["reqwest", "get"],
    &["reqwest", "blocking", "Client", "new"],
    &["reqwest", "blocking", "ClientBuilder", "new"],
    &["reqwest", "blocking", "get"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Files that ARE the bridge — exempted by path so they can construct
/// the underlying reqwest client. Slash-normalized suffix match.
const BRIDGE_MODULES: &[&str] = &[
    "crates/fbuild-core/src/http.rs",
    "crates/fbuild-packages/src/http.rs",
];

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanBareReqwest {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_bridge_module(&normalized) || is_allowlisted(&normalized) {
            return;
        }

        match expr.kind {
            ExprKind::MethodCall(_, _, _, _) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
                }
            }
            ExprKind::Path(ref qpath) => {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) {
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
        BAN_BARE_REQWEST,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` bypasses fbuild's HTTP bridge. Use \
                 `fbuild_core::http::client()` (shared async client), \
                 `fbuild_core::http::client_with_timeout(...)` (per-call \
                 timeout override), or `fbuild_core::http::blocking_client(...)` \
                 (for the rare OS-thread case). See FastLED/fbuild#844."
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

fn in_production_scope(normalized: &str) -> bool {
    let Some(crates_at) = normalized.find(CRATES_PREFIX) else {
        return false;
    };
    let after_crates = &normalized[crates_at + CRATES_PREFIX.len()..];
    after_crates.contains(SRC_SEGMENT)
}

fn is_bridge_module(normalized: &str) -> bool {
    BRIDGE_MODULES
        .iter()
        .any(|bridge| normalized.ends_with(bridge))
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
    fn bridge_modules_recognized() {
        assert!(is_bridge_module("crates/fbuild-core/src/http.rs"));
        assert!(is_bridge_module("crates/fbuild-packages/src/http.rs"));
        assert!(!is_bridge_module("crates/fbuild-cli/src/cli/build.rs"));
    }

    #[test]
    fn production_scope_matches() {
        assert!(in_production_scope("crates/fbuild-cli/src/cli/port_scan.rs"));
        assert!(!in_production_scope("crates/fbuild-daemon/tests/test_emu_endpoint.rs"));
    }
}
