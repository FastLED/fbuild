//! Tests for on-demand cache reclaim (FastLED/fbuild#1750 item 3).

use rusqlite::params;

use super::super::index::{CacheEntry, CacheIndex};
use super::super::paths::{self, Kind};
use super::{RECLAIM_ORDER, reclaim};

/// A throwaway cache root plus an index over it.
///
/// Rooted under fbuild's own scratch tree (`tempdir_in`), never `$TMPDIR`:
/// GATE-012 forbids an unrooted temp directory, and the reclaim walk deletes
/// whatever it finds under this root.
struct Fixture {
    _tmp: tempfile::TempDir,
    index: CacheIndex,
}

impl Fixture {
    /// The cache root the walk deletes under.
    fn root(&self) -> &std::path::Path {
        self._tmp.path()
    }
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir_in(fbuild_paths::temp_subdir("disk-cache-reclaim-tests"))
        .expect("cache-root tmp dir");
    let index = CacheIndex::open(tmp.path()).expect("index");
    Fixture { _tmp: tmp, index }
}

/// Extract an install of `bytes` bytes under the cache root and register it.
fn install(fx: &Fixture, kind: Kind, url: &str, bytes: usize) -> CacheEntry {
    let rel = format!("installed/{}/{}", kind.as_str(), slug(url));
    let dir = fx.root().join(&rel);
    std::fs::create_dir_all(&dir).expect("install dir");
    std::fs::write(dir.join("payload.bin"), vec![b'x'; bytes]).expect("payload");
    fx.index
        .record_install(kind, url, "1.0", &rel, bytes as i64)
        .expect("record install")
}

/// Same, for the downloaded-archive half: one file, not a directory.
fn archive(fx: &Fixture, kind: Kind, url: &str, bytes: usize) -> CacheEntry {
    let rel = format!("archives/{}/{}.tar.zst", kind.as_str(), slug(url));
    let path = fx.root().join(&rel);
    std::fs::create_dir_all(path.parent().expect("archive parent")).expect("archive dir");
    std::fs::write(&path, vec![b'x'; bytes]).expect("archive payload");
    fx.index
        .record_archive(kind, url, "1.0", &rel, bytes as i64, "sha")
        .expect("record archive")
}

fn slug(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("entry").to_string()
}

/// Pin an entry exactly the way a running build does (`pinned = 1`), so reclaim
/// must not see it.
fn pin(fx: &Fixture, entry: &CacheEntry) {
    fx.index
        .pin(entry.id, std::process::id(), 1)
        .expect("pin entry");
}

fn dir_exists(fx: &Fixture, rel: &str) -> bool {
    fx.root().join(rel).exists()
}

#[test]
fn eviction_order_is_cheapest_to_refetch_first() {
    assert_eq!(
        RECLAIM_ORDER,
        [
            Kind::Toolchains,
            Kind::Packages,
            Kind::Platforms,
            Kind::Libraries,
            Kind::Frameworks,
            Kind::LnkBlobs,
        ],
        "the order is the contract #1750 item 3 asks for"
    );
}

#[test]
fn a_satisfied_target_stops_before_the_next_kind() {
    let fx = fixture();
    let tool = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        500,
    );
    let framework = install(&fx, Kind::Frameworks, "https://example.com/fw.tar.gz", 900);
    assert!(dir_exists(&fx, tool.installed_path.as_deref().unwrap()));

    let report = reclaim(&fx.index, 500, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 500);
    assert!(report.is_satisfied());
    assert_eq!(report.evicted(), 1);
    assert_eq!(report.steps().len(), 1, "frameworks must not be touched");
    assert_eq!(report.steps()[0].kind, Kind::Toolchains);
    assert!(!dir_exists(&fx, tool.installed_path.as_deref().unwrap()));
    assert!(
        dir_exists(&fx, framework.installed_path.as_deref().unwrap()),
        "a satisfied target must not keep evicting"
    );
}

#[test]
fn it_stops_inside_a_kind_once_the_target_is_met() {
    let fx = fixture();
    let first = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/first.tar.gz",
        400,
    );
    let second = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/second.tar.gz",
        400,
    );
    // `first` is recorded earlier, so it is the LRU head.
    make_older(&fx, &first, 1);
    make_older(&fx, &second, 2);

    let report = reclaim(&fx.index, 400, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 400);
    assert!(!dir_exists(&fx, first.installed_path.as_deref().unwrap()));
    assert!(
        dir_exists(&fx, second.installed_path.as_deref().unwrap()),
        "the second entry stays: the target was already met"
    );
}

