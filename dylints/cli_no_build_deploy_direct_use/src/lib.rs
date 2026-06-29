#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, AmbigArg, Expr, ExprKind, Ty, TyKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans direct references to items from the `fbuild_build` and
    /// `fbuild_deploy` crates in `crates/fbuild-cli/src/`, except in
    /// the files explicitly allowlisted as "diagnostic subcommands".
    ///
    /// ### Why is this bad?
    ///
    /// `fbuild-cli` is supposed to be a thin HTTP client to the
    /// daemon (see `crates/CLAUDE.md` §"Dependency Graph"). All build
    /// orchestration lives in `fbuild-build` and all firmware-upload
    /// logic lives in `fbuild-deploy`, both consumed by the daemon —
    /// not by the CLI directly. Calling those crates from the CLI
    /// silently bypasses the daemon, breaks build streaming, defeats
    /// deploy-preemption, and produces inconsistent error surfaces.
    ///
    /// The one exception is "diagnostic subcommands" — read-only,
    /// in-process inspectors like `lib-select`, `symbols`, `bloat`,
    /// `graph`, `lnk` — that don't need the build pipeline and would
    /// only add latency by going through HTTP. Those files are listed
    /// in `src/allowlist.txt`.
    ///
    /// ### Known problems
    ///
    /// Allowlisting is file-level. A new diagnostic subcommand must
    /// add itself to `src/allowlist.txt` with a one-line reason — the
    /// PR review is the audit trail.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-cli/src/cli/my_new_cmd.rs
    /// use fbuild_build::orchestrator::run_build;       // banned
    /// use fbuild_deploy::reset::reset_device;          // banned
    /// ```
    ///
    /// Use instead: POST to the daemon's HTTP endpoint and stream the
    /// response.
    pub CLI_NO_BUILD_DEPLOY_DIRECT_USE,
    Deny,
    "ban fbuild-cli from importing fbuild-build / fbuild-deploy outside diagnostic subcommands"
}

const BANNED_CRATES: &[&str] = &["fbuild_build", "fbuild_deploy"];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Scope: only `crates/fbuild-cli/src/` Rust files.
const CLI_SRC_DIR: &str = "crates/fbuild-cli/src/";

impl<'tcx> LateLintPass<'tcx> for CliNoBuildDeployDirectUse {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        let filename = source_filename(cx, ty.span);
        let normalized = normalize_slashes(&filename);
        if !in_scope(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if let TyKind::Path(qpath) = ty.kind {
            let res = cx.qpath_res(&qpath, ty.hir_id);
            if let Some(banned) = res_banned_crate(cx, res) {
                emit_lint(cx, ty.span, banned);
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_scope(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if let ExprKind::Path(ref qpath) = expr.kind {
            let res = cx.qpath_res(qpath, expr.hir_id);
            if let Some(banned) = res_banned_crate(cx, res) {
                emit_lint(cx, expr.span, banned);
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &str) {
    let banned = banned.to_owned();
    cx.opt_span_lint(
        CLI_NO_BUILD_DEPLOY_DIRECT_USE,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`fbuild-cli` references `{banned}::*` — the CLI is supposed to be a thin \
                 HTTP client to the daemon (see `crates/CLAUDE.md` §\"Dependency Graph\"). \
                 Build orchestration belongs in the daemon. If this is a read-only \
                 diagnostic subcommand that legitimately bypasses the daemon, allowlist \
                 the file in `dylints/cli_no_build_deploy_direct_use/src/allowlist.txt` \
                 with a one-line reason."
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
    normalized.contains(CLI_SRC_DIR)
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

fn res_banned_crate(cx: &LateContext<'_>, res: Res) -> Option<&'static str> {
    let Res::Def(_, def_id) = res else {
        return None;
    };
    let def_path = cx.get_def_path(def_id);
    if def_path.is_empty() {
        return None;
    }
    for banned in BANNED_CRATES {
        if def_path[0] == Symbol::intern(banned) {
            return Some(banned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_src_files_in_scope() {
        assert!(in_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(in_scope(
            "/home/runner/work/fbuild/crates/fbuild-cli/src/lib_select.rs"
        ));
    }

    #[test]
    fn non_cli_files_out_of_scope() {
        assert!(!in_scope("crates/fbuild-daemon/src/main.rs"));
        assert!(!in_scope("crates/fbuild-build/src/lib.rs"));
        assert!(!in_scope("crates/fbuild-cli/tests/integration.rs"));
    }
}
