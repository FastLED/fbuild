//! Discover canonical GCC system headers without guessing a target's multilib.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use fbuild_core::path::{NormalizedPath, canonicalize_existing};
use fbuild_core::{FbuildError, Result};

const HEADER_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Preserve GCC's selected C++ search order while eliminating long `../` paths
/// that older Windows GCC builds cannot open (#1701).
pub async fn cxx_system_includes(
    compiler: &Path,
    flags: &[String],
    cwd: &Path,
) -> Result<Vec<NormalizedPath>> {
    let executable = NormalizedPath::from(compiler).display_slash();
    let mut args = vec![executable.as_str()];
    args.extend(target_options(flags));
    args.extend(["-E", "-x", "c++", "-v", "-"]);
    let result = fbuild_core::subprocess::run_command_with_stdin(
        &args,
        b"",
        Some(cwd),
        Some(&[("LC_ALL", "C")]),
        Some(HEADER_PROBE_TIMEOUT),
    )
    .await?;
    if !result.success() {
        return Err(FbuildError::PackageError(format!(
            "cannot discover GCC C++ system headers: {}",
            result.stderr
        )));
    }
    let cpath = std::env::var_os("CPATH");
    canonical_search_paths(&result.stderr, cwd, cpath.as_deref()).await
}

async fn canonical_search_paths(
    stderr: &str,
    cwd: &Path,
    cpath: Option<&OsStr>,
) -> Result<Vec<NormalizedPath>> {
    // CPATH is a user -I scope. Do not reclassify those paths as -isystem.
    let mut user_paths = Vec::new();
    if let Some(value) = cpath {
        for directory in std::env::split_paths(value) {
            if let Ok(path) = canonicalize_existing(cwd.join(directory)).await {
                user_paths.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    for reported in search_paths(stderr)? {
        let canonical = canonicalize_existing(cwd.join(reported)).await?;
        if !user_paths.contains(&canonical) && !paths.contains(&canonical) {
            paths.push(canonical);
        }
    }
    Ok(paths)
}

fn target_options(flags: &[String]) -> Vec<&str> {
    let mut options = Vec::new();
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let flag = flag.as_str();
        if matches!(flag, "--sysroot" | "-isysroot" | "-B" | "-imultilib") {
            options.push(flag);
            if let Some(value) = flags.next() {
                options.push(value.as_str());
            }
        } else if flag.starts_with("-m")
            || flag.starts_with("--sysroot=")
            || flag.starts_with("-isysroot")
            || flag.starts_with("-B")
            || matches!(flag, "-nostdinc" | "-nostdinc++")
        {
            options.push(flag);
        }
    }
    options
}

fn search_paths(stderr: &str) -> Result<Vec<&str>> {
    let mut in_list = false;
    let mut paths = Vec::new();
    for line in stderr.lines().map(str::trim) {
        if line == "#include <...> search starts here:" {
            in_list = true;
        } else if in_list && line == "End of search list." {
            return Ok(paths);
        } else if in_list && !line.is_empty() {
            paths.push(line);
        }
    }
    Err(FbuildError::PackageError(
        "GCC did not report a complete C++ include search list".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonicalizes_real_paths_without_reclassifying_user_headers() {
        let tmp = tempfile::tempdir().unwrap();
        for directory in ["bin", "project headers", "include cxx/target/thumb/hard"] {
            std::fs::create_dir_all(tmp.path().join(directory)).unwrap();
        }
        let log = "#include <...> search starts here:\nproject headers\nbin/../include cxx\nbin/../include cxx/target/thumb/hard\nEnd of search list.\n";
        let paths = canonical_search_paths(log, tmp.path(), Some(OsStr::new("project headers")))
            .await
            .unwrap();
        assert_eq!(
            paths,
            [
                canonicalize_existing(tmp.path().join("include cxx"))
                    .await
                    .unwrap(),
                canonicalize_existing(tmp.path().join("include cxx/target/thumb/hard"))
                    .await
                    .unwrap(),
            ]
        );
    }

    #[test]
    fn preserves_multilib_and_space_containing_paths_in_driver_order() {
        let log = "ignoring nonexistent directory /old/sdk\r\n#include \"...\" search starts here:\r\n#include <...> search starts here:\r\n C:/SDK root/bin/../include/c++\r\n C:/SDK root/include/c++/target/thumb/hard\r\nEnd of search list.\r\nignored compiler statistics\r\n";
        assert_eq!(
            search_paths(log).unwrap(),
            [
                "C:/SDK root/bin/../include/c++",
                "C:/SDK root/include/c++/target/thumb/hard"
            ]
        );
        assert!(search_paths("#include <...> search starts here:\n/truncated").is_err());
        assert!(
            search_paths("#include <...> search starts here:\nEnd of search list.")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn probes_effective_target_settings_without_dependency_output_or_user_includes() {
        let flags = [
            "-Iproject",
            "-MMD",
            "-mcpu=cortex-m4",
            "-mfloat-abi=hard",
            "--sysroot",
            "SDK root",
            "-nostdinc++",
            "-Bprefix",
            "-isysroot",
            "other SDK",
        ]
        .map(str::to_owned);
        assert_eq!(
            target_options(&flags),
            [
                "-mcpu=cortex-m4",
                "-mfloat-abi=hard",
                "--sysroot",
                "SDK root",
                "-nostdinc++",
                "-Bprefix",
                "-isysroot",
                "other SDK"
            ]
        );
    }
}
