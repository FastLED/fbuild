//! Neutral runtime-library support for third-party binaries fbuild installs.
//!
//! Vendor archives ship binaries that link against libraries the archive does
//! not carry: Linux ELF binaries with no `RPATH`, Windows PE binaries that
//! need a sibling DLL. Whether such a binary can start, what has to be staged
//! next to it, and which directories a child needs on its search path are all
//! host mechanics, so they live in the concrete per-OS tree and callers only
//! see these neutral answers.
//!
//! Callers keep the product policy: which archive, which library, how to
//! compose a search path, and what an unusable binary means for them.

use std::io;
use std::path::Path;

use crate::path::NormalizedPath;

/// What a `--version` probe of an installed binary learned about this host.
pub enum BinaryProbe {
    /// The binary started; its shared-library dependencies all resolved.
    Started,
    /// The dynamic linker could not satisfy a dependency (exit code 127).
    /// Carries the linker's own line when it could be recovered.
    MissingSharedLibrary(String),
    /// The probe produced no exit code to interpret: the binary could not be
    /// executed, or the run failed or timed out before one was available.
    ///
    /// Distinct from [`BinaryProbe::Inconclusive`] (which *did* get an exit
    /// code, just not one that means anything) because the two say different
    /// things about the runtime bundle. Collapsing "never produced a result"
    /// into the bundle's own failure mode is what made a failed `exec` report
    /// itself as a missing shared library, sending readers after binary
    /// dependencies for a problem that was never there
    /// (FastLED/fbuild#1366).
    ///
    /// Deliberately *not* split further into spawn-vs-timeout. The error
    /// crossing this boundary is already flattened to a string by
    /// `subprocess::spawn_err`, so telling them apart here would mean matching
    /// on message text — and inventing a distinction the code cannot actually
    /// make is precisely the bug above. The carried string is the underlying
    /// error, which says which it was.
    ///
    /// Treated as non-fatal exactly like `Inconclusive`: the real run reports
    /// it with full context.
    ProbeFailed(String),
    /// Probe could not be interpreted (some other non-127 exit). Treated as
    /// non-fatal: the real run reports it with full context.
    Inconclusive,
}

/// Probe `binary` by running it with `--version`.
///
/// `library_path`, when given, is exported to the child as this host's
/// dynamic-library search path so a caller can ask "does it start *with* the
/// bundled libraries applied?".
///
/// A host that resolves runtime libraries without a search-path variable, and
/// whose installed binaries are self-contained, has nothing to probe and
/// reports [`BinaryProbe::Started`].
pub fn probe_binary(binary: &Path, library_path: Option<&str>) -> BinaryProbe {
    super::selected::runtime::probe_binary(binary, library_path)
}

/// Name of the dynamic-library search-path variable this host's loader
/// honours, or `None` when the host resolves runtime libraries another way.
pub const fn library_path_var() -> Option<&'static str> {
    super::selected::runtime::LIBRARY_PATH_VAR
}

/// Stage the runtime libraries `binary` needs into its own directory.
///
/// A host whose installed binaries are self-contained reports `Ok(())`
/// without touching anything. A host that cannot supply a required library
/// reports [`io::ErrorKind::NotFound`]; the caller names what it was after.
pub fn stage_runtime_libraries(binary: &Path) -> io::Result<()> {
    super::selected::runtime::stage_runtime_libraries(binary)
}

/// Directories that must be on a child's search path so `binary` resolves its
/// runtime libraries.
///
/// Empty on a host that needs none. On a host that needs some, an empty list
/// with `Ok` would silently drop them, so an unresolvable runtime library is
/// [`io::ErrorKind::NotFound`] here instead.
pub fn runtime_library_dirs(binary: &Path) -> io::Result<Vec<NormalizedPath>> {
    super::selected::runtime::runtime_library_dirs(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The neutral probe must answer one of its own variants — never panic,
    /// and never claim a missing library for a binary it could not even run.
    #[test]
    fn probe_reports_a_variant_for_a_missing_binary() {
        let missing = std::env::temp_dir().join("fbuild-no-such-probe-binary-1315");
        match probe_binary(&missing, None) {
            BinaryProbe::Started
            | BinaryProbe::MissingSharedLibrary(_)
            | BinaryProbe::ProbeFailed(_)
            | BinaryProbe::Inconclusive => {}
        }
    }

    /// A host that stages nothing must still answer, and its empty directory
    /// list must be an honest "nothing needed" rather than a failure.
    #[test]
    fn staging_a_missing_binary_is_answerable() {
        let missing = std::env::temp_dir().join("fbuild-no-such-staged-binary-1315");
        let staged = stage_runtime_libraries(&missing);
        let dirs = runtime_library_dirs(&missing);
        assert!(
            staged.is_ok() || staged.unwrap_err().kind() == io::ErrorKind::NotFound,
            "staging must report success or NotFound"
        );
        assert!(
            dirs.is_ok() || dirs.unwrap_err().kind() == io::ErrorKind::NotFound,
            "directory lookup must report a list or NotFound"
        );
    }
}
