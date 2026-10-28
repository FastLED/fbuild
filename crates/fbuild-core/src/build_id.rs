//! Workspace build identity for `fbuild --version` and `fbuild daemon status`
//! (FastLED/fbuild#1742).
//!
//! The values are stamped at compile time by this crate's `build.rs`, so the
//! CLI and every daemon built from the same checkout report the same id even
//! though they are separate binaries. See that script for why the probe lives
//! in `fbuild-core` rather than in each entry point.

/// The checkout commit, shortened to 12 hex digits, or [`UNKNOWN`].
pub const COMMIT: &str = match option_env!("FBUILD_BUILD_COMMIT") {
    Some(commit) => commit,
    None => UNKNOWN,
};

/// Whether tracked files differed from `HEAD` when the crate was compiled.
///
/// A function rather than a `const`: comparing a `&str` in a constant is not
/// allowed (`E0658`), so the comparison happens here instead.
pub fn dirty() -> bool {
    matches!(option_env!("FBUILD_BUILD_DIRTY"), Some("1"))
}

/// Placeholder when no checkout could be fingerprinted (tarball, no git).
pub const UNKNOWN: &str = "unknown";

/// The workspace version compiled into this binary.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// This binary's build id: `<commit>`, `<commit>-dirty`, or `unknown`.
pub fn build_id() -> String {
    format_build_id(COMMIT, dirty())
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

    #[test]
    fn commit_is_stamped_or_unknown() {
        assert!(
            COMMIT == UNKNOWN
                || (COMMIT.len() <= 12 && COMMIT.chars().all(|c| c.is_ascii_hexdigit())),
            "unexpected build commit {COMMIT:?}"
        );
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
