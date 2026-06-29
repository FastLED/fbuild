#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::{Item, ItemKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `use tokio::fs::*` (and any subpath thereof) in
    /// production code, except inside the bridge module
    /// (`crates/fbuild-core/src/fs.rs`).
    ///
    /// ### Why is this bad?
    ///
    /// All async filesystem access in fbuild flows through
    /// `fbuild_core::fs::*` so the workspace has one source of truth
    /// for the tokio fs surface. Direct imports bypass the bridge and
    /// the cohesive surface starts to fragment.
    ///
    /// FastLED/fbuild#844 (bridge sweep, "Bridge pair 2").
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// use tokio::fs;        // banned
    /// use tokio::fs::File;  // banned
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// use fbuild_core::fs;
    /// use fbuild_core::fs::File;
    /// ```
    pub BAN_TOKIO_FS_DIRECT_IMPORT,
    Deny,
    "ban direct `use tokio::fs` imports outside the fbuild fs bridge"
}

const BRIDGE_MODULES: &[&str] = &["crates/fbuild-core/src/fs.rs"];

const ALLOWLIST: &str = include_str!("allowlist.txt");

const CRATES_PREFIX: &str = "crates/";
const SRC_SEGMENT: &str = "/src/";

impl<'tcx> LateLintPass<'tcx> for BanTokioFsDirectImport {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let filename = source_filename(cx, item.span);
        let normalized = normalize_slashes(&filename);

        if !in_production_scope(&normalized) {
            return;
        }
        if is_bridge_module(&normalized) || is_allowlisted(&normalized) {
            return;
        }

        if let ItemKind::Use(path, _kind) = &item.kind {
            let segs = path
                .segments
                .iter()
                .map(|s| s.ident.as_str().to_string())
                .collect::<Vec<_>>();
            // `use tokio::fs[::...]`. The resolver expands list-imports
            // (`use tokio::{fs, sync}`) into individual `UseKind::Single`
            // items in HIR — each one shows up here with its own
            // resolved path, so this single check covers all forms.
            if segs.len() >= 2 && segs[0] == "tokio" && segs[1] == "fs" {
                emit_lint(cx, item.span);
            }
        }
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_TOKIO_FS_DIRECT_IMPORT,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "direct `use tokio::fs` bypasses the fbuild fs bridge. Import \
                 from `fbuild_core::fs` instead so the workspace has one \
                 source of truth for the async filesystem surface. See \
                 FastLED/fbuild#844.",
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

fn is_bridge_module(normalized: &str) -> bool {
    BRIDGE_MODULES
        .iter()
        .any(|bridge| normalized.ends_with(bridge))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_module_recognized() {
        assert!(is_bridge_module("crates/fbuild-core/src/fs.rs"));
        assert!(!is_bridge_module("crates/fbuild-cli/src/cli/build.rs"));
    }
}
