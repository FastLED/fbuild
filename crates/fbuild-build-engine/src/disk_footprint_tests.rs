//! Tests for per-stage disk footprint measurement (FastLED/fbuild#1750 item 1).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::disk_footprint::*;
use crate::disk_preflight::FreeSpace;

const GIB: u64 = 1024 * 1024 * 1024;

/// A volume whose free space the test scripts: each call returns the next
/// value, and the last value repeats once the script runs out.
struct ScriptedFreeSpace {
    scripts: HashMap<NormalizedPath, RefCell<VecDeque<u64>>>,
    unmeasurable: Vec<NormalizedPath>,
}

impl ScriptedFreeSpace {
    fn new(scripts: &[(&str, &[u64])]) -> Self {
        Self {
            scripts: scripts
                .iter()
                .map(|(path, readings)| {
                    (
                        NormalizedPath::new(path),
                        RefCell::new(readings.iter().copied().collect()),
                    )
                })
                .collect(),
            unmeasurable: Vec::new(),
        }
    }

    fn unmeasurable(mut self, path: &str) -> Self {
        self.unmeasurable.push(NormalizedPath::new(path));
        self
    }
}

impl FreeSpace for ScriptedFreeSpace {
    fn available(&self, path: &Path) -> io::Result<u64> {
        let key = NormalizedPath::from(path);
        if self.unmeasurable.contains(&key) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        }
        let script = self
            .scripts
            .get(&key)
            .unwrap_or_else(|| panic!("unscripted root {key}"));
        let mut readings = script.borrow_mut();
        let next = readings.pop_front();
        match next {
            Some(bytes) => Ok(bytes),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "script exhausted")),
        }
    }
}

fn roots() -> Vec<FootprintRoot> {
    vec![
        FootprintRoot::new("cache", "/cache-root"),
        FootprintRoot::new("project", "/proj/build"),
    ]
}

/// The scripts every stage test uses: cache loses 2 GiB, then 1 GiB, then gains
/// 5 GiB; the project volume never moves.
fn typical_scripts() -> ScriptedFreeSpace {
    ScriptedFreeSpace::new(&[
        ("/cache-root", &[10 * GIB, 8 * GIB, 7 * GIB, 12 * GIB]),
        ("/proj/build", &[40 * GIB, 40 * GIB, 40 * GIB, 40 * GIB]),
    ])
}

#[test]
fn disabled_recorder_samples_nothing() {
    let probe = typical_scripts();
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), false);
    assert!(!footprint.is_active());
    footprint.sample("pioarduino-resolve");
    assert!(footprint.stages().is_empty(), "off means no work at all");
    assert_eq!(footprint.peak_consumed("cache"), None);
}

#[test]
fn stages_record_consumed_and_freed_bytes() {
    let probe = typical_scripts();
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
    footprint.sample("pioarduino-resolve");
    footprint.sample("compile");
    footprint.sample("clean");

    let stages = footprint.stages();
    assert_eq!(stages.len(), 3);
    assert_eq!(stages[0].stage(), "pioarduino-resolve");
    assert_eq!(stages[0].delta_for("cache"), Some(2 * GIB as i64));
    assert_eq!(stages[0].delta_for("project"), Some(0));
    assert_eq!(stages[1].delta_for("cache"), Some(GIB as i64));
    assert_eq!(
        stages[2].delta_for("cache"),
        Some(-(5 * GIB as i64)),
        "a volume that gains space reports the freed bytes"
    );
}

#[test]
fn peak_is_the_largest_consumption_across_stages() {
    let probe = typical_scripts();
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
    footprint.sample("pioarduino-resolve");
    footprint.sample("compile");
    footprint.sample("clean");

    assert_eq!(footprint.peak_consumed("cache"), Some(3 * GIB));
    assert_eq!(
        footprint.peak_consumed("project"),
        Some(0),
        "an untouched volume peaks at zero"
    );
    assert_eq!(footprint.peak_consumed("nonexistent"), None);
}

#[test]
fn freed_space_never_becomes_a_peak() {
    let probe = ScriptedFreeSpace::new(&[("/cache-root", &[10 * GIB, 12 * GIB])]);
    let mut footprint = DiskFootprint::for_test(
        "esp32",
        &probe,
        vec![FootprintRoot::new("cache", "/cache-root")],
        true,
    );
    footprint.sample("gc-ran");
    assert_eq!(
        footprint.stages()[0].delta_for("cache"),
        Some(-(2 * GIB as i64))
    );
    assert_eq!(footprint.peak_consumed("cache"), Some(0));
}

#[test]
fn unmeasurable_root_is_skipped_and_named_as_unmeasurable() {
    let probe = ScriptedFreeSpace::new(&[("/cache-root", &[10 * GIB, 9 * GIB])])
        .unmeasurable("/proj/build");
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
    footprint.sample("compile");

    let deltas = footprint.stages()[0].deltas();
    assert_eq!(deltas.len(), 1, "only the readable root is recorded");
    assert_eq!(deltas[0].label(), "cache");
    assert_eq!(footprint.peak_consumed("project"), None);
}

#[test]
fn summary_is_one_line_naming_the_stages_and_peaks() {
    let probe = typical_scripts();
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
    footprint.sample("pioarduino-resolve");
    footprint.sample("compile");

    let line = footprint.summary_line();
    assert!(!line.contains('\n'), "the build log gets one line: {line}");
    assert!(line.contains("pioarduino-resolve: cache +2.00GB"), "{line}");
    assert!(line.contains("compile: cache +1.00GB"), "{line}");
    assert!(line.contains("peak consumed cache 3.00GB"), "{line}");
}

#[test]
fn json_line_carries_stages_peaks_and_start_free() {
    let probe = typical_scripts();
    let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
    footprint.sample("pioarduino-resolve");

    let value = footprint.summary_json();
    assert_eq!(value["label"], "esp32");
    assert_eq!(value["stages"]["pioarduino-resolve"]["cache"], 2 * GIB);
    assert_eq!(value["peak_bytes"]["cache"], 2 * GIB);
    assert_eq!(value["start_free_bytes"]["cache"], 10 * GIB);
    assert!(value["unix_ms"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn json_sink_appends_one_line_per_build() {
    let dir = tempfile::tempdir_in(fbuild_paths::temp_subdir("disk-footprint-tests"))
        .expect("scratch dir under the cache root");
    let path = dir.path().join("footprint.jsonl");
    let probe = typical_scripts();
    for _ in 0..2 {
        let mut footprint = DiskFootprint::for_test("esp32", &probe, roots(), true);
        footprint.sample("pioarduino-resolve");
        footprint.write_json(&path).expect("append");
    }
    let text = std::fs::read_to_string(&path).expect("read sink");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();
    assert_eq!(lines.len(), 2, "measurements accumulate");
    assert_eq!(lines[0]["stages"]["pioarduino-resolve"]["cache"], 2 * GIB);
}
