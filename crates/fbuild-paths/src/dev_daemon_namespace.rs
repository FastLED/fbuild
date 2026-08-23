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
//! stamp landed. `crates/fbuild-build-engine/tests/
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
    let exe = std::env::current_exe()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores a single env var on drop; tests here mutate process state
    /// that outlives a single test.
    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            unsafe { std::env::set_var(name, value) };
            Self { name, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => unsafe { std::env::set_var(self.name, value) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

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

    /// The dev-mode branches of `namespace_to_export` are covered by the
    /// pure-function tests above; an env-level test for them would race the
    /// parallel tests in this crate that also flip `FBUILD_DEV_MODE`
    /// process-globally. Inheritance is the one composition worth proving
    /// end-to-end, and it is dev-flag-independent.
    #[test]
    fn export_honors_an_inherited_stamp() {
        let _ns = EnvVarGuard::set(ZCCACHE_DAEMON_NAMESPACE_ENV, "checkout-b-fedcba9876543210");

        assert_eq!(
            namespace_to_export().unwrap(),
            Some("checkout-b-fedcba9876543210".to_string())
        );
    }
}
