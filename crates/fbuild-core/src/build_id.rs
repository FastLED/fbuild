//! Workspace build identity for `fbuild --version` and `fbuild daemon status`
//! (FastLED/fbuild#1742).
//!
//! The commit and dirty flag are stamped at compile time into the binaries
//! that report them (`fbuild-cli`, `fbuild-daemon`; see
//! `crates/fbuild-core/commit_stamp.rs`), not into this crate: a value that
//! changes on every commit, stamped this low in the graph, recompiled every
//! crate that depends on `fbuild-core`. Each binary's `main` installs its
//! stamp into [`STAMP`] with [`build_stamp!`](crate::build_stamp) before
//! anything reads it, so the CLI and every daemon built from one checkout
//! still report the same id.

use std::sync::OnceLock;

/// A binary's compile-time build identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildStamp {
    /// The checkout commit, shortened to 12 hex digits, or [`UNKNOWN`].
    pub commit: &'static str,
    /// Whether tracked files differed from `HEAD` when it was compiled.
    pub dirty: bool,
}

impl BuildStamp {
    /// What a process that installed no stamp reports (tests, tools).
    pub const UNSTAMPED: Self = Self {
        commit: UNKNOWN,
        dirty: false,
    };

    /// `<commit>`, `<commit>-dirty`, or `unknown`.
    pub fn build_id(self) -> String {
        format_build_id(self.commit, self.dirty)
    }
}

/// The running binary's stamp. Installed once, from `main`, by the binaries
/// that stamp one (`require_oncelock_install_before_use`).
pub static STAMP: OnceLock<BuildStamp> = OnceLock::new();

/// The calling crate's compile-time [`BuildStamp`]. A macro so that
/// `option_env!` reads the *caller's* build-script output, not this crate's.
#[macro_export]
macro_rules! build_stamp {
    () => {
        $crate::build_id::BuildStamp {
            commit: match option_env!("FBUILD_BUILD_COMMIT") {
                Some(commit) => commit,
                None => $crate::build_id::UNKNOWN,
            },
            dirty: matches!(option_env!("FBUILD_BUILD_DIRTY"), Some("1")),
        }
    };
}

/// The installed stamp, or [`BuildStamp::UNSTAMPED`].
pub fn stamp() -> BuildStamp {
    STAMP.get().copied().unwrap_or(BuildStamp::UNSTAMPED)
}

/// Placeholder when no checkout could be fingerprinted (tarball, no git).
pub const UNKNOWN: &str = "unknown";

/// SHA-256 of the assets compiled into fbuild (config JSONs, board files,
/// scripts), or [`UNKNOWN`]. Stamped by `build.rs` (FastLED/fbuild#2042).
pub const EMBEDDED_ASSETS_DIGEST: &str = match option_env!("FBUILD_EMBEDDED_ASSETS_DIGEST") {
    Some(digest) => digest,
    None => UNKNOWN,
};

/// The workspace version compiled into this binary.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// This binary's build id: `<commit>`, `<commit>-dirty`, or `unknown`.
pub fn build_id() -> String {
    stamp().build_id()
}

/// `--version` long form: the workspace version with the build id as semver
/// build metadata (`2.5.46+git.5fca4d49c9ab-dirty`).
///
/// `metadata` is deliberately semver-legal so `fbuild -V` stays the bare
/// version and any tool that parses the long form can still read the version.
pub fn long_version() -> String {
    format_long_version(version(), &build_id())
}

fn format_build_id(commit: &str, dirty: bool) -> String {
    if commit.is_empty() || commit == UNKNOWN {
        return UNKNOWN.to_string();
    }
    if dirty {
        format!("{commit}-dirty")
    } else {
        commit.to_string()
    }
}

fn format_long_version(version: &str, build_id: &str) -> String {
    format!("{version}+git.{build_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commit stamp changes on every commit. Stamped into this crate, it
    /// recompiled `fbuild-core` and so every one of the 19 workspace crates
    /// that depend on it, on every commit: each hosted clippy, test and
    /// Dylint pass rechecked the whole workspace whatever the commit touched.
    /// The binaries that report the id stamp it instead (see `BuildStamp`).
    #[test]
    fn fbuild_core_is_not_commit_stamped() {
        assert_eq!(option_env!("FBUILD_BUILD_COMMIT"), None);
        assert_eq!(option_env!("FBUILD_BUILD_DIRTY"), None);
    }

    #[test]
    fn an_unstamped_process_reports_unknown() {
        assert_eq!(BuildStamp::UNSTAMPED.build_id(), UNKNOWN);
    }

    #[test]
    fn a_stamp_formats_like_the_compile_time_one_did() {
        let stamp = BuildStamp {
            commit: "5fca4d49c9ab",
            dirty: true,
        };
        assert_eq!(stamp.build_id(), "5fca4d49c9ab-dirty");
    }

    #[test]
    fn clean_build_id_is_the_bare_commit() {
        assert_eq!(format_build_id("5fca4d49c9ab", false), "5fca4d49c9ab");
    }

    #[test]
    fn dirty_build_id_says_so() {
        assert_eq!(format_build_id("5fca4d49c9ab", true), "5fca4d49c9ab-dirty");
    }

    #[test]
    fn unstamped_builds_report_unknown_even_when_dirty() {
        assert_eq!(format_build_id(UNKNOWN, true), UNKNOWN);
        assert_eq!(format_build_id("", true), UNKNOWN);
    }

    #[test]
    fn long_version_keeps_the_workspace_version_readable() {
        assert_eq!(
            format_long_version("2.5.46", "5fca4d49c9ab"),
            "2.5.46+git.5fca4d49c9ab"
        );
        assert_eq!(
            format_long_version("2.5.46", "unknown"),
            "2.5.46+git.unknown"
        );
    }

    #[test]
    fn this_binary_reports_a_usable_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(long_version().starts_with(&format!("{}+git.", version())));
    }
}
