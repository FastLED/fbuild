//! Neutral executable naming, discovery, and materialization APIs.

use super::host::{self, HostArch, HostPlatform};
use crate::path::NormalizedPath;
use std::io;
use std::path::{Component, Path};

/// Select the spelling of an executable or command script for an explicit host.
pub const fn name_for<'a>(host: HostPlatform, non_windows: &'a str, windows: &'a str) -> &'a str {
    if host.is_windows() {
        windows
    } else {
        non_windows
    }
}

/// Select the spelling of an executable or command script for the current host.
pub const fn name<'a>(non_windows: &'a str, windows: &'a str) -> &'a str {
    name_for(
        HostPlatform::new(host::current_os(), HostArch::Other),
        non_windows,
        windows,
    )
}

/// Add the native executable suffix to a tool stem for an explicit host.
pub fn native_name_for(host: HostPlatform, stem: &str) -> String {
    if host.is_windows() {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// Add the native executable suffix to a tool stem for the current host.
pub fn native_name(stem: &str) -> String {
    native_name_for(host::current(), stem)
}

/// Return ordered PATH/PATHEXT-compatible spellings for an explicit host.
pub fn path_candidate_names_for(host: HostPlatform, stem: &str) -> Vec<String> {
    if host.is_windows() {
        vec![format!("{stem}.exe"), stem.to_owned()]
    } else {
        vec![stem.to_owned()]
    }
}

/// Return ordered PATH/PATHEXT-compatible spellings for the current host.
pub fn path_candidate_names(stem: &str) -> Vec<String> {
    path_candidate_names_for(host::current(), stem)
}

/// Return the ordered file names probed for a package-provided host tool.
///
/// The host-native spelling comes first, then the cosmocc default `.com`
/// suffix, then the other host's spelling. Non-native spellings are only
/// accepted by [`find_tool_in_for`] when the file is an APE image, which runs
/// on every host.
pub fn tool_candidate_names_for(host: HostPlatform, stem: &str) -> Vec<String> {
    if host.is_windows() {
        vec![
            format!("{stem}.exe"),
            format!("{stem}.com"),
            stem.to_owned(),
        ]
    } else {
        vec![
            stem.to_owned(),
            format!("{stem}.com"),
            format!("{stem}.exe"),
        ]
    }
}

/// Return the ordered file names probed for a host tool on the current host.
pub fn tool_candidate_names(stem: &str) -> Vec<String> {
    tool_candidate_names_for(host::current(), stem)
}

/// Find a package-provided tool named `stem` in `dir` for an explicit host.
///
/// Returns the first candidate from [`tool_candidate_names_for`] that is a
/// regular file and runnable on `host`: the host-native spelling is always
/// accepted, any other spelling only when it is an APE image (so a
/// Windows-only PE `tool.exe` is not picked on Linux, and an extensionless
/// shell script is not picked on Windows).
pub fn find_tool_in_for(host: HostPlatform, dir: &Path, stem: &str) -> Option<NormalizedPath> {
    let native = native_name_for(host, stem);
    tool_candidate_names_for(host, stem)
        .into_iter()
        .map(|name| {
            let is_native = name == native;
            (dir.join(name), is_native)
        })
        .find(|(path, is_native)| path.is_file() && (*is_native || super::ape::is_ape_file(path)))
        .map(|(path, _)| NormalizedPath::from(path))
}

/// Find a package-provided tool named `stem` in `dir` on the current host.
pub fn find_tool_in(dir: &Path, stem: &str) -> Option<NormalizedPath> {
    find_tool_in_for(host::current(), dir, stem)
}

/// Find a tool named `stem` anywhere below `root` (depth-first), applying
/// [`find_tool_in`]'s acceptance rules in each directory. Used for archives
/// whose binary sits under an unknown nesting of top-level folders.
pub fn find_tool_in_tree(root: &Path, stem: &str) -> Option<NormalizedPath> {
    if let Some(found) = find_tool_in(root, stem) {
        return Some(found);
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .find_map(|dir| find_tool_in_tree(&dir, stem))
}

/// Find a tool on `PATH` (or any list of search directories) by stem, using
/// [`find_tool_in`]'s per-directory candidate order and acceptance rules.
pub fn find_tool_on_paths<I>(dirs: I, stem: &str) -> Option<NormalizedPath>
where
    I: IntoIterator,
    I::Item: AsRef<Path>,
{
    dirs.into_iter()
        .find_map(|dir| find_tool_in(dir.as_ref(), stem))
}

/// Resolve a package-provided tool in `dir` for an explicit host, falling back
/// to the host-native spelling when no acceptable candidate exists.
pub fn resolve_tool_in_for(host: HostPlatform, dir: &Path, stem: &str) -> NormalizedPath {
    find_tool_in_for(host, dir, stem)
        .unwrap_or_else(|| NormalizedPath::from(dir.join(native_name_for(host, stem))))
}

/// Resolve a package-provided tool in `dir` on the current host.
///
/// Returns [`find_tool_in`]'s hit, or `dir/<native name>` when nothing usable
/// exists so callers keep reporting the conventional missing path.
pub fn resolve_tool_in(dir: &Path, stem: &str) -> NormalizedPath {
    resolve_tool_in_for(host::current(), dir, stem)
}

/// Split a host-tool file name into its stem and executable suffix.
///
/// Recognizes `.exe` and `.com` (case-insensitive); the returned suffix keeps
/// its leading dot and original case, or is empty when there is none.
pub fn split_tool_suffix(file_name: &str) -> (&str, &str) {
    for suffix in [".exe", ".com"] {
        let Some(split) = file_name.len().checked_sub(suffix.len()) else {
            continue;
        };
        if split > 0
            && file_name.is_char_boundary(split)
            && file_name[split..].eq_ignore_ascii_case(suffix)
        {
            return (&file_name[..split], &file_name[split..]);
        }
    }
    (file_name, "")
}

/// Discover the path of the currently running executable image.
pub fn current_image() -> io::Result<NormalizedPath> {
    std::env::current_exe().map(NormalizedPath::from)
}

/// Return a path next to the current executable image.
pub fn current_image_sibling(name: impl AsRef<Path>) -> io::Result<NormalizedPath> {
    let name = name.as_ref();
    let mut components = name.components();
    if !matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "current executable sibling name must be exactly one file-name component",
        ));
    }

    let image = current_image()?;
    let parent = image.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "current executable image has no parent directory",
        )
    })?;
    Ok(parent.join(name).into())
}

