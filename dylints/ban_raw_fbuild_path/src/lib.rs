#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans string literals that spell the `.fbuild` directory segment by
    /// hand outside the explicit legacy allowlist. `fbuild_paths` is the
    /// single source of truth for every `.fbuild` path — call sites should
    /// use `fbuild_paths::{FBUILD_DIR_NAME, BUILD_DIR_NAME}`,
    /// `get_project_fbuild_dir()`, `get_project_build_root()`, or `BuildLayout`.
    ///
    /// ### Why is this bad?
    ///
    /// `crates/fbuild-paths/src/lib.rs` declares itself the single source of
    /// truth for all `.fbuild` paths, but the layout underneath it is not a
    /// fixed string: the env segment auto-collapses when a project has a
    /// single environment, `FBUILD_BUILD_DIR` overrides the root wholesale,
    /// and PlatformIO-style projects nest the tree under `.build/pio/<env>/`.
    /// A hardcoded `dir.join(".fbuild/build/uno/release")` encodes exactly
    /// one of those shapes. When the layout rules change, the literal keeps
    /// compiling and silently points at a directory that does not exist —
    /// `compile_cwd_from_output` and `BuildLayout` consumers then disagree
    /// about where the build lives. Routing through `fbuild_paths` keeps
    /// every consumer on one definition.
    ///
    /// ### Known problems
    ///
    /// This is a purely lexical match on string-literal *contents*: any
    /// literal containing `.fbuild` fires, including diagnostic messages and
    /// documentation strings that merely mention the directory. Those are
    /// legitimate and belong on `src/allowlist.txt` with a justification.
    /// Path segments assembled from non-literal pieces are not detected.
    ///
    /// ### Example
    ///
    /// ```rust
    /// # let project_dir = std::path::Path::new(".");
    /// let build_dir = project_dir.join(".fbuild").join("build");
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// # let project_dir = std::path::Path::new(".");
    /// let build_dir = fbuild_paths::get_project_build_root(project_dir);
    /// ```
    pub BAN_RAW_FBUILD_PATH,
    Deny,
    "ban raw '.fbuild' path literals outside the fbuild-paths source of truth"
}

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// The directory segment this lint protects. Kept as a byte-level needle so
/// the lint never has to depend on the workspace crates it polices.
const FBUILD_SEGMENT: &str = ".fbuild";

impl<'tcx> LateLintPass<'tcx> for BanRawFbuildPath {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let ExprKind::Lit(lit) = expr.kind else {
            return;
        };
        let LitKind::Str(sym, _) = lit.node else {
            return;
        };
        if !sym.as_str().contains(FBUILD_SEGMENT) {
            return;
        }
        if is_allowlisted(cx, expr.span) {
            return;
        }
        emit_lint(cx, expr.span);
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_RAW_FBUILD_PATH,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "raw '.fbuild' path literal: use fbuild_paths (FBUILD_DIR_NAME, \
                 BUILD_DIR_NAME, get_project_fbuild_dir, get_project_build_root, BuildLayout) \
                 instead of spelling the directory layout by hand",
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
    // The leading `./` keeps compiletest's `$DIR` replacement from matching
    // the `ui` inside diagnostic text.
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "./ui");
}
