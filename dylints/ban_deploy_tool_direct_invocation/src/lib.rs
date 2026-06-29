#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_ast::ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `Command::new("esptool" | "esptool.py" | "avrdude" |
    /// "picotool" | "dfu-util" | "pyocd")` (and the
    /// `tokio::process::Command` equivalent) in fbuild production code
    /// (`crates/*/src/`) outside `crates/fbuild-deploy/`.
    ///
    /// ### Why is this bad?
    ///
    /// All deploy-tool spawns must flow through `fbuild deploy`. Direct
    /// invocation outside `fbuild-deploy` skips
    /// `Deployer::post_deploy_recovery`, error reporting, serial-port
    /// hand-off, and the `--to emu` emulator routing.
    ///
    /// This lint complements `ban_raw_subprocess` (which catches
    /// `.spawn()/.output()/.status()` on `Command` regardless of the
    /// binary). They form an L-shape: even with legitimate raw-spawn
    /// entries, you still can't spawn a deploy tool outside
    /// `fbuild-deploy`.
    ///
    /// ### Known problems
    ///
    /// Only direct string literals are matched. `Command::new(path)`
    /// where `path` is computed at runtime won't trip the lint —
    /// runtime-resolved paths are likely going through `fbuild-deploy`
    /// already, and the lint targets the quick-sketch shape that's easy
    /// to add and easy to miss in review.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Banned outside fbuild-deploy:
    /// let out = std::process::Command::new("esptool")
    ///     .args(["--chip", "esp32", "flash_id"])
    ///     .output()?;
    /// ```
    ///
    /// Use instead: open an issue and call into `fbuild-deploy`'s
    /// `Deployer` trait, or run the command via `fbuild deploy`.
    pub BAN_DEPLOY_TOOL_DIRECT_INVOCATION,
    Deny,
    "ban Command::new(<deploy-tool>) outside fbuild-deploy"
}

/// Banned binary-name string literals. Matched case-sensitively against
/// the first argument of `Command::new(...)`.
const BANNED_BINARIES: &[&str] = &[
    "esptool",
    "esptool.py",
    "avrdude",
    "picotool",
    "dfu-util",
    "pyocd",
];

/// Methods we hook. Both `std` and `tokio` `Command::new` are flagged.
const COMMAND_NEW_PATHS: &[&[&str]] = &[
    &["std", "process", "Command", "new"],
    &["tokio", "process", "Command", "new"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

/// `crates/fbuild-deploy/` is the legitimate owner and is exempted by
/// directory match (not by allowlist). The full slash boundary ensures
/// a hypothetical `crates/fbuild-deploy-extras/` would still be linted.
const DEPLOY_DIR: &str = "crates/fbuild-deploy/";

impl<'tcx> LateLintPass<'tcx> for BanDeployToolDirectInvocation {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized)
            || normalized.contains(DEPLOY_DIR)
            || is_allowlisted(&normalized)
        {
            return;
        }

        // We only care about `Command::new(<literal>)` call sites. The
        // method-call shape covers the common `Command::new(...)` form
        // (yes — it's a Call, not a MethodCall, because `new` is an
        // associated function). Treat ExprKind::Call.
        if let ExprKind::Call(callee, args) = expr.kind {
            let Some(arg) = args.first() else {
                return;
            };
            let Some(literal) = string_literal(arg) else {
                return;
            };
            if !BANNED_BINARIES.iter().any(|b| literal == *b) {
                return;
            }
            // Resolve the callee — it must be `(std|tokio)::process::Command::new`.
            if let ExprKind::Path(ref qpath) = callee.kind {
                if let rustc_hir::def::Res::Def(_, def_id) = cx.qpath_res(qpath, callee.hir_id) {
                    for command_new in COMMAND_NEW_PATHS {
                        if def_path_equals(cx, def_id, command_new) {
                            emit_lint(cx, expr.span, &literal);
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn string_literal(expr: &Expr<'_>) -> Option<String> {
    if let ExprKind::Lit(ref lit) = expr.kind {
        if let LitKind::Str(sym, _) = lit.node {
            return Some(sym.as_str().to_owned());
        }
    }
    None
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, binary: &str) {
    let binary = binary.to_owned();
    cx.opt_span_lint(
        BAN_DEPLOY_TOOL_DIRECT_INVOCATION,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`Command::new(\"{binary}\")` outside fbuild-deploy bypasses the \
                 `fbuild deploy` contract (no post-deploy serial recovery, no \
                 emulator routing, no consistent error surface). Route through \
                 `fbuild-deploy`'s `Deployer` API or run via `fbuild deploy`."
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
    fn in_production_scope_matches_src_files() {
        assert!(in_production_scope("crates/fbuild-build/src/foo.rs"));
    }

    #[test]
    fn deploy_dir_match_is_anchored() {
        assert!("crates/fbuild-deploy/src/avr.rs".contains(DEPLOY_DIR));
        assert!(!"crates/fbuild-deploy-extras/src/lib.rs".contains(DEPLOY_DIR));
        assert!(!"crates/fbuild-build/src/foo.rs".contains(DEPLOY_DIR));
    }
}
