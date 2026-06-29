//! avr8js npm cache integrity tests (issue #86).
//!
//! These tests probe `avr8js_cache_is_intact`, `prepare_avr8js_cache_for_install`,
//! `refresh_emu_cache_requested`, and `ensure_avr8js_npm_in` — covering the
//! detection of partial/corrupt npm installs, the force-refresh env var, and
//! actionable errors when `npm` is unreachable.

use super::avr8js_npm::{
    avr8js_cache_is_intact, ensure_avr8js_npm_in, prepare_avr8js_cache_for_install,
    refresh_emu_cache_requested, Avr8jsCachePrep, REFRESH_EMU_CACHE_ENV,
};

/// Serialises tests that mutate process-wide env vars (PATH). Without
/// this, parallel cargo-test workers would clobber each other's PATH.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[test]
fn avr8js_cache_is_intact_detects_missing_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Empty cache dir: no node_modules at all.
    assert!(!avr8js_cache_is_intact(tmp.path()));
}

#[test]
fn avr8js_cache_is_intact_detects_missing_package_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Simulate a partial/corrupt install: dir exists, but no package.json.
    std::fs::create_dir_all(tmp.path().join("node_modules").join("avr8js")).unwrap();
    assert!(
        !avr8js_cache_is_intact(tmp.path()),
        "corrupt install (no package.json) must not count as intact"
    );
}

#[test]
fn avr8js_cache_is_intact_accepts_marker_present() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module_dir = tmp.path().join("node_modules").join("avr8js");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("package.json"), b"{\"name\":\"avr8js\"}").unwrap();
    assert!(avr8js_cache_is_intact(tmp.path()));
}

#[test]
fn prepare_avr8js_cache_skips_when_intact() {
    let tmp = tempfile::TempDir::new().unwrap();
    let module_dir = tmp.path().join("node_modules").join("avr8js");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("package.json"), b"{}").unwrap();
    assert_eq!(
        prepare_avr8js_cache_for_install(tmp.path(), false),
        Avr8jsCachePrep::AlreadyIntact
    );
    // Tree must survive untouched.
    assert!(module_dir.join("package.json").exists());
}

#[test]
fn prepare_avr8js_cache_wipes_node_modules_when_corrupt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let node_modules = tmp.path().join("node_modules");
    let module_dir = node_modules.join("avr8js");
    std::fs::create_dir_all(&module_dir).unwrap();
    // Stray partial file inside an otherwise package.json-less install.
    std::fs::write(module_dir.join("index.mjs"), b"partial").unwrap();
    assert!(!avr8js_cache_is_intact(tmp.path()));

    let prep = prepare_avr8js_cache_for_install(tmp.path(), false);
    assert_eq!(prep, Avr8jsCachePrep::WipedNodeModules);
    assert!(
        !node_modules.exists(),
        "corrupt node_modules tree must be wiped before reinstall"
    );
}

#[test]
fn prepare_avr8js_cache_force_refresh_wipes_entire_dir() {
    let tmp_parent = tempfile::TempDir::new().unwrap();
    let cache = tmp_parent.path().join("avr8js-node");
    let module_dir = cache.join("node_modules").join("avr8js");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("package.json"), b"{}").unwrap();
    // Even a fully-intact install must be wiped under force_refresh=true.
    let prep = prepare_avr8js_cache_for_install(&cache, true);
    assert_eq!(prep, Avr8jsCachePrep::ForceWiped);
    assert!(!cache.exists(), "force-refresh must remove the cache dir");
}

#[test]
fn prepare_avr8js_cache_nothing_to_clean_on_fresh_path() {
    let tmp_parent = tempfile::TempDir::new().unwrap();
    let cache = tmp_parent.path().join("avr8js-node"); // doesn't exist yet
    assert_eq!(
        prepare_avr8js_cache_for_install(&cache, false),
        Avr8jsCachePrep::NothingToClean
    );
}

