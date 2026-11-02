//! Busybox-style `argv[0]` dispatch (FastLED/fbuild#1851).
//!
//! One image, several names. The release still ships a `fbuild-daemon`
//! executable today (phase 1), but the implementation it runs is
//! [`fbuild_daemon::entry::run`] — the very same library entry `fbuild` itself
//! links. Once the packaging phases land, `fbuild-daemon` becomes a hardlink to
//! `fbuild` and this module is what makes that indistinguishable from the
//! standalone binary: it classifies `argv[0]` and takes the tiny fast path
//! *before* the 8 MB stack trampoline, clap's subcommand tree, and the tokio
//! runtime exist.
//!
//! Only `fbuild-daemon` is recognised. Anything else — including
//! `fbuild-daemon`-looking paths that are not exactly that stem — falls through
//! to today's CLI path unchanged.
//!
//! Why `argv[0]` and not a hidden subcommand: external callers (FastLED,
//! scripts, service definitions, the running-process broker) exec
//! `fbuild-daemon` by name, and fbuild identifies its daemon by exe stem
//! (`fbuild_paths::daemon_ownership::DAEMON_EXE_STEM`). A hidden subcommand
//! would give the process the stem `fbuild` and make it indistinguishable from
//! a CLI invocation.

/// Name (exe stem) that selects the daemon entry point.
const DAEMON_ALIAS: &str = "fbuild-daemon";

/// What `main` should do instead of running the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MulticallDispatch {
    /// The dispatch already ran to completion; exit with this code.
    Exit(i32),
}

/// Identity selected by `argv[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    FbuildDaemon,
}

/// Dispatch on this process's `argv[0]`, if it names another tool.
pub(crate) fn maybe_dispatch() -> Option<MulticallDispatch> {
    let argv0 = std::env::args_os().next()?;
    dispatch_for_classified(&argv0.to_string_lossy())
}

/// Classify a raw `argv[0]` string and run the corresponding entry point.
fn dispatch_for_classified(argv0: &str) -> Option<MulticallDispatch> {
    match classify_argv0(argv0)? {
        Identity::FbuildDaemon => Some(MulticallDispatch::Exit(fbuild_daemon::entry::run())),
    }
}

/// Classify `argv[0]` by file stem, case-insensitively.
fn classify_argv0(argv0: &str) -> Option<Identity> {
    let stem = argv0_stem(argv0)?;
    match stem.to_ascii_lowercase().as_str() {
        DAEMON_ALIAS => Some(Identity::FbuildDaemon),
        _ => None,
    }
}

/// File stem of `argv0`: last path component with any final extension removed.
///
/// Separators are stripped as text (`/` and `\`) rather than through
/// `std::path::Path`, so a Windows path classifies identically on a Unix host —
/// the dispatch rule must not depend on which platform is running the tests or
/// the wheels. Extension stripping is textual for the same reason: `Path`'s
/// `file_stem` only recognises the platform's own separator.
fn argv0_stem(argv0: &str) -> Option<&str> {
    let file_name = argv0.rsplit(['/', '\\']).next()?;
    let stem = match file_name.rfind('.') {
        // `dot > 0` keeps dotfiles (".exe") intact instead of emptying them.
        Some(dot) if dot > 0 => &file_name[..dot],
        _ => file_name,
    };
    (!stem.is_empty()).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_unix_daemon_path() {
        assert_eq!(
            classify_argv0("/usr/local/bin/fbuild-daemon"),
            Some(Identity::FbuildDaemon)
        );
    }

    #[test]
    fn classifies_windows_daemon_path_with_exe_and_any_case() {
        // Windows argv[0] arrives with backslashes and a `.EXE` suffix; the
        // comparison must be case-insensitive and extension-blind.
        assert_eq!(
            classify_argv0(r"C:\Users\zach\AppData\Roaming\Python\Scripts\FBUILD-DAEMON.EXE"),
            Some(Identity::FbuildDaemon)
        );
    }

    #[test]
    fn classifies_relocated_daemon_path() {
        // The relocated copy keeps the stem, so identity survives relocation
        // (#1851 step 6): `<cache root>/runtime/fbuild-daemon/<hash>/fbuild-daemon`.
        assert_eq!(
            classify_argv0("/home/u/cache/fbuild/runtime/fbuild-daemon/9f2b1c/fbuild-daemon"),
            Some(Identity::FbuildDaemon)
        );
    }

    #[test]
    fn classifies_bare_daemon_name_and_exe_suffix() {
        assert_eq!(
            classify_argv0("fbuild-daemon"),
            Some(Identity::FbuildDaemon)
        );
        assert_eq!(
            classify_argv0("fbuild-daemon.exe"),
            Some(Identity::FbuildDaemon)
        );
    }

    #[test]
    fn falls_through_for_the_cli_itself() {
        assert_eq!(classify_argv0("/x/fbuild"), None);
        assert_eq!(classify_argv0("/x/fbuild.exe"), None);
        assert_eq!(classify_argv0("fbuild"), None);
    }

    #[test]
    fn falls_through_for_empty_and_foreign_names() {
        assert_eq!(classify_argv0(""), None);
        assert_eq!(classify_argv0("/x/"), None);
        assert_eq!(classify_argv0("fbuild-daemon-helper"), None);
        assert_eq!(classify_argv0("my-fbuild-daemon"), None);
    }

    #[test]
    fn stem_strips_directories_and_the_final_extension_only() {
        assert_eq!(argv0_stem("/a/b/c.tool.exe"), Some("c.tool"));
        assert_eq!(argv0_stem(r"C:\a\b\tool.exe"), Some("tool"));
        assert_eq!(argv0_stem("tool"), Some("tool"));
        assert_eq!(argv0_stem(""), None);
    }
}