/// Return the conventional unsuffixed and `.exe` sibling candidates.
///
/// Probing both preserves compatibility with archives that carry an explicit
/// Windows suffix even when inspected from another host.
pub fn current_image_sibling_candidates(stem: &str) -> io::Result<[NormalizedPath; 2]> {
    let unsuffixed = current_image_sibling(stem)?;
    let explicit_exe = NormalizedPath::from(unsuffixed.with_extension("exe"));
    Ok([unsuffixed, explicit_exe])
}

#[cfg(test)]
mod tests {
    use crate::path::NormalizedPath;
    use crate::platform::host::{HostArch, HostOs, HostPlatform};

    #[test]
    fn executable_and_command_script_names_follow_the_explicit_host() {
        let windows = HostPlatform::new(HostOs::Windows, HostArch::X86_64);
        let linux = HostPlatform::new(HostOs::Linux, HostArch::X86_64);

        assert_eq!(super::name_for(windows, "clang", "clang.exe"), "clang.exe");
        assert_eq!(super::name_for(linux, "clang", "clang.exe"), "clang");
        assert_eq!(super::name_for(windows, "npm", "npm.cmd"), "npm.cmd");
        assert_eq!(super::name_for(linux, "npm", "npm.cmd"), "npm");
        assert_eq!(super::native_name_for(windows, "tool"), "tool.exe");
        assert_eq!(super::native_name_for(linux, "tool"), "tool");
        assert_eq!(
            super::path_candidate_names_for(windows, "pio"),
            ["pio.exe", "pio"]
        );
        assert_eq!(super::path_candidate_names_for(linux, "pio"), ["pio"]);
    }

    const WINDOWS: HostPlatform = HostPlatform::new(HostOs::Windows, HostArch::X86_64);
    const LINUX: HostPlatform = HostPlatform::new(HostOs::Linux, HostArch::X86_64);
    const MACOS_ARM: HostPlatform = HostPlatform::new(HostOs::Macos, HostArch::Aarch64);

    fn write_ape(path: &std::path::Path) {
        std::fs::write(path, b"MZqFpD='\n#!/bin/sh\n").expect("write fake APE");
    }

    fn write_plain(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("write plain file");
    }

