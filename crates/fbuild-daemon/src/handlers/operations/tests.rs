//! Unit tests for the operations handlers split.
//!
//! All tests previously inlined at the bottom of `operations.rs` now
//! live here. They reach into sibling submodules via `super::*`.

mod deploy_message_tests {
    //! Verifies the `/api/deploy` response message exposes the real
    //! deploy outcome (full / verify-skip / selective) instead of the
    //! generic `"deploy succeeded"`. See GitHub issue #76.
    //!
    //! These tests cover only the pure string-formatting contract; the
    //! underlying outcome computation is tested in `fbuild-deploy`.
    use fbuild_deploy::{esp32::FlashRegion, DeployOutcome};

    fn prefix_for(outcome: &DeployOutcome) -> String {
        format!("deploy succeeded ({})", outcome.describe())
    }

    #[test]
    fn full_flash_prefix() {
        assert_eq!(
            prefix_for(&DeployOutcome::FullFlash),
            "deploy succeeded (full flash)"
        );
    }

    #[test]
    fn verify_skip_prefix() {
        assert_eq!(
            prefix_for(&DeployOutcome::VerifySkip),
            "deploy succeeded (verify skipped, device already matched)"
        );
    }

    #[test]
    fn selective_flash_firmware_prefix() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        assert_eq!(
            prefix_for(&outcome),
            "deploy succeeded (selective flash: firmware)"
        );
    }

    #[test]
    fn monitor_suffix_preserved_on_selective_flash() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        let prefix = prefix_for(&outcome);
        let combined = format!("{}; monitor: ok", prefix);
        assert_eq!(
            combined,
            "deploy succeeded (selective flash: firmware); monitor: ok"
        );
    }

    #[test]
    fn monitor_error_suffix_preserved_on_verify_skip() {
        let prefix = prefix_for(&DeployOutcome::VerifySkip);
        let combined = format!("{}; monitor error: pattern matched", prefix);
        assert_eq!(
            combined,
            "deploy succeeded (verify skipped, device already matched); monitor error: pattern matched"
        );
    }
}

#[cfg(feature = "espflash-native")]
mod espflash_env_tests {
    use super::super::common::{native_verify_enabled, native_write_enabled};

    // This lock only serializes these unit tests while they mutate the
    // process environment. Production callers and any other tests reading
    // the same env vars remain unsynchronized.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn native_verify_defaults_on_and_allows_opt_out() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FBUILD_USE_ESPFLASH_VERIFY");
        assert!(native_verify_enabled());

        std::env::set_var("FBUILD_USE_ESPFLASH_VERIFY", "0");
        assert!(!native_verify_enabled());

        std::env::set_var("FBUILD_USE_ESPFLASH_VERIFY", "false");
        assert!(!native_verify_enabled());

        std::env::set_var("FBUILD_USE_ESPFLASH_VERIFY", "1");
        assert!(native_verify_enabled());
        std::env::remove_var("FBUILD_USE_ESPFLASH_VERIFY");
    }

    #[test]
    fn native_write_defaults_on_and_allows_opt_out() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FBUILD_USE_ESPFLASH_WRITE");
        assert!(native_write_enabled());

        std::env::set_var("FBUILD_USE_ESPFLASH_WRITE", "off");
        assert!(!native_write_enabled());

        std::env::set_var("FBUILD_USE_ESPFLASH_WRITE", "yes");
        assert!(native_write_enabled());
        std::env::remove_var("FBUILD_USE_ESPFLASH_WRITE");
    }
}

mod image_hash_memo_tests {
    //! Memo-cache correctness for [`compute_esp32_image_hash`]: the
    //! memo must *reuse* the stored hash when none of the three
    //! region files have changed, and *re-hash* when any of them
    //! changes on disk.
    use super::super::common::compute_esp32_image_hash;
    use crate::context::DaemonContext;
    use std::io::Write;
    use std::path::Path;

    fn write(path: &Path, bytes: &[u8]) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn fresh_ctx() -> std::sync::Arc<DaemonContext> {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        std::sync::Arc::new(DaemonContext::new(8765, tx, "unknown".to_string()))
    }

    fn seed_image(dir: &Path) {
        write(&dir.join("bootloader.bin"), b"BOOT0");
        write(&dir.join("partitions.bin"), b"PART00");
        write(&dir.join("firmware.bin"), b"FW__FIRST_BUILD");
    }

    /// Second call with unchanged files must hit the memo (same
    /// result, no work). We verify the memo side by directly
    /// inspecting `ctx.image_hash_memo`.
    #[test]
    fn memo_hit_reuses_hash() {
        let tmp = tempfile::tempdir().unwrap();
        seed_image(tmp.path());
        let ctx = fresh_ctx();
        let fw = tmp.path().join("firmware.bin");

        let h1 = compute_esp32_image_hash(&ctx, &fw, 0x0, 0x8000, 0x10000).unwrap();
        assert_eq!(ctx.image_hash_memo.len(), 1);
        let h2 = compute_esp32_image_hash(&ctx, &fw, 0x0, 0x8000, 0x10000).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(ctx.image_hash_memo.len(), 1, "memo must not grow on a hit");
    }

    /// When any of the three files changes on disk, the memo
    /// invalidates via `mtime` change and the hash recomputes.
    /// We assert the hash *differs* because the file contents
    /// changed — so this catches both the bytes going through the
    /// hasher AND the invalidation path.
    #[test]
    fn memo_miss_on_firmware_mtime_change() {
        let tmp = tempfile::tempdir().unwrap();
        seed_image(tmp.path());
        let ctx = fresh_ctx();
        let fw = tmp.path().join("firmware.bin");

        let h1 = compute_esp32_image_hash(&ctx, &fw, 0x0, 0x8000, 0x10000).unwrap();
        // Rewrite firmware.bin with new content; `std::fs::File::create`
        // bumps the `mtime` on Windows with enough resolution (100 ns)
        // even for sub-millisecond follow-ups.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&fw, b"FW__SECOND_BUILD_DIFFERENT");

        let h2 = compute_esp32_image_hash(&ctx, &fw, 0x0, 0x8000, 0x10000).unwrap();
        assert_ne!(h1, h2, "content-changed image must hash to a new value");
    }

    /// Missing files on disk short-circuit to `None` — the caller
    /// falls through to the regular verify-flash path instead of
    /// trust-skipping with a stale hash. The memo must NOT store an
    /// entry for an input that couldn't be hashed.
    #[test]
    fn memo_skipped_when_inputs_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // Only create firmware.bin — bootloader/partitions absent.
        write(&tmp.path().join("firmware.bin"), b"FW");
        let ctx = fresh_ctx();
        let fw = tmp.path().join("firmware.bin");

        assert!(compute_esp32_image_hash(&ctx, &fw, 0x0, 0x8000, 0x10000).is_none());
        assert_eq!(
            ctx.image_hash_memo.len(),
            0,
            "memo must not record entries for inputs that fail to hash"
        );
    }
}
