//! GNU-compatible static archives with bounded command lines (#1656).

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::{
    FbuildError, Result, disk_full,
    subprocess::{ToolOutput, run_command},
};

// Leave room below CreateProcess's 32,767 UTF-16-unit limit for quoting.
const RESPONSE_THRESHOLD: usize = 30_000;

/// `D` pins deterministic output: uid, gid and mtime are written as zero, so
/// two archives holding identical members are byte-identical on every host.
/// A distro `ar` configured without deterministic defaults (or one honouring
/// `POSIXLY_CORRECT`) otherwise stamps the members' metadata and a
/// served-vs-source comparison can never agree (#2196 archive parity).
/// `r` insert, `c` create, `s` symbol index.
const MODIFIERS: &str = "Drcs";

/// Some vendor archivers reject `D`. Writing an archive the host can read
/// matters more than byte parity, so a rejected modifier falls back to the
/// plain set once.
const MODIFIERS_FALLBACK: &str = "rcs";

/// Does this archiver refusal mean it does not understand `D`?
fn rejects_deterministic_modifier(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    (lowered.contains("modifier")
        || lowered.contains("invalid option")
        || lowered.contains("usage:"))
        && !lowered.contains("no such file")
}

struct Invocation {
    args: Vec<String>,
    // Keep the file alive through the subprocess, including failure/cancellation.
    _response: Option<tempfile::NamedTempFile>,
}

fn invocation(
    ar: &Path,
    objects: &[impl AsRef<Path>],
    output: &Path,
    modifiers: &str,
) -> Result<Invocation> {
    let mut args = vec![
        ar.to_string_lossy().into_owned(),
        modifiers.into(),
        output.to_string_lossy().into_owned(),
    ];
    let paths: Vec<_> = objects
        .iter()
        .map(|object| object.as_ref().to_string_lossy().into_owned())
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
                crate::path::NormalizedPath::from(path).display_slash()
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
    objects: &[impl AsRef<Path>],
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
    let first = run_archiver(ar, objects, output, MODIFIERS, timeout).await?;
    if first.success() {
        return Ok(());
    }
    if rejects_deterministic_modifier(&first.stderr) {
        let retried = run_archiver(ar, objects, output, MODIFIERS_FALLBACK, timeout).await?;
        if retried.success() {
            return Ok(());
        }
        return Err(archiver_failure(&retried, label, output));
    }
    Err(archiver_failure(&first, label, output))
}

/// One archiver attempt. The output is removed first so `r` cannot leave a
/// stale member behind, and the response file lives until the child exits.
async fn run_archiver(
    ar: &Path,
    objects: &[impl AsRef<Path>],
    output: &Path,
    modifiers: &str,
    timeout: Duration,
) -> Result<ToolOutput> {
    let command = invocation(ar, objects, output, modifiers)?;
    match std::fs::remove_file(output) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let args: Vec<_> = command.args.iter().map(String::as_str).collect();
    let result = run_command(&args, None, None, Some(timeout)).await?;
    drop(command);
    Ok(result)
}

