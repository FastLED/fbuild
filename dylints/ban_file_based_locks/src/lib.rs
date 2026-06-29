#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_ast::ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans file-based locking primitives in fbuild production code
    /// (`crates/*/src/`):
    ///
    ///   * `OpenOptions::create_new(true)` — the rename-or-fail lock-file
    ///     pattern.
    ///   * `fs2::FileExt` and `fs2::lock_*` methods (the `fs2` crate).
    ///   * raw `flock(...)` from libc / nix wrappers.
    ///
    /// ### Why is this bad?
    ///
    /// Per CLAUDE.md's "Key Constraints" section, fbuild has **no
    /// file-based locks** — all locking flows through the daemon's
    /// in-memory managers. File-based locks regress that contract:
    /// they leak across crashes on Windows, race with watch-set
    /// invalidation, and can deadlock CI when two workers share a
    /// stale lock-file path.
    ///
    /// ### Known problems
    ///
    /// Only direct call shapes are detected. Code that wraps `flock`
    /// behind a trait method whose def-path no longer mentions `fs2`,
    /// `flock`, or `OpenOptions::create_new` slips through and would
    /// need a follow-up extension. The current invariant is that fbuild
    /// has zero such wrappers.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // Banned: rename-or-fail lock file.
    /// let lock = OpenOptions::new()
    ///     .write(true)
    ///     .create_new(true)
    ///     .open("/var/run/fbuild.lock")?;
    /// ```
    ///
    /// Use instead: route the lock through the daemon's in-memory
    /// manager (e.g. `LockManager` in `fbuild-daemon`).
    pub BAN_FILE_BASED_LOCKS,
    Deny,
    "ban file-based locking primitives in fbuild production code"
}

/// Methods whose presence is enough to flag the call. Each entry is the
/// fully-qualified def-path; matching is exact.
///
/// `std::fs::OpenOptions::create_new` is special-cased because the
/// method itself is not banned — only the `(true)` argument form is.
/// The handler below inspects the argument.
const BANNED_METHOD_PATHS: &[&[&str]] = &[
    // fs2 crate (POSIX-style file locking API).
    &["fs2", "FileExt", "lock_exclusive"],
    &["fs2", "FileExt", "lock_shared"],
    &["fs2", "FileExt", "try_lock_exclusive"],
    &["fs2", "FileExt", "try_lock_shared"],
    &["fs2", "FileExt", "unlock"],
    // libc and nix flock wrappers.
    &["libc", "flock"],
    &["nix", "fcntl", "flock"],
];

/// `OpenOptions::create_new(true)` is the rename-or-fail lock-file
/// pattern. The method itself is not banned — only the `true` form.
const OPEN_OPTIONS_CREATE_NEW: &[&str] = &["std", "fs", "OpenOptions", "create_new"];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanFileBasedLocks {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized) || is_allowlisted(&normalized) {
            return;
        }

        match expr.kind {
            ExprKind::MethodCall(_, _, args, _) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    // OpenOptions::create_new(true) — only flag when the
                    // single argument is the literal `true`.
                    if def_path_equals(cx, def_id, OPEN_OPTIONS_CREATE_NEW) {
                        if let [arg] = args {
                            if is_literal_true(arg) {
                                emit_create_new_lint(cx, expr.span);
                            }
                        }
                        return;
                    }
                    for banned in BANNED_METHOD_PATHS {
                        if def_path_equals(cx, def_id, banned) {
                            emit_lint(cx, expr.span, banned);
                            return;
                        }
                    }
                }
            }
            ExprKind::Path(ref qpath) => {
                if let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) {
                    for banned in BANNED_METHOD_PATHS {
                        if def_path_equals(cx, def_id, banned) {
                            emit_lint(cx, expr.span, banned);
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_FILE_BASED_LOCKS,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` is a file-based lock; fbuild has no file-based locks (see \
                 CLAUDE.md \"Key Constraints\"). Route the lock through the daemon's \
                 in-memory manager."
            ));
        }),
    );
}

fn emit_create_new_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_FILE_BASED_LOCKS,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "`OpenOptions::create_new(true)` is the rename-or-fail lock-file pattern; \
                 fbuild has no file-based locks (see CLAUDE.md \"Key Constraints\"). \
                 Route the lock through the daemon's in-memory manager.",
            );
        }),
    );
}

fn is_literal_true(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Lit(ref lit) if matches!(lit.node, LitKind::Bool(true))
    )
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
        assert!(in_production_scope("crates/fbuild-daemon/src/lock.rs"));
    }

    #[test]
    fn in_production_scope_rejects_non_src() {
        assert!(!in_production_scope("crates/fbuild-daemon/tests/lock.rs"));
        assert!(!in_production_scope("ci/foo.py"));
    }
}
