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
    /// Bans hand-rolled `.replace('\\', "/")` calls outside the explicit
    /// legacy allowlist. `fbuild_core::path::NormalizedPath::display_slash()`
    /// is the canonical primitive — every caller should delegate to it.
    ///
    /// ### Why is this bad?
    ///
    /// Every hand-rolled `path.to_string_lossy().replace('\\', "/")` is a
    /// re-implementation of what `NormalizedPath::display_slash()` already
    /// does, including the Windows UNC-prefix strip. Bugs
    /// FastLED/fbuild#875, #885, #890, and #912 were the same class:
    /// backslashes reached the compiler / linker with `\` intact and GCC's
    /// driver interpreted them as escape characters. Each fix added yet
    /// another manual `.replace('\\', "/")` at yet another call site. This
    /// lint makes the anti-pattern impossible to introduce.
    ///
    /// ### Known problems
    ///
    /// The receiver expression is not inspected — this is a syntactic
    /// match. The whole-workspace grep confirms this pattern is only ever
    /// used to slash-normalize paths, so the expected false-positive rate
    /// is near zero. Any true false-positive lands on `src/allowlist.txt`.
    ///
    /// ### Example
    ///
    /// ```rust
    /// let path = std::path::PathBuf::from(r"C:\foo\bar");
    /// let arg = path.to_string_lossy().replace('\\', "/");
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// use fbuild_core::path::NormalizedPath;
    /// let path = std::path::PathBuf::from(r"C:\foo\bar");
    /// let arg = NormalizedPath::from(path).display_slash();
    /// ```
    pub BAN_MANUAL_SLASH_NORMALIZE,
    Deny,
    "ban hand-rolled '.replace('\\\\', \"/\")' outside the primitive at fbuild_core::path"
}

const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for BanManualSlashNormalize {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if is_allowlisted(cx, expr.span) {
            return;
        }

        if let ExprKind::MethodCall(path_segment, _receiver, args, _) = expr.kind {
            if path_segment.ident.name.as_str() != "replace" {
                return;
            }
            if args.len() != 2 {
                return;
            }
            if is_backslash_char_lit(&args[0]) && is_forward_slash_str_lit(&args[1]) {
                emit_lint(cx, expr.span);
            }
        }
    }
}

fn is_backslash_char_lit(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Lit(lit) if matches!(lit.node, LitKind::Char('\\'))
    )
}

fn is_forward_slash_str_lit(expr: &Expr<'_>) -> bool {
    if let ExprKind::Lit(lit) = expr.kind {
        if let LitKind::Str(sym, _) = lit.node {
            return sym.as_str() == "/";
        }
    }
    false
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_MANUAL_SLASH_NORMALIZE,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "use fbuild_core::path::NormalizedPath::display_slash() instead of \
                 hand-rolled '.replace('\\\\', \"/\")' backslash-to-slash rewrite",
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

#[cfg(test)]
struct CurrentDirGuard(std::path::PathBuf);

#[cfg(test)]
impl CurrentDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::current_dir().expect("current dir should be readable");
        std::env::set_current_dir(path).expect("current dir should switch to manifest dir");
        Self(previous)
    }
}

#[cfg(test)]
fn prepare_dylint_library() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let status = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(manifest_dir)
        .status()
        .expect("cargo build should start");
    assert!(status.success(), "cargo build should succeed");

    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("RUSTUP_TOOLCHAIN should be set");
    let library_name = env!("CARGO_PKG_NAME").replace('-', "_");
    let target_debug = manifest_dir.join("target").join("debug");
    let expected = target_debug.join(format!(
        "{}{}@{}{}",
        std::env::consts::DLL_PREFIX,
        library_name,
        toolchain,
        std::env::consts::DLL_SUFFIX
    ));
    if expected.exists() {
        return;
    }

    let plain = target_debug.join(format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        library_name,
        std::env::consts::DLL_SUFFIX
    ));
    if plain.exists() {
        std::fs::copy(&plain, &expected)
            .expect("toolchain-suffixed dylint library should be copied");
        return;
    }

    let deps_dir = target_debug.join("deps");
    for entry in std::fs::read_dir(&deps_dir).expect("deps dir should be readable") {
        let path = entry.expect("deps entry should be readable").path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with(&format!("{}{}", std::env::consts::DLL_PREFIX, library_name))
            && name.ends_with(std::env::consts::DLL_SUFFIX)
        {
            std::fs::copy(&path, &expected)
                .expect("hashed dylint library should be copied to the expected filename");
            return;
        }
    }

    panic!(
        "could not find a built dylint library to copy into {}",
        expected.display()
    );
}

#[cfg(test)]
impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("current dir should be restored");
    }
}

#[test]
fn ui() {
    let _guard = CurrentDirGuard::set(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    prepare_dylint_library();
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
