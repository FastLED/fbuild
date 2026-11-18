//! Link-inputs digest stored next to `firmware.elf` (FastLED/fbuild#1736).
//!
//! `Linker::link_all` reuses an ELF newer than its objects. That mtime check
//! can't see a changed link flag, a removed object, a new fbuild release or a
//! platform's linker-script recipe, so the reuse also requires this digest to
//! match the one written by the last successful link.

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::linker::LinkExtraArgs;

/// Sidecar holding the digest of the inputs `firmware.elf` was linked from.
pub(crate) fn digest_path(elf: &Path) -> NormalizedPath {
    let mut name = elf.as_os_str().to_owned();
    name.push(".link");
    NormalizedPath::new(name)
}

/// Digest of everything besides object contents that shapes the link.
///
/// `command` is [`crate::linker::Linker::link_command`]: the argv the linker
/// runs, so a built-in flag from an MCU config JSON, a board file or
/// orchestrator code relinks without being listed here (FastLED/fbuild#2042).
/// The objects are hashed as well because a platform may fold them into an
/// archive whose path is all the argv shows (AVR's `libframework.a`).
pub(crate) fn link_digest(
    objects: impl IntoIterator<Item = impl AsRef<Path>>,
    command: &[String],
    extra: &LinkExtraArgs,
    platform_fingerprint: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(env!("CARGO_PKG_VERSION").as_bytes());
    field(platform_fingerprint.as_bytes());
    field(&[u8::from(extra.bloat_analysis)]);
    field(b"--command");
    for arg in command {
        field(arg.as_bytes());
    }
    field(b"--objects");
    for object in objects {
        field(object.as_ref().to_string_lossy().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Whether the sidecar next to `elf` records `digest`.
pub(crate) fn digest_matches(elf: &Path, digest: &str) -> bool {
    std::fs::read_to_string(digest_path(elf)).is_ok_and(|stored| stored.trim() == digest)
}

/// Record `digest` for a freshly linked `elf`. Best effort: a missing sidecar
/// only costs one extra relink next time.
pub(crate) fn record_digest(elf: &Path, digest: &str) {
    if let Err(e) = std::fs::write(digest_path(elf), digest) {
        tracing::debug!("could not write link digest for {}: {e}", elf.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(objects: &[&str], command: &[&str], fingerprint: &str) -> String {
        let command: Vec<String> = command.iter().map(|a| a.to_string()).collect();
        link_digest(objects, &command, &LinkExtraArgs::default(), fingerprint)
    }

    #[test]
    fn digest_changes_with_every_link_input() {
        let base = digest(&["a.o", "b.o"], &["gcc", "-Wl,--gc-sections"], "");
        assert_eq!(
            base,
            digest(&["a.o", "b.o"], &["gcc", "-Wl,--gc-sections"], "")
        );
        assert_ne!(base, digest(&["a.o"], &["gcc", "-Wl,--gc-sections"], ""));
        assert_ne!(base, digest(&["a.o", "b.o"], &["gcc"], ""));
        assert_ne!(
            base,
            digest(&["a.o", "b.o"], &["gcc", "-Wl,--gc-sections"], "gen2")
        );
        assert_ne!(digest(&["ab.o"], &[], ""), digest(&["a", "b.o"], &[], ""));
    }

    /// A flag the platform adds itself reaches the digest through the argv,
    /// with nothing in `LinkExtraArgs` (FastLED/fbuild#2042).
    #[test]
    fn digest_changes_with_a_builtin_flag_and_argv_order() {
        let base = digest(&["a.o"], &["gcc", "-mmcu=atmega328p", "a.o"], "");
        assert_ne!(
            base,
            digest(&["a.o"], &["gcc", "-mmcu=atmega2560", "a.o"], "")
        );
        assert_ne!(
            base,
            digest(&["a.o"], &["gcc", "a.o", "-mmcu=atmega328p"], "")
        );
        let extra = LinkExtraArgs {
            bloat_analysis: true,
            ..Default::default()
        };
        let command = vec!["gcc".to_string()];
        assert_ne!(
            link_digest(["a.o"], &command, &LinkExtraArgs::default(), ""),
            link_digest(["a.o"], &command, &extra, "")
        );
    }

    #[test]
    fn sidecar_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let elf = dir.path().join("firmware.elf");
        assert!(!digest_matches(&elf, "x"));
        record_digest(&elf, "x");
        assert!(digest_matches(&elf, "x"));
        assert!(!digest_matches(&elf, "y"));
    }
}
