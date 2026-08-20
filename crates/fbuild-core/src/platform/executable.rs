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
