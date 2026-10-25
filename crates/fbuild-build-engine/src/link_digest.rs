//! Link-inputs digest stored next to `firmware.elf` (FastLED/fbuild#1736).
//!
//! `Linker::link_all` reuses an ELF newer than its objects. That mtime check
//! can't see a changed link flag, a removed object, a new fbuild release or a
//! platform's linker-script recipe, so the reuse also requires this digest to
//! match the one written by the last successful link.

use std::path::{Path, PathBuf};

use crate::linker::LinkExtraArgs;

/// Sidecar holding the digest of the inputs `firmware.elf` was linked from.
pub(crate) fn digest_path(elf: &Path) -> PathBuf {
    let mut name = elf.as_os_str().to_owned();
    name.push(".link");
    PathBuf::from(name)
}

/// Digest of everything besides object contents that shapes the link.
pub(crate) fn link_digest(
    objects: &[&PathBuf],
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
    for flag in &extra.flags {
        field(flag.as_bytes());
    }
    field(b"--libs");
    for lib in &extra.libs {
        field(lib.as_bytes());
    }
    field(b"--objects");
    for object in objects {
        field(object.to_string_lossy().as_bytes());
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

    fn digest(objects: &[&str], flags: &[&str], fingerprint: &str) -> String {
        let objects: Vec<PathBuf> = objects.iter().map(PathBuf::from).collect();
        let refs: Vec<&PathBuf> = objects.iter().collect();
        let extra = LinkExtraArgs {
            flags: flags.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        link_digest(&refs, &extra, fingerprint)
    }

    #[test]
    fn digest_changes_with_every_link_input() {
        let base = digest(&["a.o", "b.o"], &["-Wl,--gc-sections"], "");
        assert_eq!(base, digest(&["a.o", "b.o"], &["-Wl,--gc-sections"], ""));
        assert_ne!(base, digest(&["a.o"], &["-Wl,--gc-sections"], ""));
        assert_ne!(base, digest(&["a.o", "b.o"], &[], ""));
        assert_ne!(
            base,
            digest(&["a.o", "b.o"], &["-Wl,--gc-sections"], "gen2")
        );
        assert_ne!(digest(&["ab.o"], &[], ""), digest(&["a", "b.o"], &[], ""));
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
