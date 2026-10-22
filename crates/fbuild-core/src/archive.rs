//! GNU-compatible static archives with bounded command lines (#1656).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{FbuildError, Result, subprocess::run_command};

// Leave room below CreateProcess's 32,767 UTF-16-unit limit for quoting.
const RESPONSE_THRESHOLD: usize = 30_000;

struct Invocation {
    args: Vec<String>,
    // Keep the file alive through the subprocess, including failure/cancellation.
    _response: Option<tempfile::NamedTempFile>,
}

fn invocation(ar: &Path, objects: &[PathBuf], output: &Path) -> Result<Invocation> {
    let mut args = vec![
        ar.to_string_lossy().into_owned(),
        "rcs".into(),
        output.to_string_lossy().into_owned(),
    ];
    let paths: Vec<_> = objects
        .iter()
        .map(|object| object.to_string_lossy().into_owned())
        .collect();
    // Account conservatively for Windows argument quoting and escaping.
    let length: usize = args
        .iter()
        .chain(paths.iter())
        .map(|arg| {
            arg.encode_utf16()
                .count()
                .saturating_mul(2)
                .saturating_add(3)
        })
        .sum();
    if crate::platform::host::is_windows() || length >= RESPONSE_THRESHOLD {
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut response = tempfile::Builder::new()
            .prefix("fbuild-archive-")
            .suffix(".rsp")
            .tempfile_in(parent)?;
        for path in paths {
            // Unlike compiler -D flags, these are literal filenames. Preserve
            // apostrophes, quotes, whitespace and Unix backslashes exactly.
            let path = if crate::platform::host::is_windows() {
                path.replace('\\', "/")
            } else {
                path
            };
            let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
            writeln!(response, "\"{escaped}\"")?;
        }
        args.push(format!("@{}", response.path().display()));
        Ok(Invocation {
            args,
            _response: Some(response),
        })
    } else {
        args.extend(paths);
        Ok(Invocation {
            args,
            _response: None,
        })
    }
}

/// Replace an archive with exactly `objects`, preserving the caller's timeout
/// and diagnostic label. Response files are removed on every exit path.
pub async fn create(
    ar: &Path,
    objects: &[PathBuf],
    output: &Path,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let command = invocation(ar, objects, output)?;
    match std::fs::remove_file(output) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let args: Vec<_> = command.args.iter().map(String::as_str).collect();
    let result = run_command(&args, None, None, Some(timeout)).await?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "{label} failed: {}",
            result.stderr
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_invocation_is_short_and_response_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let objects: Vec<_> = (0..525)
            .map(|index| {
                tmp.path()
                    .join(format!("{} member's {index}.o", "x".repeat(100)))
            })
            .collect();
        let command = invocation(Path::new("ar"), &objects, &tmp.path().join("lib.a")).unwrap();
        assert_eq!(command.args.len(), 4);
        assert!(command.args.iter().map(String::len).sum::<usize>() < 1024);
        let response = command._response.as_ref().unwrap().path().to_path_buf();
        let content = std::fs::read_to_string(&response).unwrap();
        assert_eq!(content.lines().count(), 525);
        assert!(content.contains("member's 524.o\""));
        drop(command);
        assert!(!response.exists());
    }

    #[tokio::test]
    async fn response_is_removed_after_archiver_spawn_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let objects = vec![tmp.path().join("x".repeat(200)); 525];
        assert!(
            create(
                &tmp.path().join("missing-ar"),
                &objects,
                &tmp.path().join("lib.a"),
                "ar",
                Duration::from_secs(5)
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
}