#[test]
fn installed_goes_before_archive_within_a_kind() {
    let fx = fixture();
    let entry = archive(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        1000,
    );
    let installed = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        700,
    );
    assert_eq!(entry.id, installed.id, "one row, both phases");

    // Ask for exactly the installed half: the archive must survive.
    let report = reclaim(&fx.index, 700, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 700);
    assert_eq!(report.steps()[0].installed_evicted, 1);
    assert_eq!(
        report.steps()[0].archives_evicted,
        0,
        "an archive is only evicted after its install"
    );
    assert!(dir_exists(&fx, entry.archive_path.as_deref().unwrap()));
}

#[test]
fn a_dry_run_reports_the_same_bytes_and_deletes_nothing() {
    let fx = fixture();
    let tool = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        600,
    );
    let rel = tool.installed_path.clone().unwrap();

    // Reclaim evicts whole entries, so a 400-byte target frees the one 600-byte
    // entry rather than half of it — the report says 600 in both modes.
    let dry = reclaim(&fx.index, 400, true).expect("dry run");
    assert!(dry.dry_run());
    assert_eq!(dry.freed_bytes(), 600);
    assert!(dry.is_satisfied());
    assert!(dir_exists(&fx, &rel), "a dry run deletes nothing");

    let real = reclaim(&fx.index, 400, false).expect("real run");
    assert_eq!(
        real.freed_bytes(),
        dry.freed_bytes(),
        "dry-run and real numbers must agree on an unchanged cache"
    );
    assert_eq!(real.evicted(), dry.evicted());
    assert!(!dir_exists(&fx, &rel));
}

#[test]
fn pinned_entries_are_never_evicted() {
    let fx = fixture();
    let leased = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/leased.tar.gz",
        800,
    );
    pin(&fx, &leased);

    let report = reclaim(&fx.index, 800, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 0, "a leased entry is invisible");
    assert!(!report.is_satisfied());
    assert_eq!(report.remaining_bytes(), 800);
    assert!(dir_exists(&fx, leased.installed_path.as_deref().unwrap()));
}

#[test]
fn a_zero_target_does_nothing() {
    let fx = fixture();
    let tool = install(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        600,
    );

    let report = reclaim(&fx.index, 0, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 0);
    assert!(report.steps().is_empty());
    assert!(report.is_satisfied());
    assert!(dir_exists(&fx, tool.installed_path.as_deref().unwrap()));
}

#[test]
fn an_unsatisfiable_target_evicts_everything_and_reports_what_is_left() {
    let fx = fixture();
    install(
        &fx,
        Kind::Toolchains,
        "https://example.com/tool.tar.gz",
        300,
    );
    install(&fx, Kind::Frameworks, "https://example.com/fw.tar.gz", 200);
    install(&fx, Kind::LnkBlobs, "https://example.com/blob.bin", 100);

    let report = reclaim(&fx.index, 10_000, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 600);
    assert_eq!(report.remaining_bytes(), 9_400);
    assert!(!report.is_satisfied());
    let kinds: Vec<Kind> = report.steps().iter().map(|step| step.kind).collect();
    assert_eq!(
        kinds,
        [Kind::Toolchains, Kind::Frameworks, Kind::LnkBlobs],
        "steps are reported in eviction order: {kinds:?}"
    );
    assert_eq!(fx.index.entry_count().expect("count"), 0);
}

#[test]
fn a_vanished_path_evicts_its_row_instead_of_failing() {
    let fx = fixture();
    // Recorded but never written: a crash between index write and extract.
    fx.index
        .record_install(
            Kind::Packages,
            "https://example.com/gone.tar.gz",
            "1.0",
            "installed/packages/gone",
            1_200,
        )
        .expect("record install");

    let report = reclaim(&fx.index, 1_200, false).expect("reclaim");

    assert_eq!(report.freed_bytes(), 1_200);
    assert_eq!(fx.index.entry_count().expect("count"), 0);
    assert!(!dir_exists(&fx, "installed/packages/gone"));
}

/// Force an entry to the LRU head: `last_used_at` is second-granular, so two
/// entries recorded in the same second would otherwise tie.
fn make_older(fx: &Fixture, entry: &CacheEntry, seconds: i64) {
    let conn = rusqlite::Connection::open(paths::index_path(fx.root())).expect("index db");
    conn.execute(
        "UPDATE entries SET last_used_at = ?1 WHERE id = ?2",
        params![seconds, entry.id],
    )
    .expect("bump lru clock");
}