#[test]
fn refresh_emu_cache_requested_recognises_truthy_values() {
    let _guard = env_lock();
    for (val, expected) in [
        ("1", true),
        ("true", true),
        ("TRUE", true),
        ("yes", true),
        ("YES", true),
        ("0", false),
        ("", false),
        ("no", false),
    ] {
        std::env::set_var(REFRESH_EMU_CACHE_ENV, val);
        assert_eq!(
            refresh_emu_cache_requested(),
            expected,
            "{}={:?} should parse as {}",
            REFRESH_EMU_CACHE_ENV,
            val,
            expected
        );
    }
    std::env::remove_var(REFRESH_EMU_CACHE_ENV);
    assert!(!refresh_emu_cache_requested());
}

/// When npm isn't on PATH, `ensure_avr8js_npm_in` must return an
/// `FbuildError::DeployFailed` that names both `npm` and the cache dir.
/// This is the fix for issue #86's silent `ERR_MODULE_NOT_FOUND`.
#[tokio::test]
async fn ensure_avr8js_npm_in_reports_clear_error_without_npm() {
    let _guard = env_lock();
    let saved_path = std::env::var_os("PATH");
    // PATHEXT matters on Windows for command resolution of .cmd files.
    let saved_pathext = std::env::var_os("PATHEXT");

    std::env::set_var("PATH", "");
    #[cfg(windows)]
    {
        // Ensure .cmd isn't resolved via some fallback.
        std::env::set_var("PATHEXT", "");
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let cache = tmp.path().join("avr8js-node");
    let result = ensure_avr8js_npm_in(&cache, false).await;

    // Restore BEFORE asserting so a panic doesn't leak PATH="" to sibling tests.
    if let Some(p) = saved_path {
        std::env::set_var("PATH", p);
    } else {
        std::env::remove_var("PATH");
    }
    if let Some(p) = saved_pathext {
        std::env::set_var("PATHEXT", p);
    } else {
        std::env::remove_var("PATHEXT");
    }

    let err = result.expect_err("npm must be unresolvable with PATH=\"\"");
    let msg = err.to_string();
    assert!(
        msg.contains("npm"),
        "error message must mention 'npm'; got: {}",
        msg
    );
    assert!(
        msg.contains(&cache.display().to_string()),
        "error message must include cache dir path; got: {}",
        msg
    );
    assert!(
        msg.contains(REFRESH_EMU_CACHE_ENV),
        "error message must reference {} for recovery; got: {}",
        REFRESH_EMU_CACHE_ENV,
        msg
    );
}

/// When the cache dir contains a corrupt partial install, the reinstall
/// path must fire (detected here by asserting the partial tree is wiped
/// even when the downstream npm call subsequently fails).
#[tokio::test]
async fn ensure_avr8js_npm_in_wipes_corrupt_before_reinstall() {
    let _guard = env_lock();
    let saved_path = std::env::var_os("PATH");

    // Force the npm spawn to fail so we isolate the wipe behaviour.
    std::env::set_var("PATH", "");

    let tmp = tempfile::TempDir::new().unwrap();
    let cache = tmp.path().join("avr8js-node");
    let node_modules = cache.join("node_modules");
    let module_dir = node_modules.join("avr8js");
    std::fs::create_dir_all(&module_dir).unwrap();
    // Deliberately omit package.json → corrupt.
    std::fs::write(module_dir.join("garbage"), b"partial").unwrap();
    assert!(!avr8js_cache_is_intact(&cache));

    let result = ensure_avr8js_npm_in(&cache, false).await;

    if let Some(p) = saved_path {
        std::env::set_var("PATH", p);
    } else {
        std::env::remove_var("PATH");
    }

    // npm spawn must fail (no PATH), but the corrupt tree must be gone.
    assert!(result.is_err(), "empty PATH should prevent npm install");
    assert!(
        !node_modules.exists(),
        "corrupt node_modules must be wiped before reinstall attempt"
    );
}
