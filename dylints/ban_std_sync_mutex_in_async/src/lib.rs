#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{def::Res, AmbigArg, Expr, ExprKind, HirId, Ty, TyKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{
    symbol::{sym, Symbol},
    FileName, RemapPathScopeComponents,
};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `std::sync::Mutex<T>` and `std::sync::RwLock<T>` type
    /// uses inside files under `crates/fbuild-daemon/src/**` and
    /// `crates/fbuild-serial/src/**`. Test files (`*tests*.rs`) and
    /// files explicitly listed in `src/allowlist.txt` are exempt.
    ///
    /// ### Why is this bad?
    ///
    /// Holding an `std::sync::Mutex` guard across an `.await` point
    /// is two bugs at once:
    ///
    /// 1. **Scheduler starvation.** The mutex doesn't know about
    ///    Tokio's futures; while the guard is held the executor
    ///    can't park the task. Other tasks on the same worker
    ///    thread stall.
    /// 2. **Poison panic.** A panic inside an async section that's
    ///    holding the guard poisons the mutex; every subsequent
    ///    `.lock()` call panics. One panic per type makes the whole
    ///    subsystem unusable in a long-running daemon.
    ///
    /// The fix is `tokio::sync::Mutex` / `tokio::sync::RwLock` (or
    /// `parking_lot::Mutex` for hot non-async paths that never cross
    /// an await), or restructuring so the std lock is released
    /// *before* any await.
    ///
    /// ### Known problems
    ///
    /// - The lint matches the *type position*, not the actual
    ///   `.lock()` / `.read()` / `.write()` call. A file that
    ///   declares a struct field of type `std::sync::Mutex<T>` will
    ///   trip the lint even if the struct is never accessed from an
    ///   async context. That's the right *direction* though — if a
    ///   type lives in the daemon's tree it can flow into async code
    ///   later, and the lint is loud at type-introduction time so
    ///   the conversation happens early.
    /// - Path aliases / re-exports that resolve back to
    ///   `std::sync::Mutex` are caught by `qpath_res` /
    ///   `get_def_path`.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// // crates/fbuild-daemon/src/some_async_path.rs
    /// use std::sync::Mutex;       // ← caught
    /// struct State { inner: std::sync::Mutex<Inner> } // ← caught
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// use tokio::sync::Mutex;
    /// struct State { inner: tokio::sync::Mutex<Inner> }
    /// ```
    pub BAN_STD_SYNC_MUTEX_IN_ASYNC,
    Deny,
    "ban std::sync::Mutex / std::sync::RwLock in daemon / serial code"
}

/// Banned canonical def-paths. Matched exactly against the resolved
/// `DefId`'s def-path. Listed both with and without the rustc-internal
/// `poison` submodule because both shapes can appear in the
/// `get_def_path` output across rustc nightlies.
const BANNED_PATHS: &[&[&str]] = &[
    &["std", "sync", "Mutex"],
    &["std", "sync", "RwLock"],
    &["std", "sync", "poison", "Mutex"],
    &["std", "sync", "poison", "RwLock"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Scope: any file path matching one of these directory prefixes.
const SCOPES: &[&str] = &[
    "crates/fbuild-daemon/src/",
    "crates/fbuild-serial/src/",
];

impl<'tcx> LateLintPass<'tcx> for BanStdSyncMutexInAsync {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        let filename = source_filename(cx, ty.span);
        let normalized = normalize_slashes(&filename);
        if !in_scope(&normalized) || is_test_file(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if owned_by_cfg_test_module(cx, ty.hir_id) {
            return;
        }
        if let TyKind::Path(qpath) = ty.kind {
            let res = cx.qpath_res(&qpath, ty.hir_id);
            if let Some(banned) = res_banned(cx, res) {
                emit_lint(cx, ty.span, banned);
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_scope(&normalized) || is_test_file(&normalized) || is_allowlisted(&normalized) {
            return;
        }
        if owned_by_cfg_test_module(cx, expr.hir_id) {
            return;
        }
        // Catches `std::sync::Mutex::new(...)` constructor calls — the
        // type-position match handles field/variable declarations, this
        // arm catches the path-as-callee form.
        if let ExprKind::Path(ref qpath) = expr.kind {
            let res = cx.qpath_res(qpath, expr.hir_id);
            if let Some(banned) = res_banned(cx, res) {
                emit_lint(cx, expr.span, banned);
            }
        }
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

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_STD_SYNC_MUTEX_IN_ASYNC,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` in an async-reachable fbuild crate (daemon / serial) — \
                 holding a `std::sync::Mutex` / `RwLock` guard across `.await` starves \
                 the Tokio worker and a panic poisons the lock for every subsequent \
                 caller. Use `tokio::sync::Mutex` / `tokio::sync::RwLock` (or \
                 `parking_lot::Mutex` for hot non-async paths that never cross an await), \
                 or restructure so the lock is released before any `.await`. If this \
                 file is a genuinely-synchronous hot path that should keep using \
                 `std::sync`, allowlist it in \
                 `dylints/ban_std_sync_mutex_in_async/src/allowlist.txt` with a one-line \
                 reason."
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

fn is_test_file(normalized: &str) -> bool {
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

fn res_banned(cx: &LateContext<'_>, res: Res) -> Option<&'static [&'static str]> {
    let Res::Def(_, def_id) = res else {
        return None;
    };
    let def_path = cx.get_def_path(def_id);
    for banned in BANNED_PATHS {
        if def_path_equals_path(&def_path, banned) {
            return Some(banned);
        }
    }
    None
}

fn def_path_equals_path(def_path: &[Symbol], expected: &[&str]) -> bool {
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
    fn daemon_and_serial_src_are_in_scope() {
        assert!(in_scope("crates/fbuild-daemon/src/context.rs"));
        assert!(in_scope(
            "crates/fbuild-daemon/src/handlers/operations/build.rs"
        ));
        assert!(in_scope("crates/fbuild-serial/src/manager.rs"));
    }

    #[test]
    fn other_crates_are_out_of_scope() {
        assert!(!in_scope("crates/fbuild-cli/src/cli/build.rs"));
        assert!(!in_scope("crates/fbuild-core/src/lib.rs"));
        assert!(!in_scope("crates/fbuild-daemon/tests/foo.rs"));
    }

    #[test]
    fn test_files_in_scope_are_exempt() {
        assert!(is_test_file(
            "crates/fbuild-daemon/src/handlers/websockets_tests.rs"
        ));
        assert!(is_test_file("crates/fbuild-serial/src/manager/tests.rs"));
        assert!(!is_test_file("crates/fbuild-serial/src/manager.rs"));
    }
}
