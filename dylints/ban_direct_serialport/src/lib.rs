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
    /// Bans direct references to items from the `serialport` crate in
    /// fbuild production code (`crates/*/src/`) outside
    /// `crates/fbuild-serial/` and a small allowlist of files that
    /// legitimately need raw serial access (diagnostic CLIs, the daemon
    /// device manager, and the fbuild-deploy bootloader paths).
    ///
    /// ### Why is this bad?
    ///
    /// All serial-port access in fbuild must flow through fbuild-serial's
    /// blessed APIs so the Windows USB-CDC contract (30-retry open,
    /// aggressive buffer drain, DTR/RTS toggling after flash) stays in
    /// one place. Direct `serialport::` usage scattered across crates
    /// silently regresses that invariant.
    ///
    /// ### Known problems
    ///
    /// Allowlisting is file-level via `src/allowlist.txt`. The lint does
    /// not detect `#[cfg(test)]` scope programmatically.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let port = serialport::new("/dev/ttyUSB0", 115200).open()?;
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// // From inside fbuild-serial, or via the manager API:
    /// let session = fbuild_serial::manager::SerialManager::open(...)?;
    /// ```
    pub BAN_DIRECT_SERIALPORT,
    Deny,
    "ban direct serialport crate references outside fbuild-serial"
}

/// The crate whose items are banned. Matching is on the first path
/// segment of the resolved DefId's def-path — i.e. the originating
/// crate name. This catches `use serialport::*`, `serialport::new(..)`,
/// `serialport::SerialPort` type refs, and re-exports that still resolve
/// back to a `serialport::*` DefId.
const BANNED_CRATE: &str = "serialport";

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Production-code scope. Only files whose path contains BOTH
/// `crates/` and a subsequent `/src/` segment are linted.
const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

/// Path fragment that unconditionally exempts a file: it IS the blessed
/// wrapper. Match the full directory boundary so a hypothetical
/// `crates/fbuild-serial-extras/` would still be linted.
const WRAPPER_DIR: &str = "crates/fbuild-serial/";

impl<'tcx> LateLintPass<'tcx> for BanDirectSerialport {
    fn check_ty(&mut self, cx: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        let filename = source_filename(cx, ty.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized)
            || is_wrapper_dir(&normalized)
            || is_allowlisted(&normalized)
        {
            return;
        }

        if let TyKind::Path(qpath) = ty.kind {
            let res = cx.qpath_res(&qpath, ty.hir_id);
            if res_is_from_banned_crate(cx, res) {
                emit_lint(cx, ty.span);
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);
        if !in_production_scope(&normalized)
            || is_wrapper_dir(&normalized)
            || is_allowlisted(&normalized)
        {
            return;
        }

        // Path-expression form: `serialport::new(...)`,
        // `serialport::SerialPortType::UsbPort(_)`, etc.
        if let ExprKind::Path(qpath) = expr.kind {
            let res = cx.qpath_res(&qpath, expr.hir_id);
            if res_is_from_banned_crate(cx, res) {
                emit_lint(cx, expr.span);
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_DIRECT_SERIALPORT,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "direct `serialport::*` reference outside fbuild-serial — route serial \
                 access through `fbuild_serial::manager` so the Windows USB-CDC \
                 contract (30-retry open, drain semantics, DTR/RTS rules) is applied. \
                 If raw access is truly justified for this file, allowlist it in \
                 `dylints/ban_direct_serialport/src/allowlist.txt` with a one-line \
                 reason.",
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

fn is_wrapper_dir(normalized: &str) -> bool {
    normalized.contains(WRAPPER_DIR)
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

fn res_is_from_banned_crate(cx: &LateContext<'_>, res: Res) -> bool {
    let Res::Def(_, def_id) = res else {
        return false;
    };
    let def_path = cx.get_def_path(def_id);
    if def_path.is_empty() {
        return false;
    }
    def_path[0] == Symbol::intern(BANNED_CRATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_production_scope_matches_src_files() {
        assert!(in_production_scope("crates/fbuild-cli/src/cli/port_scan.rs"));
        assert!(in_production_scope(
            "/home/runner/work/fbuild/crates/fbuild-deploy/src/teensy/mod.rs"
        ));
    }

    #[test]
    fn in_production_scope_rejects_non_src() {
        assert!(!in_production_scope("crates/fbuild-cli/tests/foo.rs"));
        assert!(!in_production_scope("crates/fbuild-cli/examples/demo.rs"));
        assert!(!in_production_scope("build.rs"));
    }

    #[test]
    fn wrapper_dir_is_recognised() {
        assert!(is_wrapper_dir("crates/fbuild-serial/src/manager.rs"));
        assert!(is_wrapper_dir(
            "/anywhere/crates/fbuild-serial/src/session.rs"
        ));
        assert!(!is_wrapper_dir("crates/fbuild-serial-extras/src/lib.rs"));
        assert!(!is_wrapper_dir("crates/fbuild-cli/src/cli/port_scan.rs"));
    }
}
