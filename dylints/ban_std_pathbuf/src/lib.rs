#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{AmbigArg, Expr, ExprKind, Ty, TyKind, def::Res};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, symbol::Symbol};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::path::PathBuf` outside the explicit legacy allowlist.
    ///
    /// ### Why is this bad?
    ///
    /// Raw `PathBuf` values do not carry fbuild's normalization invariant, which
    /// has caused Windows-only cache key and watcher mismatches (FastLED/fbuild#436,
    /// #437, #282).
    ///
    /// ### Known problems
    ///
    /// The workspace still has legacy `PathBuf` call sites. Those files are
    /// temporarily allowlisted and should be removed from the allowlist as they are
    /// migrated.
    ///
    /// ### Example
    ///
    /// ```rust
    /// use std::path::PathBuf;
    ///
    /// let path = PathBuf::from("src/foo.c");
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// use fbuild_core::path::NormalizedPath;
    ///
    /// let path = NormalizedPath::new("src/foo.c");
    /// ```
    pub BAN_STD_PATHBUF,
    Deny,
    "ban std::path::PathBuf outside the legacy allowlist"
}

const PATHBUF_DEF_PATH: &[&str] = &["std", "path", "PathBuf"];
const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for BanStdPathbuf {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        if is_allowlisted(cx, ty.span) {
            return;
        }

        if let TyKind::Path(qpath) = ty.kind {
            let res = cx.qpath_res(&qpath, ty.hir_id);
            if res_is_pathbuf(cx, res) {
                emit_lint(cx, ty.span);
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if is_allowlisted(cx, expr.span) {
            return;
        }

        if let ExprKind::Path(qpath) = expr.kind {
            let res = cx.qpath_res(&qpath, expr.hir_id);
            if res_is_pathbuf_assoc(cx, res) {
                emit_lint(cx, expr.span);
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_STD_PATHBUF,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "use fbuild_core::path::NormalizedPath instead of std::path::PathBuf",
            );
        }),
    );
}

fn is_allowlisted(cx: &LateContext<'_>, span: rustc_span::Span) -> bool {
    let filename = match cx.sess().source_map().span_to_filename(span) {
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
    };
    let normalized = normalize_slashes(&filename);
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|allowed| normalized.ends_with(allowed))
}

fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

fn res_is_pathbuf(cx: &LateContext<'_>, res: Res) -> bool {
    match res {
        Res::Def(_, def_id) => def_path_starts_with(cx, def_id, PATHBUF_DEF_PATH),
        _ => false,
    }
}

fn res_is_pathbuf_assoc(cx: &LateContext<'_>, res: Res) -> bool {
    match res {
        Res::Def(_, def_id) => {
            let def_path = cx.get_def_path(def_id);
            def_path.len() > PATHBUF_DEF_PATH.len()
                && def_path
                    .iter()
                    .take(PATHBUF_DEF_PATH.len())
                    .zip(PATHBUF_DEF_PATH.iter())
                    .all(|(actual, expected)| *actual == Symbol::intern(expected))
        }
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

#[test]
fn ui() {
    // Dylint 6.0.1 looks for its test library directly under
    // <target>/debug, while Soldr selects the host through
    // CARGO_BUILD_TARGET and Cargo writes to <target>/<host>/debug.
    // SAFETY: this test binary contains one test, so no peer can observe
    // the process-wide environment change.
    unsafe {
        std::env::remove_var("CARGO_BUILD_TARGET");
    }
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
