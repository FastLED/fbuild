//! Development daemon-identity namespace stamping (FastLED/fbuild#1285).
//!
//! In dev (`FBUILD_DEV_MODE=1`) co-located checkouts of fbuild share one
//! home root (`~/.fbuild/dev`). The zccache compile daemons those checkouts
//! spawn identify themselves by the *zccache binary's* content hash —
//! identical across checkouts — so two dev checkouts rendezvous on the same
//! compile daemon and each displaces the other as "stale" on every
//! invocation (the `displace-stale` war, root-caused in zackees/soldr#2352).
//!
//! The fix is a per-checkout namespace stamp exported as
//! `ZCCACHE_DAEMON_NAMESPACE`, which the pinned zccache folds into the IPC
//! endpoint its daemons rendezvous on — so two stamps mean two pipes, and
//! neither checkout can see the other as stale.
//!
//! This module previously claimed the export was "inert until fbuild repins
//! a zccache release containing it". That was wrong: endpoint namespacing is
//! already present at the pinned rev, and the isolation has worked since the
//! stamp landed. `crates/fbuild-build/tests/env_isolated/
//! dev_daemon_namespace_isolation.rs` pins the contract so a future repin
//! cannot drop it silently. What remains zccache-side (zccache#1362) is
//! zccache *deriving its own* stamp when nothing exported one — which fbuild
//! does not need, because fbuild exports one:
//!
//! ```text
//! stamp = "<workspace version>-<first 16 hex digits of blake3(current_exe)>"
//! ```
//!
//! The stamp is content-based (a rebuilt checkout gets a fresh identity)
//! and computed **once per process tree**: each binary entry point (`fbuild`,
//! `fbuild-daemon`) derives it through this module and exports the *value*;
//! every child — including the spawned daemon — inherits it instead of
//! re-hashing. Propagating the value (not a path) also keeps the identity
//! stable across the Windows self-update lock-rename dance.
//!
//! Official (non-dev) invocations export nothing: release builds keep the
//! bare namespace and single-daemon-on-upgrade semantics — only dev pays.
//! A valid inherited stamp wins even outside dev mode, so a release CLI
//! under a dev parent stays in its family's namespace.
//!
//! The same dev-only image digest also keys the no-op build identity
//! ([`dev_image_digest`], FastLED/fbuild#2125): there it is always this
//! process's own image, never an inherited value, because the daemon that
//! builds can be relinked without the CLI that spawned it.
//!
//! Hash failure is reported, never silently swallowed — a silent downgrade
//! would quietly reintroduce the shared-daemon war this module exists to
//! prevent. Entry points log the warning and continue (today's behavior),
//! matching the repo-wide rule of never gating progress on a broken
//! filesystem.

use crate::is_dev_mode;

/// The variable zccache reads to namespace its daemons (zccache#1362).
pub const ZCCACHE_DAEMON_NAMESPACE_ENV: &str = "ZCCACHE_DAEMON_NAMESPACE";

/// Number of hex digits taken from the blake3 digest.
const HASH_PREFIX_HEX: usize = 16;

/// Derive the namespace this process should export.
///
/// * A valid (non-blank) inherited stamp wins without hashing — one hash
///   per process tree, and a CLI-spawned daemon stays in its spawner's
///   namespace.
/// * Otherwise, only dev mode stamps, keyed on the current executable's
///   content.
/// * Otherwise (official builds) no stamp is exported.
pub(crate) fn namespace_for_process<F>(
    inherited: Option<&str>,
    dev_mode: bool,
    hash_current_exe: F,
) -> std::io::Result<Option<String>>
where
    F: FnOnce() -> std::io::Result<[u8; 32]>,
{
    if let Some(stamp) = inherited.map(str::trim).filter(|stamp| !stamp.is_empty()) {
        return Ok(Some(stamp.to_string()));
    }
    if !dev_mode {
        return Ok(None);
    }
    let digest = blake3::Hash::from_bytes(hash_current_exe()?);
    let hex = digest.to_hex();
    Ok(Some(format!(
        "{}-{}",
        env!("CARGO_PKG_VERSION"),
        &hex.as_str()[..HASH_PREFIX_HEX]
    )))
}

/// Hash the running executable's bytes with blake3.
///
/// Content-based by design: the loaded image would be a per-run nonce
/// (ASLR/IAT/`.data` mutation), while the on-disk bytes identify the
/// checkout build.
fn hash_current_exe() -> std::io::Result<[u8; 32]> {
    let exe = fbuild_core::platform::executable::current_image()?;
    let mut file = std::fs::File::open(&exe)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}

/// The namespace this process should export into its own environment
/// before spawning children, or `None` for official (non-dev) builds.
///
/// Binary entry points (`main.rs` only — see the
/// `ban_env_var_set_after_import` dylint) call this and perform the
/// `set_var` themselves.
pub fn namespace_to_export() -> std::io::Result<Option<String>> {
    let inherited = std::env::var(ZCCACHE_DAEMON_NAMESPACE_ENV).ok();
    namespace_for_process(inherited.as_deref(), is_dev_mode(), hash_current_exe)
}