fn archiver_failure(result: &ToolOutput, label: &str, output: &Path) -> FbuildError {
    // A full volume is not an archiver error (FastLED/fbuild#1750 item 4):
    // say what it is instead of forwarding the raw ENOSPC text.
    if let Some(signature) = disk_full::enospc_signature(&result.stderr) {
        return disk_full::disk_full_error(
            &format!("{label} (archive {})", output.display()),
            signature,
        );
    }
    FbuildError::BuildFailed(format!("{label} failed: {}", result.stderr))
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
        let command = invocation(
            Path::new("ar"),
            &objects,
            &tmp.path().join("lib.a"),
            MODIFIERS,
        )
        .unwrap();
        assert_eq!(command.args.len(), 4);
        assert!(command.args.iter().map(String::len).sum::<usize>() < 1024);
        let response = command._response.as_ref().unwrap().path().to_path_buf();
        let content = std::fs::read_to_string(&response).unwrap();
        assert_eq!(content.lines().count(), 525);
        assert!(content.contains("member's 524.o\""));
        drop(command);
        assert!(!response.exists());
    }

    /// Byte parity of two archives of identical members is what #2196 rests on,
    /// so the shared primitive must ask for it rather than inherit whatever
    /// default the host archiver was built with.
    #[test]
    fn the_archiver_is_asked_for_deterministic_output() {
        let tmp = tempfile::tempdir().unwrap();
        let object = tmp.path().join("u.o");
        std::fs::write(&object, b"obj").unwrap();
        let command = invocation(
            Path::new("ar"),
            &[object],
            &tmp.path().join("lib.a"),
            MODIFIERS,
        )
        .unwrap();
        assert_eq!(command.args[1], "Drcs");
    }

    #[test]
    fn only_modifier_refusals_are_retried() {
        assert!(rejects_deterministic_modifier("ar: invalid modifier 'D'"));
        assert!(rejects_deterministic_modifier(
            "llvm-ar: error: unknown modifier"
        ));
        assert!(rejects_deterministic_modifier(
            "Usage: ar [emulation options]"
        ));
        assert!(!rejects_deterministic_modifier(
            "ar: missing-object.o: No such file"
        ));
        assert!(!rejects_deterministic_modifier(
            "ar: libcore.a: No space left"
        ));
    }

    /// An archiver that refuses `D` still has to produce an archive: the
    /// fallback fires exactly once and the caller sees success.
    #[tokio::test]
    async fn an_archiver_that_rejects_d_still_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("ar-no-deterministic");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$1\" in *D*) echo \"ar: invalid modifier 'D'\" >&2; exit 1 ;; esac\nprintf 'archive %s\\n' \"$2\" > \"$2\"\n",
        )
        .unwrap();
        crate::platform::fs::set_executable(&script).unwrap();
        let object = tmp.path().join("u.o");
        std::fs::write(&object, b"obj").unwrap();
        let out = tmp.path().join("lib.a");
        create(&script, &[object], &out, "ar", Duration::from_secs(30))
            .await
            .unwrap();
        assert!(out.exists(), "the fallback produced no archive");
    }

    /// A genuine archiver error is reported, not retried forever.
    #[tokio::test]
    async fn a_real_archiver_failure_is_not_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("ar-broken");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'ar: libcore.a: permission denied' >&2\nexit 1\n",
        )
        .unwrap();
        crate::platform::fs::set_executable(&script).unwrap();
        let object = tmp.path().join("u.o");
        std::fs::write(&object, b"obj").unwrap();
        let error = create(
            &script,
            &[object],
            &tmp.path().join("lib.a"),
            "ar",
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();
        assert!(format!("{error}").contains("permission denied"), "{error}");
    }

    /// The property #2196 builds on, asserted through the production primitive:
    /// identical members written under different mtimes archive identically.
    #[tokio::test]
    async fn identical_members_archive_identically() {
        let tmp = tempfile::tempdir().unwrap();
        let mut archives = Vec::new();
        for arm in ["a", "b"] {
            let dir = tmp.path().join(arm);
            std::fs::create_dir_all(&dir).unwrap();
            let mut objects = Vec::new();
            for index in 0..2u8 {
                let object = dir.join(format!("unit_{index}.o"));
                std::fs::write(&object, [b'o', b'b', b'j', index]).unwrap();
                objects.push(object);
            }
            // The second arm's members carry a later mtime, exactly like a
            // hydrated tree copied after the from-source compile.
            std::thread::sleep(Duration::from_millis(1100));
            for object in &objects {
                std::fs::write(object, std::fs::read(object).unwrap()).unwrap();
            }
            let out = dir.join("libcore.a");
            create(
                Path::new("ar"),
                &objects,
                &out,
                "ar",
                Duration::from_secs(60),
            )
            .await
            .unwrap();
            archives.push(std::fs::read(&out).unwrap());
        }
        assert_eq!(
            archives[0], archives[1],
            "identical members, different archive bytes"
        );
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
