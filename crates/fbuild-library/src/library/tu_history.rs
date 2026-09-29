//! Per-translation-unit compile duration history (FastLED/fbuild#1564).
//!
//! Records how long each TU took to compile, keyed on the source path plus
//! its compile signature, so a later build of the same project can dispatch
//! the longest TUs first instead of guessing from source order alone. The
//! history lives under the fbuild cache root, so it survives `clean` of the
//! build dir, and it affects dispatch **order only** — it never touches
//! object files, rebuild signatures, or cache keys. A missing or corrupt
//! record is silently treated as "no history".

use std::time::Duration;

use fbuild_core::path::NormalizedPath;

/// Subdirectory of the cache root holding one small file per TU record.
const HISTORY_DIR: &str = "tu-durations";

/// Default estimated cost (ms) for a C++ TU with no recorded history.
const DEFAULT_CPP_ESTIMATE_MS: u64 = 1000;
/// Default estimated cost (ms) for an assembly TU with no recorded history.
const DEFAULT_ASM_ESTIMATE_MS: u64 = 10;

/// The dispatch rank fallback used when no history is available for a TU:
/// C++ starts first, then C, then assembly (FastLED/fbuild#1537/#1553).
fn dispatch_rank(source: &std::path::Path) -> u8 {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("c") => 1,
        Some("s" | "sx" | "asm") => 2,
        _ => 0,
    }
}

