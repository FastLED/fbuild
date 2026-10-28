//! Tests for `fbuild cache reclaim` (FastLED/fbuild#1750 item 3).
//!
//! The eviction walk itself is tested against real files in
//! `fbuild-packages-fetch` (`disk_cache::reclaim`); what matters here is the
//! CLI contract: never mid-build, ordered rendering, honest verdict.

use std::path::Path;

use fbuild_packages::disk_cache::budget::parse_human_size;
use fbuild_packages::disk_cache::paths::Kind;
use fbuild_packages::disk_cache::reclaim::{ReclaimReport, ReclaimStep};

use super::{busy_reason, render_reclaim};

fn step(kind: Kind, installed: (u64, u64), archives: (u64, u64)) -> ReclaimStep {
    ReclaimStep {
        kind,
        installed_evicted: installed.0,
        installed_bytes_freed: installed.1,
        archives_evicted: archives.0,
        archive_bytes_freed: archives.1,
    }
}

#[test]
fn a_build_in_progress_is_reported_with_its_operation() {
    let json = r#"{
        "state": "Building",
        "message": "building blinky for esp32dev",
        "operation_in_progress": true,
        "current_operation": "build"
    }"#;
    let reason = busy_reason(json).expect("a live build must block reclaim");
    assert!(reason.contains("build"), "{reason}");
    assert!(reason.contains("blinky"), "{reason}");
}

#[test]
fn an_idle_daemon_does_not_block_reclaim() {
    let json = r#"{
        "state": "Idle",
        "message": "Daemon idle",
        "operation_in_progress": false
    }"#;
    assert_eq!(busy_reason(json), None);
}

#[test]
fn a_missing_or_unreadable_status_never_blocks_reclaim() {
    assert_eq!(busy_reason(""), None);
    assert_eq!(busy_reason("not json"), None);
    assert_eq!(
        busy_reason(r#"{"state": "Idle"}"#),
        None,
        "an older status without the field means idle"
    );
    assert_eq!(
        busy_reason(r#"{"operation_in_progress": "yes"}"#),
        None,
        "a non-boolean is not a running build"
    );
}

#[test]
fn steps_render_in_eviction_order_with_a_satisfied_verdict() {
    let mut report = ReclaimReport::new(2 * 1024 * 1024 * 1024, false);
    report.push_step(step(Kind::Toolchains, (2, 1_500_000_000), (1, 300_000_000)));
    report.push_step(step(Kind::Frameworks, (3, 400_000_000), (0, 0)));

    let text = render_reclaim(&report, Path::new("/cache/fbuild"));

    let toolchains = text.find("toolchains").expect("toolchains listed");
    let frameworks = text.find("frameworks").expect("frameworks listed");
    assert!(toolchains < frameworks, "eviction order: {text}");
    assert!(text.contains("satisfied"), "{text}");
    assert!(
        text.contains("6 entries evicted"),
        "every step's count is summed: {text}"
    );
    assert!(
        !text.contains("zccache"),
        "a satisfied run must not lecture about zccache: {text}"
    );
    assert!(!text.contains("dry run"), "{text}");
}

#[test]
fn a_short_run_says_so_and_names_the_daemon_owned_step() {
    let mut report = ReclaimReport::new(10 * 1024 * 1024 * 1024, false);
    report.push_step(step(Kind::Toolchains, (1, 1_000_000_000), (0, 0)));

    let text = render_reclaim(&report, Path::new("/cache/fbuild"));

    assert!(text.contains("SHORT"), "{text}");
    assert!(text.contains("zccache"), "{text}");
    assert!(text.contains("fbuild purge"), "actionable: {text}");
    assert!(!report.is_satisfied());
}

#[test]
fn a_dry_run_is_labelled() {
    let mut report = ReclaimReport::new(1024, true);
    report.push_step(step(Kind::LnkBlobs, (1, 2048), (0, 0)));

    let text = render_reclaim(&report, Path::new("/cache/fbuild"));

    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("nothing deleted"), "{text}");
}

#[test]
fn an_empty_report_says_nothing_is_reclaimable() {
    let report = ReclaimReport::new(4096, false);

    let text = render_reclaim(&report, Path::new("/cache/fbuild"));

    assert!(text.contains("nothing reclaimable"), "{text}");
    assert!(
        text.contains("SHORT"),
        "an empty run is a shortfall: {text}"
    );
}

#[test]
fn the_target_accepts_human_sizes_and_rejects_junk() {
    assert_eq!(parse_human_size("2G"), Some(2 * 1024 * 1024 * 1024));
    assert_eq!(parse_human_size("512M"), Some(512 * 1024 * 1024));
    assert_eq!(parse_human_size("1048576"), Some(1_048_576));
    assert_eq!(parse_human_size("two gigs"), None);
}
