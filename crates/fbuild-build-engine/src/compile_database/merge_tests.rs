//! Re-rooting and merging tests for the delegated compile database
//! (FastLED/fbuild#1973).
//!
//! The fixture is a reduced capture of a delegated ESP-IDF database: the same
//! shape, prefixes and invocation spellings as the measured 959-entry file
//! (7.39 MB for a two-component application), cut down to the five entry kinds
//! the merge has to get right — the sketch, a project component, two framework
//! components and a generated source inside the build directory.

use fbuild_core::path::NormalizedPath;

use super::*;

/// The captured fixture, as committed.
const FIXTURE: &str = include_str!("../../testdata/idf-compile-commands.json");

/// A scratch directory holding a database file written from [`FIXTURE`].
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("compile_commands.json"), FIXTURE).unwrap();
        Self { dir }
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn root(&self) -> NormalizedPath {
        NormalizedPath::new(self.dir.path())
    }

    fn delegated(&self) -> Vec<DatabaseEntry> {
        read_database(&self.root().join("compile_commands.json")).unwrap()
    }

    fn project(&self) -> NormalizedPath {
        NormalizedPath::new("/proj")
    }
}

fn native_entry(directory: &str, file: &str) -> DatabaseEntry {
    DatabaseEntry {
        directory: directory.to_string(),
        file: file.to_string(),
        output: Some(format!("/proj/build/obj/{}.o", file.replace('/', "_"))),
        invocation: Invocation::Arguments {
            arguments: vec![
                "/opt/tools/gcc".to_string(),
                "-c".to_string(),
                file.to_string(),
            ],
        },
    }
}

fn files(entries: &[DatabaseEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.file.clone()).collect()
}

/// Nothing in `directory`/`file` may still name the delegate's scratch build
/// directory or the IDF checkout.
#[test]
fn reroot_removes_scratch_and_checkout_prefixes() {
    let fixture = Fixture::new();
    let rerooted = reroot(fixture.delegated(), &fixture.project());

    for entry in &rerooted {
        assert_eq!(entry.directory, "/proj", "directory: {entry:?}");
        assert!(
            !entry.file.starts_with("/proj/build/idf"),
            "build-dir prefix survived in {}",
            entry.file
        );
        assert!(
            !entry.file.starts_with("/opt/idf"),
            "IDF-checkout prefix survived in {}",
            entry.file
        );
        assert!(
            !entry.file.starts_with('/'),
            "file is still absolute: {}",
            entry.file
        );
        let output = entry.output.as_deref().unwrap_or_default();
        assert!(
            !output.starts_with("/proj/build/idf"),
            "build-dir prefix survived in {output}"
        );
    }
}

/// The relative spelling is rooted at the project: project sources are plain
/// relative paths, framework sources climb out of it.
#[test]
fn reroot_spells_sources_relative_to_the_project() {
    let fixture = Fixture::new();
    let rerooted = files(&reroot(fixture.delegated(), &fixture.project()));

    assert_eq!(
        rerooted,
        vec![
            "main/main.c".to_string(),
            "components/mycomp/mycomp.c".to_string(),
            "../opt/idf/components/freertos/FreeRTOS-Kernel/tasks.c".to_string(),
            "../opt/idf/components/esp_system/port/panic_handler.c".to_string(),
            "build/idf/project_elf_src_esp32s3.c".to_string(),
        ]
    );
}

/// `command`/`arguments` is the replayable payload: re-rooting must not touch
/// a byte of it, because only it may name the real compiler and flag paths.
#[test]
fn reroot_preserves_the_invocation_payload() {
    let fixture = Fixture::new();
    let original = fixture.delegated();
    let rerooted = reroot(original.clone(), &fixture.project());

    assert_eq!(rerooted.len(), original.len());
    for (before, after) in original.iter().zip(rerooted.iter()) {
        assert_eq!(before.invocation, after.invocation);
        assert_eq!(before.output.is_some(), after.output.is_some());
    }
}

/// A file fbuild compiled itself keeps fbuild's entry: its compiler and flags
/// are the ones the rest of the build's artifacts agree with.
#[test]
fn merge_keeps_fbuilds_entry_for_a_file_both_compiled() {
    let fixture = Fixture::new();
    let delegated = reroot(fixture.delegated(), &fixture.project());
    let native = vec![native_entry("/proj", "/proj/main/main.c")];

    let merged = merge(&native, delegated);
    let sketch = merged
        .iter()
        .find(|entry| entry.file == "/proj/main/main.c")
        .expect("the sketch entry survives the merge");

    assert_eq!(sketch.invocation, native[0].invocation);
    assert_eq!(
        merged
            .iter()
            .filter(|entry| entry.file == "/proj/main/main.c")
            .count(),
        1,
        "one entry per translation unit"
    );
}

