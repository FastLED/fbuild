//! Cross-project `size` report reuse (FastLED/fbuild#1894).

use super::{content_key, prune_to, restore, store};
use fbuild_core::SizeInfo;

fn report(total_flash: u64) -> SizeInfo {
    SizeInfo {
        text: total_flash,
        data: 20,
        bss: 30,
        total_flash,
        total_ram: 50,
        max_flash: None,
        max_ram: None,
    }
}

/// The report is reused across directories, so its key must ignore the path
/// and follow exactly the inputs that can change the numbers.
#[test]
fn size_report_key_ignores_path_and_follows_inputs() {
    let first = tempfile::TempDir::new().unwrap();
    let second = tempfile::TempDir::new().unwrap();
    let elf_a = first.path().join("firmware.elf");
    let elf_b = second.path().join("firmware.elf");
    std::fs::write(&elf_a, b"elf-bytes").unwrap();
    std::fs::write(&elf_b, b"elf-bytes").unwrap();

    let identity = ["fingerprint=1", "size_tool=/sdk/bin/size"];
    let key_a = content_key(&elf_a, &identity).unwrap();
    let key_b = content_key(&elf_b, &identity).unwrap();
    assert_eq!(key_a, key_b, "same bytes in another directory share a key");

    std::fs::write(&elf_b, b"elf-bytez").unwrap();
    assert_ne!(
        key_a,
        content_key(&elf_b, &identity).unwrap(),
        "different ELF bytes must not share a key"
    );

    let newer_tool = ["fingerprint=1", "size_tool=/sdk-1.0.3/bin/size"];
    assert_ne!(
        key_a,
        content_key(&elf_a, &newer_tool).unwrap(),
        "a different size binary must not share a key"
    );

    assert!(
        content_key(&second.path().join("absent.elf"), &identity).is_none(),
        "an unreadable ELF is a miss, never a panic"
    );
}

#[test]
fn size_report_round_trip_across_directories() {
    let cache = tempfile::TempDir::new().unwrap();
    store(cache.path(), "deadbeef", &report(4096));

    let restored = restore(cache.path(), "deadbeef").expect("a stored report must restore");
    assert_eq!(restored.total_flash, 4096);
    assert_eq!(restored.total_ram, 50);
    assert!(
        restore(cache.path(), "cafebabe").is_none(),
        "an unknown key is a miss"
    );
}

#[test]
fn size_report_prune_keeps_the_newest_entries() {
    let cache = tempfile::TempDir::new().unwrap();
    for key in ["older", "newest"] {
        store(cache.path(), key, &report(1));
        // Distinct mtimes decide the eviction order; 30 ms is far above the
        // timestamp granularity of every filesystem CI runs on.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }

    let older = cache.path().join("older.json");
    let newest = cache.path().join("newest.json");
    // Room for exactly one report: the oldest must go, the newest must stay.
    let budget = std::fs::metadata(&newest).unwrap().len();
    prune_to(cache.path(), budget);
    assert!(!older.exists(), "the oldest entry must be evicted first");
    assert!(newest.exists(), "the newest entry must survive the prune");
}