/// Digest of the running fbuild image, for the no-op build identity, or
/// `None` outside dev mode (FastLED/fbuild#2125).
///
/// A dev checkout's commit stamp cannot see an uncommitted `.rs` edit, but
/// any such edit relinks the binary, so the image's bytes change. Official
/// runs return `None`: an installed release binary changes only with its
/// version, which the identity already carries, and hashing its path or
/// mtime would make CI-restored build dirs miss (docs/CI_CACHE.md).
///
/// Computed once per process. The daemon calls it at startup, so on hosts
/// without a replace-proof image handle the digest describes the binary that
/// is running, not one rebuilt over it later.
pub fn dev_image_digest() -> Option<&'static str> {
    static DIGEST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| image_digest_for(is_dev_mode(), hash_running_image))
        .as_deref()
}

/// [`dev_image_digest`]'s seam. A hash failure yields a unique identity,
/// never a stable fallback: that would no-op on stale artifacts.
pub(crate) fn image_digest_for<F>(dev_mode: bool, hash: F) -> Option<String>
where
    F: FnOnce() -> std::io::Result<[u8; 32]>,
{
    if !dev_mode {
        return None;
    }
    Some(match hash() {
        Ok(digest) => blake3::Hash::from_bytes(digest).to_hex().to_string(),
        Err(error) => {
            static UNHASHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            tracing::warn!(
                "cannot hash the running fbuild image; builds will not reuse artifacts from another fbuild process: {error}"
            );
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let call = UNHASHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("unhashed-{}-{nanos}-{call}", std::process::id())
        }
    })
}

/// Hash the running image: Linux's replace-proof `/proc/self/exe` when the
/// host has one, so a binary rebuilt over the launch path is not mistaken
/// for the running one; otherwise the launch path.
fn hash_running_image() -> std::io::Result<[u8; 32]> {
    let image = match fbuild_core::platform::executable::replace_proof_live_image() {
        Some(image) => image,
        None => fbuild_core::platform::executable::current_image()?,
    };
    let mut file = std::fs::File::open(&image)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn inherited_stamp_wins_without_hashing() {
        let hashed = std::cell::Cell::new(false);
        let namespace = namespace_for_process(Some("checkout-a-0123456789abcdef"), true, || {
            hashed.set(true);
            Ok(hash(0xaa))
        })
        .unwrap();

        assert_eq!(namespace, Some("checkout-a-0123456789abcdef".to_string()));
        assert!(
            !hashed.get(),
            "an inherited stamp must avoid re-hashing per process"
        );
    }

    #[test]
    fn inherited_stamp_wins_even_outside_dev_mode() {
        let namespace = namespace_for_process(Some("checkout-a-0123456789abcdef"), false, || {
            Err(std::io::Error::other("must not be called"))
        })
        .unwrap();

        assert_eq!(namespace, Some("checkout-a-0123456789abcdef".to_string()));
    }

    #[test]
    fn official_build_exports_nothing() {
        let hashed = std::cell::Cell::new(false);
        let namespace = namespace_for_process(None, false, || {
            hashed.set(true);
            Ok(hash(0xaa))
        })
        .unwrap();

        assert_eq!(namespace, None);
        assert!(
            !hashed.get(),
            "official releases retain upgrade semantics without paying the hash"
        );
    }

    #[test]
    fn dev_build_stamps_version_and_first_sixteen_hex_digits() {
        let namespace = namespace_for_process(None, true, || Ok(hash(0xab))).unwrap();

        assert_eq!(
            namespace.as_deref(),
            Some(concat!(env!("CARGO_PKG_VERSION"), "-abababababababab"))
        );
    }

    #[test]
    fn blank_inherited_stamp_is_not_an_identity() {
        let namespace = namespace_for_process(Some("   "), true, || Ok(hash(0x12))).unwrap();

        assert_eq!(
            namespace.as_deref(),
            Some(concat!(env!("CARGO_PKG_VERSION"), "-1212121212121212"))
        );
    }

    #[test]
    fn hash_failure_is_not_silently_downgraded() {
        let error = namespace_for_process(None, true, || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "locked",
            ))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// Any `.rs` edit relinks the binary, so in dev mode the no-op build
    /// identity carries the running image's digest (FastLED/fbuild#2125).
    #[test]
    fn dev_image_digest_is_the_full_image_hash() {
        let digest = image_digest_for(true, || Ok(hash(0xab))).unwrap();
        assert_eq!(digest, "ab".repeat(32));
        assert_ne!(
            image_digest_for(true, || Ok(hash(0xcd))),
            Some(digest),
            "a rebuilt image must change the identity"
        );
    }

    #[test]
    fn official_runs_carry_no_image_digest_and_never_hash() {
        let digest = image_digest_for(false, || Err(std::io::Error::other("must not be called")));
        assert_eq!(digest, None, "release identities stay reproducible");
    }

    /// An unreadable image must never fall back to a stable value, which
    /// would no-op on stale artifacts: it gets a per-call identity instead.
    #[test]
    fn unhashable_image_never_matches_a_previous_identity() {
        let fail = || Err(std::io::Error::other("unreadable"));
        let first = image_digest_for(true, fail).unwrap();
        let second = image_digest_for(true, fail).unwrap();
        assert!(first.starts_with("unhashed-"), "{first}");
        assert_ne!(first, second);
    }

    /// Keep inheritance coverage on the pure seam: mutating the process
    /// environment here can race every parallel test that reads `HOME` or any
    /// other variable (FastLED/fbuild#1483).
    #[test]
    fn export_honors_an_inherited_stamp() {
        assert_eq!(
            namespace_for_process(Some("checkout-b-fedcba9876543210"), false, || {
                Err(std::io::Error::other("must not be called"))
            })
            .unwrap(),
            Some("checkout-b-fedcba9876543210".to_string())
        );
    }
}
