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
    /// Bans `Path::starts_with` and `Path::strip_prefix` (on
    /// `std::path::Path`, and therefore also `PathBuf` via deref) in
    /// fbuild production code (files under `crates/*/src/`).
    ///
    /// Catches both call shapes:
    ///   * Method-call:  `p.starts_with(root)` / `p.strip_prefix(root)`
    ///   * Qualified path call:
    ///     `Path::starts_with(&p, root)` etc.
    ///
    /// ### Why is this bad?
    ///
    /// `Path::starts_with` / `strip_prefix` compare **component strings
    /// as written**. Two spellings of the same location do not match:
    /// a canonicalized dir (`\\?\C:\…` stripped, symlinks resolved,
    /// `/private/var/…` on macOS) versus a raw dir, a trailing slash,
    /// or a case difference on Windows/macOS. When one side is a cache
    /// root or project dir and the other is a path pulled from a
    /// compiler flag, the comparison silently mismatches — a
    /// cross-project cache key then encodes the project directory and
    /// the global cache never hits (FastLED/fbuild#952).
    ///
    /// Normalize both sides through the shared factory first —
    /// `fbuild_core::path::normalize_for_key` (strips `\\?\`, folds
    /// separators to `/`, case-folds on Windows/macOS) — or compare
    /// `NormalizedPath` values, before any prefix test.
    ///
    /// ### Known problems
    ///
    /// Allowlisting is file-level via `src/allowlist.txt`. Many raw
    /// prefix comparisons are legitimate — the receiver and argument are
    /// already in the same normal form, or the result does not feed a
    /// cache key (e.g. the path-normalization bridge itself, workspace
    /// relativization in `zccache.rs`, test scaffolding under
    /// `crates/*/src/`). Add such files to the allowlist with a one-line
    /// reason. Files outside `crates/*/src/` are out of scope by design.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // banned: raw spelling comparison against a root
    /// if include_dir.starts_with(project_dir) { /* ... */ }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// use fbuild_core::path::normalize_for_key;
    /// let dir_key = normalize_for_key(project_dir);
    /// if normalize_for_key(include_dir).starts_with(&format!("{dir_key}/")) {
    ///     /* ... */
    /// }
    /// ```
    pub BAN_RAW_PATH_PREFIX_COMPARE,
    Deny,
    "ban raw Path::{starts_with, strip_prefix} in fbuild production code"
}

/// Each entry is a fully-qualified path to a banned method. Matching is
/// exact against the canonical method `DefId`, so `PathBuf` receivers
/// (which deref to `Path`) and re-exports resolve to the same
/// `std::path::Path::*` entry.
const BANNED_METHOD_PATHS: &[&[&str]] = &[
    &["std", "path", "Path", "starts_with"],
    &["std", "path", "Path", "strip_prefix"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Production-code scope. Only files whose path contains BOTH
/// `crates/` and a subsequent `/src/` segment are linted. This
/// intentionally excludes `crates/*/tests/`, `crates/*/examples/`,
/// `crates/*/benches/`, `ci/`, `dylints/`, build scripts, and any
/// other non-production path.
const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanRawPathPrefixCompare {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_allowlisted(&normalized) {
            return;
        }

        match expr.kind {
            // Method-call shape: `p.starts_with(root)`. `type_dependent_def_id`
            // returns the canonical DefId of the resolved method.
            ExprKind::MethodCall(_, _, _, _) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    check_def_id(cx, expr.span, def_id);
                }
            }
            // Qualified-path call shape: `Path::starts_with(&p, root)`.
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
    for banned in BANNED_METHOD_PATHS {
        if def_path_equals(cx, def_id, banned) {
            emit_lint(cx, span, banned);
            return;
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_RAW_PATH_PREFIX_COMPARE,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` compares path component strings as written, so a \
                 canonicalized vs raw spelling (or `\\\\?\\` prefix, trailing \
                 slash, or case difference) silently mismatches. When either \
                 side is a cache root or project dir this defeats a cache key \
                 (FastLED/fbuild#952). Normalize both sides through \
                 `fbuild_core::path::normalize_for_key` (or compare \
                 `NormalizedPath` values) first. If this comparison is \
                 already same-normalized or does not feed a cache key, \
                 allowlist the file in \
                 `dylints/ban_raw_path_prefix_compare/src/allowlist.txt` with \
                 a one-line reason."
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
        assert!(in_production_scope("crates/fbuild-build/src/compiler.rs"));
        assert!(in_production_scope(
            "C:/Users/x/dev/fbuild/crates/fbuild-packages/src/library/library_compiler.rs"
        ));
    }

    #[test]
    fn in_production_scope_rejects_non_src() {
        assert!(!in_production_scope(
            "crates/fbuild-cli/tests/lib_select.rs"
        ));
        assert!(!in_production_scope(
            "dylints/ban_raw_path_prefix_compare/src/lib.rs"
        ));
        assert!(!in_production_scope("build.rs"));
    }

    #[test]
    fn is_allowlisted_matches_path_suffix() {
        assert!(crate::is_allowlisted(
            "/anywhere/crates/fbuild-core/src/path.rs"
        ));
    }
}
