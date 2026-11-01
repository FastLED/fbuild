//! Cache-key uniqueness tests for `PackageBase::with_override`.
//!
//! The contract that FastLED/fbuild#681 promises every framework
//! orchestrator is: when a consumer writes
//! `platform_packages = framework-x@<URL>#<sha>` in `platformio.ini`,
//! the resulting cache dir is **distinct** from the default pin's cache
//! dir AND distinct from any other override at a different commit on the
//! same upstream URL. Without that, bisection workflows like
//! FastLED/FastLED#3325 silently reuse the wrong vendored sources.
//!
//! These tests pin the contract at the `PackageBase` level so the
//! invariant holds for every framework package (16 of them at audit
//! time) without each package needing to re-prove it.
use super::*;
use fbuild_config::PackageOverride;

const DEFAULT_URL: &str = "https://github.com/example/repo/archive/default.tar.gz";

fn make_base(tmp: &Path, cache_root: &Path) -> PackageBase {
    PackageBase::with_cache_root(
        "framework-test",
        "0.1.0+gdefault",
        DEFAULT_URL,
        DEFAULT_URL,
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
        CacheSubdir::Platforms,
        tmp,
        cache_root,
    )
}

#[test]
fn override_changes_install_path() {
    // This is the unit test FastLED/fbuild#681 calls out by name:
    //   1. Default pin → some install_path P_default.
    //   2. Override URL A → install_path P_A, must differ from P_default.
    //   3. Override URL B (same repo, different commit) → install_path P_B,
    //      must differ from both P_default and P_A.
    //
    // If any two of these collide, a bisection step that swaps the URL
    // would silently reuse the previous commit's vendored sources — the
    // exact failure mode the override is meant to prevent.
    let tmp = tempfile::TempDir::new().unwrap();
    let cache_root = tmp.path().join("cache");

    let default_base = make_base(tmp.path(), &cache_root);
    let p_default = default_base.install_path();

    let ovr_a = PackageOverride {
        url: "https://github.com/example/repo/archive/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tar.gz"
            .to_string(),
        version: "0.0.0+gaaaaaaa".to_string(),
        checksum: None,
    };
    let p_a = make_base(tmp.path(), &cache_root)
        .with_override(ovr_a.clone())
        .install_path();

    let ovr_b = PackageOverride {
        url: "https://github.com/example/repo/archive/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.tar.gz"
            .to_string(),
        version: "0.0.0+gbbbbbbb".to_string(),
        checksum: None,
    };
    let p_b = make_base(tmp.path(), &cache_root)
        .with_override(ovr_b)
        .install_path();

    assert_ne!(
        p_default, p_a,
        "override URL must produce a different cache dir from the default pin"
    );
    assert_ne!(
        p_default, p_b,
        "second override URL must also differ from the default pin"
    );
    assert_ne!(
        p_a, p_b,
        "same upstream repo at different commits MUST hash to different cache dirs \
         — otherwise a bisection step silently reuses the previous commit's sources"
    );

    // Round-trip sanity: applying the same override twice yields the same path
    // (the hash is deterministic, not session-dependent).
    let p_a_again = make_base(tmp.path(), &cache_root)
        .with_override(ovr_a)
        .install_path();
    assert_eq!(p_a, p_a_again, "override hash must be deterministic");
}

#[test]
fn override_replaces_url_cache_key_version_and_checksum() {
    // The cache-key swap is the load-bearing field — assert it directly so a
    // future refactor can't accidentally preserve the default `cache_key`
    // while updating only `url`.
    let tmp = tempfile::TempDir::new().unwrap();
    let cache_root = tmp.path().join("cache");
    let base = make_base(tmp.path(), &cache_root);
    assert_eq!(base.cache_key, DEFAULT_URL);
    assert!(base.checksum.is_some());

    let ovr = PackageOverride {
        url: "https://example.com/override/archive/cafef00d.tar.gz".to_string(),
        version: "0.0.0+gcafef00".to_string(),
        checksum: None,
    };
    let overridden = base.with_override(ovr.clone());
    assert_eq!(overridden.url, ovr.url);
    assert_eq!(
        overridden.cache_key, ovr.url,
        "cache_key MUST be set from the override URL — install_path() uses cache_key, not url"
    );
    assert_eq!(overridden.version, ovr.version);
    assert_eq!(overridden.checksum, None);
    assert_eq!(
        overridden.name, "framework-test",
        "name is preserved across override"
    );
}

#[test]
fn registry_override_checksum_changes_install_identity_at_same_url_and_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cache_root = tmp.path().join("cache");
    let url = "https://dl.registry.platformio.org/download/acme/tool/gcc/1.0/gcc.tar.gz";
    let first = make_base(tmp.path(), &cache_root).with_override(PackageOverride {
        url: url.into(),
        version: "1.0".into(),
        checksum: Some("a".repeat(64)),
    });
    let changed = make_base(tmp.path(), &cache_root).with_override(PackageOverride {
        url: url.into(),
        version: "1.0".into(),
        checksum: Some("b".repeat(64)),
    });
    assert_ne!(first.install_path(), changed.install_path());
    assert_ne!(first.cache_key, changed.cache_key);
}
