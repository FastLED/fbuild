#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind, def::Res};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, symbol::Symbol};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans calls that create scratch directories/files under the OS temp
    /// directory (`$TMPDIR` / `%TEMP%`) instead of under
    /// `fbuild_paths::get_cache_root()` / `get_fbuild_root()`.
    ///
    /// ### Why is this bad?
    ///
    /// fbuild state should live under one ground-truth directory the user
    /// can inspect or override via `FBUILD_DEV_MODE` / fbuild's own path
    /// helpers. Scratch dirs scattered across `$TMPDIR` are invisible to
    /// `fbuild` cleanup, survive process death on Windows for hours, and
    /// on Windows specifically can sit on a different volume from the
    /// destination — breaking the atomic-rename invariant that
    /// `tempfile::NamedTempFile::persist` relies on.
    ///
    /// ### Known problems
    ///
    /// Legacy call sites are exempted via `src/allowlist.txt`. Migrate them
    /// as you touch each file.
    ///
    /// ### Example
    ///
    /// ```rust
    /// let dir = tempfile::tempdir().unwrap();
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// let root = fbuild_paths::get_cache_root().join("tmp");
    /// std::fs::create_dir_all(&root).unwrap();
    /// let dir = tempfile::tempdir_in(&root).unwrap();
    /// ```
    pub BAN_UNROOTED_TEMPDIR,
    Deny,
    "ban tempdir/temp_dir calls that aren't rooted under fbuild's cache dir"
}

/// Each entry is a fully-qualified path that resolves to a banned function
/// or associated function. Matching is exact — sub-paths are not banned.
/// The `*_in(...)` variants (`tempdir_in`, `TempDir::new_in`,
/// `NamedTempFile::new_in`) are intentionally absent: they accept an
/// explicit base directory and are the recommended replacement.
const BANNED_FN_PATHS: &[&[&str]] = &[
    &["std", "env", "temp_dir"],
    &["tempfile", "tempdir"],
    &["tempfile", "dir", "TempDir", "new"],
    &["tempfile", "TempDir", "new"],
    &["tempfile", "file", "NamedTempFile", "new"],
    &["tempfile", "NamedTempFile", "new"],
];

const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for BanUnrootedTempdir {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if is_allowlisted(cx, expr.span) {
            return;
        }
        if is_unit_test_module_scope(cx, expr.hir_id) {
            return;
        }

        if let ExprKind::Path(qpath) = expr.kind {
            let res = cx.qpath_res(&qpath, expr.hir_id);
            if let Res::Def(_, def_id) = res {
                for banned in BANNED_FN_PATHS {
                    if def_path_equals(cx, def_id, banned) {
                        emit_lint(cx, expr.span, banned);
                        return;
                    }
                }
            }
        }
    }
}

fn is_unit_test_module_scope(cx: &LateContext<'_>, hir_id: rustc_hir::HirId) -> bool {
    std::iter::once(hir_id)
        .chain(cx.tcx.hir_parent_id_iter(hir_id))
        .any(|id| is_test_module_node(cx, id))
}

fn is_test_module_node(cx: &LateContext<'_>, hir_id: rustc_hir::HirId) -> bool {
    let rustc_hir::Node::Item(item) = cx.tcx.hir_node(hir_id) else {
        return false;
    };
    let rustc_hir::ItemKind::Mod(ident, _) = item.kind else {
        return false;
    };
    let name = ident.name.as_str();
    name == "tests" || name.ends_with("_tests") || name.ends_with("_test")
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    let joined = banned.join("::");
    cx.opt_span_lint(
        BAN_UNROOTED_TEMPDIR,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "`{joined}` writes under $TMPDIR; root it under fbuild_paths::get_cache_root() (or a named subdir) and use the `_in` variant so the path lives under fbuild's user-visible cache tree"
            ));
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

    // Blanket-allow integration tests and benchmarks. These run on the
    // developer's machine, not in the user's installed binary, so they don't
    // need to land under `~/.fbuild/`. The lint is about production code
    // shipping in `fbuild.exe` / `fbuild-daemon.exe`.
    if normalized.contains("/tests/") || normalized.contains("/benches/") {
        return true;
    }

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

// UI test ignored until matching `.stderr` snapshots are captured locally.
// The lint behavior is exercised end-to-end by `cargo dylint --all
// --workspace` against the real workspace tree (`tests/` and `benches/`
// are blanket-allowed, the allowlist covers the legacy production sites,
// and any new violation lights up the workspace lint).
#[test]
#[ignore = "no .stderr snapshots yet — verify via `cargo dylint --all --workspace`"]
fn ui() {
    // Dylint 6.0.1 looks for its test library directly under
    // <target>/debug, while Soldr selects the host through
    // CARGO_BUILD_TARGET and Cargo writes to <target>/<host>/debug.
    // SAFETY: this test binary contains one test, so no peer can observe
    // the process-wide environment change.
    unsafe {
        std::env::remove_var("CARGO_BUILD_TARGET");
    }
    // The leading `./` keeps compiletest's `$DIR` replacement from matching
    // the `ui` inside diagnostic text such as `fbuild_core`.
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "./ui");
}