/// The history key for a (source, signature) pair: a blake3 hash of the
/// canonical-ish source path string plus the compile signature, so an
/// identical source compiled with a different flag set (a different
/// signature) gets its own record.
fn history_key(source: &std::path::Path, signature: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(NormalizedPath::new(source).key().as_bytes());
    hasher.update(&[0]);
    hasher.update(signature.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Record `duration` for `(source, signature)` under the global cache root.
/// Errors are logged at debug level and otherwise ignored: history is a
/// best-effort optimization, never a build requirement.
pub fn record(source: &std::path::Path, signature: &str, duration: Duration) {
    let root = fbuild_paths::get_cache_root();
    record_in(&root, source, signature, duration);
}

/// Look up a previously recorded duration for `(source, signature)` under
/// the global cache root. Returns `None` on any missing or corrupt record.
pub fn lookup(source: &std::path::Path, signature: &str) -> Option<Duration> {
    let root = fbuild_paths::get_cache_root();
    lookup_in(&root, source, signature)
}

/// A process-wide counter guaranteeing a unique temp-file name even when two
/// compiles for the same `(source, signature)` race to record (two builds of
/// the same library at once): the pid alone is not enough to disambiguate
/// concurrent writers inside one process.
static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`record`], with an explicit history root — used directly by tests and by
/// callers that need a hermetic, non-global history location.
///
/// A record already present for this key is kept if `duration` is under 25%
/// of it: a cache hit (a few ms) must never overwrite a real compile's
/// measured cost (FastLED/fbuild#1564), since the *next* build would then
/// dispatch that TU last right when it needs to compile for real.
pub fn record_in(
    root: &std::path::Path,
    source: &std::path::Path,
    signature: &str,
    duration: Duration,
) {
    if let Some(previous) = lookup_in(root, source, signature) {
        if duration.as_millis() * 4 < previous.as_millis() {
            return;
        }
    }
    if let Err(e) = record_in_unconditional(root, source, signature, duration) {
        tracing::debug!(
            "tu_history: failed to record duration for {:?}: {e}",
            source
        );
    }
}

fn record_in_unconditional(
    root: &std::path::Path,
    source: &std::path::Path,
    signature: &str,
    duration: Duration,
) -> std::io::Result<()> {
    let dir = root.join(HISTORY_DIR);
    std::fs::create_dir_all(&dir)?;
    let key = history_key(source, signature);
    let final_path = dir.join(&key);
    let unique = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = dir.join(format!("{key}.{}.{unique}.tmp", std::process::id()));
    std::fs::write(&tmp_path, duration.as_millis().to_string())?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// [`lookup`], with an explicit history root — used directly by tests and by
/// callers that need a hermetic, non-global history location.
pub fn lookup_in(
    root: &std::path::Path,
    source: &std::path::Path,
    signature: &str,
) -> Option<Duration> {
    let key = history_key(source, signature);
    let path = root.join(HISTORY_DIR).join(key);
    let contents = std::fs::read_to_string(path).ok()?;
    let ms: u64 = contents.trim().parse().ok()?;
    Some(Duration::from_millis(ms))
}

/// Sort `items` longest-estimated-first for dispatch.
///
/// `key(item)` returns `(source, signature)` for a work item. The estimated
/// cost of an item is:
/// - its recorded duration, if `lookup` finds one;
/// - otherwise, a rank-based default: the median (upper median, for an
///   even-sized sample) of *this batch's* recorded C++ durations (if any are
///   known), else [`DEFAULT_CPP_ESTIMATE_MS`] for a C++ source, half that for
///   C, and [`DEFAULT_ASM_ESTIMATE_MS`] for assembly.
///
/// Sorting is by estimated cost **descending**. Ties break known-before-estimated
/// (a recorded duration always outranks a guess that happens to match it),
/// then by original (stable-sort) source order — so items with no history at
/// all fall back to the existing C++-before-C-before-asm, source-order
/// behavior.
pub fn dispatch_order<T>(items: &mut Vec<T>, key: impl Fn(&T) -> (NormalizedPath, String)) {
    dispatch_order_in(&fbuild_paths::get_cache_root(), items, key);
}

/// [`dispatch_order`], with an explicit history root — used directly by
/// tests and by callers that need a hermetic, non-global history location.
pub fn dispatch_order_in<T>(
    root: &std::path::Path,
    items: &mut Vec<T>,
    key: impl Fn(&T) -> (NormalizedPath, String),
) {
    let known: Vec<Option<Duration>> = items
        .iter()
        .map(|item| {
            let (source, signature) = key(item);
            lookup_in(root, source.as_path(), &signature)
        })
        .collect();

    let mut known_cpp_ms: Vec<u64> = known
        .iter()
        .zip(items.iter())
        .filter_map(|(d, item)| {
            let (source, _) = key(item);
            if dispatch_rank(source.as_path()) == 0 {
                d.map(|d| d.as_millis() as u64)
            } else {
                None
            }
        })
        .collect();
    let cpp_default_ms = if known_cpp_ms.is_empty() {
        DEFAULT_CPP_ESTIMATE_MS
    } else {
        known_cpp_ms.sort_unstable();
        known_cpp_ms[known_cpp_ms.len() / 2]
    };

    let estimate_ms = |idx: usize, source: &std::path::Path| -> u64 {
        if let Some(d) = known[idx] {
            return d.as_millis() as u64;
        }
        match dispatch_rank(source) {
            1 => cpp_default_ms / 2,
            2 => DEFAULT_ASM_ESTIMATE_MS,
            _ => cpp_default_ms,
        }
    };

    // Sort descending by estimated cost. A tie between a *known* duration and
    // an *estimated* one favors the known value (an unknown item's estimate
    // is only ever a guess, even when it happens to equal a real recorded
    // duration); ties between two items of the same kind keep source order.
    let mut indexed: Vec<(usize, u64, bool)> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let (source, _) = key(item);
            (
                idx,
                estimate_ms(idx, source.as_path()),
                known[idx].is_some(),
            )
        })
        .collect();
    indexed.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));

    let order: Vec<usize> = indexed.into_iter().map(|(idx, _, _)| idx).collect();
    let mut reordered: Vec<Option<T>> = items.drain(..).map(Some).collect();
    for idx in order {
        items.push(reordered[idx].take().expect("index used once"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn record_and_lookup_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/foo.cpp");
        record_in(dir.path(), source, "sig1", Duration::from_millis(1234));
        let found = lookup_in(dir.path(), source, "sig1");
        assert_eq!(found, Some(Duration::from_millis(1234)));
    }

    #[test]
    fn lookup_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/missing.cpp");
        assert_eq!(lookup_in(dir.path(), source, "sig1"), None);
    }

    #[test]
    fn lookup_corrupt_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/foo.cpp");
        record_in(dir.path(), source, "sig1", Duration::from_millis(1234));
        let key = history_key(source, "sig1");
        std::fs::write(dir.path().join(HISTORY_DIR).join(&key), "not-a-number").unwrap();
        assert_eq!(lookup_in(dir.path(), source, "sig1"), None);
    }

    #[test]
    fn different_signature_is_a_different_record() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/foo.cpp");
        record_in(dir.path(), source, "sig1", Duration::from_millis(100));
        assert_eq!(lookup_in(dir.path(), source, "sig2"), None);
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/foo.cpp");
        record_in(dir.path(), source, "sig1", Duration::from_millis(1));
        let history_dir = dir.path().join(HISTORY_DIR);
        let leftovers: Vec<_> = std::fs::read_dir(&history_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn dispatch_order_prefers_recorded_longest_first() {
        let dir = tempfile::tempdir().unwrap();
        let a = Path::new("/proj/a.cpp").to_path_buf();
        let b = Path::new("/proj/b.cpp").to_path_buf();
        let c = Path::new("/proj/c.cpp").to_path_buf();
        record_in(dir.path(), &a, "sig", Duration::from_millis(100));
        record_in(dir.path(), &b, "sig", Duration::from_millis(5000));
        record_in(dir.path(), &c, "sig", Duration::from_millis(500));

        let mut items = vec![a.clone(), b.clone(), c.clone()];
        dispatch_order_in(dir.path(), &mut items, |p| {
            (NormalizedPath::new(p), "sig".to_string())
        });
        assert_eq!(items, vec![b, c, a]);
    }

    #[test]
    fn dispatch_order_without_history_keeps_rank_then_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let cpp1 = Path::new("/proj/cpp1.cpp").to_path_buf();
        let c1 = Path::new("/proj/c1.c").to_path_buf();
        let asm1 = Path::new("/proj/asm1.s").to_path_buf();
        let cpp2 = Path::new("/proj/cpp2.cpp").to_path_buf();

        let mut items = vec![c1.clone(), asm1.clone(), cpp1.clone(), cpp2.clone()];
        dispatch_order_in(dir.path(), &mut items, |p| {
            (NormalizedPath::new(p), "sig".to_string())
        });
        assert_eq!(items, vec![cpp1, cpp2, c1, asm1]);
    }

    #[test]
    fn dispatch_order_mixed_known_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let known_slow = Path::new("/proj/known_slow.cpp").to_path_buf();
        let unknown_cpp = Path::new("/proj/unknown.cpp").to_path_buf();
        let known_fast = Path::new("/proj/known_fast.cpp").to_path_buf();

        // A known duration far above the default estimate sorts first.
        record_in(
            dir.path(),
            &known_slow,
            "sig",
            Duration::from_millis(20_000),
        );
        record_in(dir.path(), &known_fast, "sig", Duration::from_millis(1));

        let mut items = vec![unknown_cpp.clone(), known_slow.clone(), known_fast.clone()];
        dispatch_order_in(dir.path(), &mut items, |p| {
            (NormalizedPath::new(p), "sig".to_string())
        });
        assert_eq!(items[0], known_slow);
        assert_eq!(items[2], known_fast);
    }

    /// FastLED/fbuild#1564: a warm zccache hit compiles in a few ms; that must
    /// never overwrite a real (cold) compile's measured cost, or the next
    /// build would dispatch a genuinely slow TU last right when it needs to
    /// compile for real.
    #[test]
    fn record_in_keeps_the_slow_sample_over_a_later_fast_hit() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/fl.fx.cpp");
        record_in(dir.path(), source, "sig", Duration::from_millis(17_000));
        record_in(dir.path(), source, "sig", Duration::from_millis(20));
        assert_eq!(
            lookup_in(dir.path(), source, "sig"),
            Some(Duration::from_millis(17_000)),
            "a cache-hit-fast sample must not overwrite the real recorded duration"
        );
    }

    /// A duration in the same ballpark as (or slower than) the prior record
    /// still replaces it — only an implausibly fast sample is suspect.
    #[test]
    fn record_in_replaces_a_comparable_or_slower_sample() {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new("/proj/fl.fx.cpp");
        record_in(dir.path(), source, "sig", Duration::from_millis(1000));
        record_in(dir.path(), source, "sig", Duration::from_millis(1200));
        assert_eq!(
            lookup_in(dir.path(), source, "sig"),
            Some(Duration::from_millis(1200))
        );
    }
}
