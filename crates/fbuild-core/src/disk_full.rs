//! Recognising a full volume, and saying so the way the pre-flight check does
//! (FastLED/fbuild#1750 item 4).
//!
//! `fbuild_build_engine::disk_preflight` catches a volume that is *already* too
//! full before a build starts. It cannot catch a volume that fills up *during*
//! a build — a 40 MB LTO temporary, a `.lnk` blob, a framework install — and
//! that used to reach the user as a raw `ENOSPC` from whichever subprocess
//! wrote last, naming neither the path nor a way out.
//!
//! Everything here recognises that condition at a tool boundary
//! ([`enospc_signature`] for a subprocess's stderr, [`is_enospc_io`] for
//! fbuild's own writes) and describes it in the same shape and with the same
//! closing hint ([`DISK_FULL_HINT`]) as the pre-flight failure, so a user sees
//! one message family, not two unrelated errors.

use std::io;
use std::path::Path;

use crate::FbuildError;

/// The line every disk-space failure ends with. Shared with
/// `fbuild_build_engine::disk_preflight::PreflightReport::failure_message` so
/// the pre-flight and the translated `ENOSPC` cannot drift apart.
pub const DISK_FULL_HINT: &str =
    "free space on that volume, or set FBUILD_MIN_DISK_BYTES=0 to skip the pre-flight check";

/// How each platform reports a full volume in a subprocess's stderr.
///
/// `ENOSPC` is the POSIX errno name: linkers and `esptool` print it verbatim,
/// while libc-backed tools print the `strerror` text. Win32 tools print
/// `ERROR_DISK_FULL`'s message. A quota (`EDQUOT`) presents identically to the
/// user — the write failed and no amount of retrying helps — so it is
/// translated the same way.
pub const ENOSPC_SIGNATURES: [&str; 4] = [
    "No space left on device",
    "ENOSPC",
    "There is not enough space on the disk",
    "Disk quota exceeded",
];

/// The signature `text` contains, if any. Case-insensitive: `esptool` capitalises
/// differently from the C `strerror` table.
pub fn enospc_signature(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return None;
    }
    let lowered = text.to_ascii_lowercase();
    ENOSPC_SIGNATURES
        .iter()
        .copied()
        .find(|signature| lowered.contains(&signature.to_ascii_lowercase()))
}

/// `ENOSPC` — the errno every POSIX host returns for a full volume.
const POSIX_ENOSPC_ERRNO: i32 = 28;

/// Win32 `ERROR_HANDLE_DISK_FULL` and `ERROR_DISK_FULL`.
const WINDOWS_DISK_FULL_ERRNOS: [i32; 2] = [39, 112];

/// Whether `error` is the OS saying the volume is full.
///
/// Checks the errno first and falls back to the formatted message, because an
/// `io::Error` built by a library with a lossy `raw_os_error()` still carries
/// the text. The host is asked through `platform::host` — the one place this
/// workspace selects an operating system.
pub fn is_enospc_io(error: &io::Error) -> bool {
    if let Some(code) = error.raw_os_error() {
        let host = crate::platform::host::current();
        if host.is_unix() && code == POSIX_ENOSPC_ERRNO {
            return true;
        }
        if host.is_windows() && WINDOWS_DISK_FULL_ERRNOS.contains(&code) {
            return true;
        }
    }
    enospc_signature(&error.to_string()).is_some()
}

/// One actionable error for a tool whose write failed because the volume is
/// full. `context` names what was being written (`"ESP32 link"`,
/// `"esptool elf2image"`, `"ar (libfoo.a)"`).
pub fn disk_full_error(context: &str, signature: &str) -> FbuildError {
    FbuildError::BuildFailed(format!(
        "insufficient disk space:\n  \
         {context} failed: the volume is full ({signature})\n  \
         the pre-flight check passed, so the volume filled up during the build\n  \
         {DISK_FULL_HINT}{}",
        // #1750 item 6: a full volume mid-build would otherwise also starve the
        // failure path itself. Releasing the reserve is what makes this message
        // writable to the log, so it belongs in the message.
        crate::disk_reserve::release_note(context)
    ))
}

/// The same error for fbuild's own write of `path`, or `None` when `error` is
/// any other failure — callers keep their existing handling for those.
pub fn disk_full_io_error(context: &str, path: &Path, error: &io::Error) -> Option<FbuildError> {
    if !is_enospc_io(error) {
        return None;
    }
    Some(FbuildError::BuildFailed(format!(
        "insufficient disk space:\n  \
         {context} could not write {}: the volume is full ({error})\n  \
         the pre-flight check passed, so the volume filled up during the build\n  \
         {DISK_FULL_HINT}{}",
        path.display(),
        crate::disk_reserve::release_note(context)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_and_windows_signatures_are_recognised() {
        assert_eq!(
            enospc_signature("ld: final link failed: No space left on device"),
            Some("No space left on device")
        );
        assert_eq!(
            enospc_signature("A fatal error occurred: Could not write: errno 28 ENOSPC"),
            Some("ENOSPC")
        );
        assert_eq!(
            enospc_signature(
                "error writing 'firmware.bin': There is not enough space on the disk."
            ),
            Some("There is not enough space on the disk")
        );
        assert_eq!(
            enospc_signature("cp: Disk quota exceeded"),
            Some("Disk quota exceeded")
        );
    }

    #[test]
    fn ordinary_tool_failures_are_not_mistaken_for_a_full_disk() {
        assert_eq!(enospc_signature(""), None);
        assert_eq!(enospc_signature("undefined reference to `setup()'"), None);
        assert_eq!(enospc_signature("cc1plus: error: out of memory"), None);
    }

    #[test]
    fn the_message_names_the_context_the_cause_and_the_way_out() {
        let message = disk_full_error("ESP32 link", "No space left on device").to_string();
        assert!(message.contains("insufficient disk space"), "{message}");
        assert!(message.contains("ESP32 link failed"), "{message}");
        assert!(
            message.contains("No space left on device"),
            "the underlying cause must survive: {message}"
        );
        assert!(
            message.contains("FBUILD_MIN_DISK_BYTES=0"),
            "every disk failure ends with the same hint: {message}"
        );
        assert!(message.contains(DISK_FULL_HINT), "{message}");
    }

    #[test]
    fn an_enospc_errno_is_recognised() {
        let code = if crate::platform::host::current().is_windows() {
            WINDOWS_DISK_FULL_ERRNOS[1]
        } else {
            POSIX_ENOSPC_ERRNO
        };
        let error = io::Error::from_raw_os_error(code);
        assert!(is_enospc_io(&error));
        let translated = disk_full_io_error(
            "writing the firmware image",
            Path::new("/proj/build/firmware.bin"),
            &error,
        )
        .expect("ENOSPC must translate");
        let message = translated.to_string();
        assert!(
            message.contains("/proj/build/firmware.bin"),
            "the path being written must be named: {message}"
        );
        assert!(message.contains("writing the firmware image"), "{message}");
    }

    #[test]
    fn other_io_errors_keep_their_own_handling() {
        let error = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(!is_enospc_io(&error));
        assert!(
            disk_full_io_error("writing", Path::new("/proj/x"), &error).is_none(),
            "only a full volume may be translated"
        );
    }

    #[test]
    fn a_library_error_without_an_errno_still_translates() {
        let error = io::Error::other("write failed: No space left on device");
        assert!(is_enospc_io(&error));
    }
}
