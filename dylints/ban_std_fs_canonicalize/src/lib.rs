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
    /// Bans `std::fs::canonicalize` and `tokio::fs::canonicalize` in
    /// production code, except inside the bridge module
    /// (`crates/fbuild-core/src/path.rs`).
    ///
    /// ### Why is this bad?
    ///
    /// Raw `canonicalize` on Windows injects the `\\?\` extended-
    /// length prefix, which then leaks into cache keys, log lines,
    /// and JSON dumps where the un-prefixed form is canonical. The
    /// bridge (`fbuild_core::path::canonicalize_existing`) strips
    /// the prefix and returns a `NormalizedPath` whose `Hash` /
    /// `Eq` / `Ord` are platform-stable.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 5").
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let canon = std::fs::canonicalize(&path)?; // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let canon = fbuild_core::path::canonicalize_existing(&path).await?;
    /// ```
    pub BAN_STD_FS_CANONICALIZE,
    Deny,
    "ban std::fs::canonicalize and tokio::fs::canonicalize outside the path bridge"
}

const BANNED_PATHS: &[&[&str]] = &[
    &["std", "fs", "canonicalize"],
    &["tokio", "fs", "canonicalize"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const BRIDGE_MODULES: &[&str] = &["crates/fbuild-core/src/path.rs", "crates/fbuild-core/src/fs.rs"];

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanStdFsCanonicalize {
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
        BAN_STD_FS_CANONICALIZE,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "raw `canonicalize` leaks the Windows `\\\\?\\` extended-length \
                 prefix into cache keys and logs. Use \
                 `fbuild_core::path::canonicalize_existing(...).await` — it \
                 strips the prefix and returns a `NormalizedPath` whose \
                 Hash/Eq/Ord are platform-stable. See FastLED/fbuild#844.",
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
