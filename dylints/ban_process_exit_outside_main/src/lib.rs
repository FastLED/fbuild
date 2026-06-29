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
    /// Bans `std::process::exit(_)` calls in fbuild production code
    /// (`crates/*/src/`) outside the canonical program entry-point
    /// files (`crates/*/src/main.rs` and `crates/*/src/bin/*.rs`).
    ///
    /// ### Why is this bad?
    ///
    /// `std::process::exit` skips destructors. In fbuild that
    /// translates to: temp files / `TempDir`s under the cache root are
    /// not cleaned up; `running-process` containment guards do not run
    /// their drop handlers (child processes keep running after the
    /// parent dies); the tokio runtime is dropped abruptly so
    /// in-flight HTTP/WS responses are truncated.
    ///
    /// The only safe places to call `process::exit` are the program
    /// entry points where the runtime is being torn down anyway.
    /// Everywhere else, return a `Result` (or `FbuildError`) and let
    /// the entry point translate it into an exit code in one place.
    ///
    /// ### Known problems
    ///
    /// `std::process::abort` and `libc::_exit` are out of scope by
    /// design — they're explicit "I know what I'm doing" calls and
    /// already used inside post-fork containment paths.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Banned outside `main.rs` / `src/bin/*.rs`:
    /// if config.broken {
    ///     eprintln!("error: bad config");
    ///     std::process::exit(1);
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// if config.broken {
    ///     return Err(FbuildError::msg("bad config"));
    /// }
    /// ```
    pub BAN_PROCESS_EXIT_OUTSIDE_MAIN,
    Deny,
    "ban std::process::exit outside main.rs / src/bin/*.rs"
}

/// Banned path. Matched exactly against the resolved DefId's def-path.
const BANNED_PATH: &[&str] = &["std", "process", "exit"];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Production-code scope. Only files whose path contains BOTH
/// `crates/` and a subsequent `/src/` segment are linted.
const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanProcessExitOutsideMain {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized)
            || is_entry_point(&normalized)
            || is_allowlisted(&normalized)
        {
            return;
        }

        if let ExprKind::Path(ref qpath) = expr.kind {
            if let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) {
                if def_path_equals(cx, def_id, BANNED_PATH) {
                    emit_lint(cx, expr.span);
                }
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_PROCESS_EXIT_OUTSIDE_MAIN,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`std::process::exit` outside `main.rs` / `src/bin/*.rs` skips destructors \
                 — temp files leak, containment guards don't run, the tokio runtime is \
                 dropped abruptly, and in-flight HTTP/WS responses get truncated. Return a \
                 `Result` / `FbuildError` and let `main` translate it into an exit code in \
                 one place. If `process::exit` is truly justified here, allowlist the file \
                 in `dylints/ban_process_exit_outside_main/src/allowlist.txt` with a \
                 one-line reason.",
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

/// Unconditional exemptions for canonical program entry points:
///   * `crates/*/src/main.rs`
///   * `crates/*/src/bin/*.rs`
fn is_entry_point(normalized: &str) -> bool {
    normalized.ends_with("/src/main.rs")
        || normalized.contains("/src/bin/") && normalized.ends_with(".rs")
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
        assert!(in_production_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(in_production_scope("crates/fbuild-daemon/src/main.rs"));
    }

    #[test]
    fn in_production_scope_rejects_non_src() {
        assert!(!in_production_scope("crates/fbuild-cli/tests/foo.rs"));
        assert!(!in_production_scope("build.rs"));
    }

    #[test]
    fn entry_points_are_recognised() {
        assert!(is_entry_point("crates/fbuild-cli/src/main.rs"));
        assert!(is_entry_point("crates/fbuild-daemon/src/main.rs"));
        assert!(is_entry_point(
            "crates/fbuild-config/src/bin/enrich_boards.rs"
        ));
        assert!(is_entry_point(
            "crates/fbuild-daemon/src/bin/containment_harness.rs"
        ));
        assert!(!is_entry_point("crates/fbuild-cli/src/cli/build.rs"));
        // `lib.rs` is NOT an entry point — the function `main()` isn't there.
        assert!(!is_entry_point("crates/fbuild-cli/src/lib.rs"));
    }
}