    #[test]
    fn tool_candidate_names_put_the_native_spelling_first_then_com() {
        assert_eq!(
            super::tool_candidate_names_for(WINDOWS, "gcc"),
            ["gcc.exe", "gcc.com", "gcc"]
        );
        assert_eq!(
            super::tool_candidate_names_for(LINUX, "gcc"),
            ["gcc", "gcc.com", "gcc.exe"]
        );
        assert_eq!(
            super::tool_candidate_names_for(MACOS_ARM, "gcc"),
            ["gcc", "gcc.com", "gcc.exe"]
        );
    }

    #[test]
    fn find_tool_prefers_the_native_spelling_over_an_ape() {
        let dir = tempfile::tempdir().unwrap();
        write_plain(&dir.path().join("tool"), b"\x7fELF");
        write_ape(&dir.path().join("tool.com"));
        assert_eq!(
            super::find_tool_in_for(LINUX, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool")))
        );
    }

    #[test]
    fn find_tool_accepts_ape_com_and_exe_on_unix_hosts() {
        let dir = tempfile::tempdir().unwrap();
        write_ape(&dir.path().join("tool.com"));
        assert_eq!(
            super::find_tool_in_for(LINUX, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool.com")))
        );

        let dir = tempfile::tempdir().unwrap();
        write_ape(&dir.path().join("tool.exe"));
        assert_eq!(
            super::find_tool_in_for(MACOS_ARM, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool.exe")))
        );
    }

    #[test]
    fn find_tool_refuses_a_non_ape_windows_pe_on_unix_hosts() {
        let dir = tempfile::tempdir().unwrap();
        write_plain(&dir.path().join("tool.exe"), b"MZ\x90\x00\x03\x00\x00\x00");
        assert_eq!(super::find_tool_in_for(LINUX, dir.path(), "tool"), None);
        assert_eq!(
            super::resolve_tool_in_for(LINUX, dir.path(), "tool"),
            NormalizedPath::from(dir.path().join("tool"))
        );
    }

    #[test]
    fn find_tool_on_windows_prefers_exe_then_ape_com_and_refuses_plain_scripts() {
        let dir = tempfile::tempdir().unwrap();
        write_plain(&dir.path().join("tool"), b"#!/bin/sh\n");
        assert_eq!(super::find_tool_in_for(WINDOWS, dir.path(), "tool"), None);
        assert_eq!(
            super::resolve_tool_in_for(WINDOWS, dir.path(), "tool"),
            NormalizedPath::from(dir.path().join("tool.exe"))
        );

        write_ape(&dir.path().join("tool.com"));
        assert_eq!(
            super::find_tool_in_for(WINDOWS, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool.com")))
        );

        write_plain(&dir.path().join("tool.exe"), b"MZ\x90\x00");
        assert_eq!(
            super::find_tool_in_for(WINDOWS, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool.exe")))
        );
    }

    #[test]
    fn find_tool_accepts_an_extensionless_ape_on_windows_and_skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tool.exe")).unwrap();
        write_ape(&dir.path().join("tool"));
        assert_eq!(
            super::find_tool_in_for(WINDOWS, dir.path(), "tool"),
            Some(NormalizedPath::from(dir.path().join("tool")))
        );
    }

    #[test]
    fn split_tool_suffix_recognizes_exe_and_com() {
        assert_eq!(super::split_tool_suffix("gcc.exe"), ("gcc", ".exe"));
        assert_eq!(super::split_tool_suffix("gcc.COM"), ("gcc", ".COM"));
        assert_eq!(super::split_tool_suffix("gcc"), ("gcc", ""));
        assert_eq!(super::split_tool_suffix(".exe"), (".exe", ""));
    }

    #[test]
    fn current_image_and_sibling_discovery_share_the_same_parent() {
        let image = super::current_image().expect("current test image");
        let sibling = super::current_image_sibling("fbuild-sibling").expect("sibling path");
        assert_eq!(sibling.parent(), image.parent());
        assert_eq!(
            sibling.file_name().and_then(|name| name.to_str()),
            Some("fbuild-sibling")
        );
        let candidates =
            super::current_image_sibling_candidates("fbuild-daemon").expect("candidate paths");
        assert_eq!(
            candidates[0].file_name().and_then(|name| name.to_str()),
            Some("fbuild-daemon")
        );
        assert_eq!(
            candidates[1].file_name().and_then(|name| name.to_str()),
            Some("fbuild-daemon.exe")
        );
    }

    #[test]
    fn current_image_sibling_rejects_paths_that_can_escape_the_image_directory() {
        for invalid in [
            std::path::Path::new("/tmp/other"),
            std::path::Path::new("../other"),
        ] {
            let error = super::current_image_sibling(invalid).expect_err("reject non-sibling path");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