/// Everything only IDF compiled comes from the delegated side, one entry each.
#[test]
fn merge_adds_framework_entries_only_idf_compiled() {
    let fixture = Fixture::new();
    let delegated = reroot(fixture.delegated(), &fixture.project());
    let native = vec![native_entry("/proj", "/proj/main/main.c")];

    let merged = merge(&native, delegated);

    assert_eq!(merged.len(), 5);
    for framework in [
        "../opt/idf/components/freertos/FreeRTOS-Kernel/tasks.c",
        "../opt/idf/components/esp_system/port/panic_handler.c",
        "components/mycomp/mycomp.c",
    ] {
        assert!(
            merged.iter().any(|entry| entry.file == framework),
            "missing delegated entry {framework}"
        );
    }
    assert!(
        merged
            .iter()
            .find(|entry| entry.file == "components/mycomp/mycomp.c")
            .is_some_and(|entry| matches!(entry.invocation, Invocation::Command { .. })),
        "the delegated entry keeps its `command` spelling"
    );
}

/// The merged order is the translation-unit key order, not the input order.
#[test]
fn merge_is_independent_of_input_order() {
    let fixture = Fixture::new();
    let mut delegated = reroot(fixture.delegated(), &fixture.project());
    let forward = merge(&[], delegated.clone());
    delegated.reverse();
    let reversed = merge(&[], delegated);

    assert_eq!(files(&forward), files(&reversed));
}

/// Two identical builds produce a byte-identical database, and therefore the
/// same digest.
#[test]
fn identical_builds_write_identical_bytes() {
    let fixture = Fixture::new();
    let delegated = reroot(fixture.delegated(), &fixture.project());
    let native = vec![native_entry("/proj", "/proj/main/main.c")];

    let first = merged_path(&fixture, "first.json");
    let second = merged_path(&fixture, "second.json");
    let first_digest = write_database(&first, &merge(&native, delegated.clone())).unwrap();
    let second_digest = write_database(&second, &merge(&native, delegated)).unwrap();

    assert_eq!(first_digest, second_digest);
    assert_eq!(
        std::fs::read(first.as_path()).unwrap(),
        std::fs::read(second.as_path()).unwrap()
    );
    assert_eq!(first_digest.len(), 64);
}

/// fbuild's own database is what a non-delegated build writes: merging it
/// with nothing reproduces that file byte for byte.
#[test]
fn native_only_database_is_written_byte_for_byte() {
    let fixture = Fixture::new();
    let mut native = crate::compile_database::CompileDatabase::new();
    native.add_entry(crate::compile_database::CompileEntry {
        arguments: vec![
            "/opt/tools/xtensa-esp32s3-elf/bin/g++".to_string(),
            "-std=gnu++17".to_string(),
            "-c".to_string(),
            "/proj/src/main.cpp".to_string(),
            "-o".to_string(),
            "/proj/build/main.cpp.o".to_string(),
        ],
        directory: "/proj".to_string(),
        file: "/proj/src/main.cpp".to_string(),
        output: Some("/proj/build/main.cpp.o".to_string()),
    });

    let fbuild_written = native.write(fixture.path()).unwrap();
    let merged_path = merged_path(&fixture, "merged.json");
    write_database(&merged_path, &merge(&native_entries(&native), Vec::new())).unwrap();

    assert_eq!(
        std::fs::read(&fbuild_written).unwrap(),
        std::fs::read(merged_path.as_path()).unwrap(),
        "a native-only merge must reproduce fbuild's own spelling exactly"
    );
}

/// A database that is not a JSON compilation database is an error naming the
/// file, never an empty merge.
#[test]
fn malformed_database_is_an_error() {
    let fixture = Fixture::new();
    let broken = fixture.root().join("broken.json");
    std::fs::write(broken.as_path(), b"{\"not\": \"an array\"}").unwrap();

    let error = read_database(&broken).unwrap_err().to_string();
    assert!(error.contains("broken.json"), "{error}");

    let missing = fixture.root().join("missing.json");
    assert!(read_database(&missing).is_err());
}

fn merged_path(fixture: &Fixture, name: &str) -> NormalizedPath {
    fixture.root().join(name)
}
