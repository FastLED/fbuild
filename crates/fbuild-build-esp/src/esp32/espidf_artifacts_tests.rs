//! Artifact discovery/publication tests for the delegated ESP-IDF build
//! (FastLED/fbuild#1971).

use fbuild_core::path::NormalizedPath;

use super::*;

/// A build root laid out the way `idf.py build` leaves one: the app image at
/// the root, the bootloader and partition table nested in their own
/// directories, and `flasher_args.json` naming all three.
struct FakeBuildRoot {
    dir: tempfile::TempDir,
}

impl FakeBuildRoot {
    fn new(project: &str) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = Self { dir };
        root.write(&format!("{project}.bin"), 208_528);
        root.write(&format!("{project}.elf"), 3_721_344);
        root.write(&format!("{project}.map"), 3_108_610);
        root.write("bootloader/bootloader.bin", 21_056);
        root.write("partition_table/partition-table.bin", 3_072);
        root.write("project/sdkconfig", 73_024);
        root.write("compile_commands.json", 7_391_003);
        std::fs::write(
            root.path().join("flasher_args.json"),
            serde_json::json!({
                "flash_files": {
                    "0x0": "bootloader/bootloader.bin",
                    "0x8000": "partition_table/partition-table.bin",
                    "0x10000": format!("{project}.bin"),
                },
                "app": {"offset": "0x10000", "file": format!("{project}.bin")},
                "bootloader": {"offset": "0x0", "file": "bootloader/bootloader.bin"},
                "partition-table": {
                    "offset": "0x8000",
                    "file": "partition_table/partition-table.bin",
                },
            })
            .to_string(),
        )
        .unwrap();
        root
    }

    fn write(&self, relative: &str, bytes: usize) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, vec![0x5a; bytes]).unwrap();
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn root(&self) -> NormalizedPath {
        NormalizedPath::new(self.path())
    }

    /// The project directory IDF-generated `sdkconfig` lives in.
    fn project(&self) -> NormalizedPath {
        self.root().join("project")
    }
}

/// Every artifact #1971 names is discovered from IDF's own metadata, and the
/// two nested ones are published flat beside the rest.
#[test]
fn discovery_publishes_all_artifacts_flat() {
    let fake = FakeBuildRoot::new("component_demo");
    let artifacts = discover_and_publish(&fake.root(), &fake.project()).unwrap();

    assert_eq!(artifacts.project, "component_demo");
    assert_eq!(
        artifacts.app_bin.as_path(),
        fake.path().join("component_demo.bin")
    );
    assert_eq!(
        artifacts.elf.as_path(),
        fake.path().join("component_demo.elf")
    );
    assert_eq!(
        artifacts.map.as_path(),
        fake.path().join("component_demo.map")
    );
    assert_eq!(artifacts.sdkconfig.as_path(), fake.path().join("sdkconfig"));
    assert_eq!(
        artifacts.compile_commands.as_path(),
        fake.path().join("compile_commands.json")
    );

    // `idf.py` nests these two; the flat copies are what the flash path and
    // the follow-ups address by name.
    assert_eq!(
        artifacts.bootloader_bin.as_path(),
        fake.path().join("bootloader.bin")
    );
    assert_eq!(
        artifacts.partition_table_bin.as_path(),
        fake.path().join("partition-table.bin")
    );
    assert_eq!(
        std::fs::metadata(fake.path().join("bootloader.bin"))
            .unwrap()
            .len(),
        21_056
    );
    assert_eq!(
        std::fs::metadata(fake.path().join("partition-table.bin"))
            .unwrap()
            .len(),
        3_072
    );

    let expected = 208_528 + 3_721_344 + 3_108_610 + 21_056 + 3_072 + 73_024 + 7_391_003;
    assert_eq!(artifacts.total_bytes(), expected);
}

/// The index is the artifact proof the follow-ups consume: it must name every
/// published file with its real byte count, relative to the delegated root.
#[test]
fn index_records_every_artifact_with_bytes() {
    let fake = FakeBuildRoot::new("component_demo");
    let artifacts = discover_and_publish(&fake.root(), &fake.project()).unwrap();
    let index_path = artifacts
        .write_index("esp32s3", &fake.root(), "cold-only")
        .unwrap();
    assert_eq!(index_path.as_path(), fake.path().join(INDEX_FILE));

    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(index_path.as_path()).unwrap()).unwrap();
    assert_eq!(index["project"], "component_demo");
    assert_eq!(index["chip"], "esp32s3");
    // The index says how the build was cached, so a slow delegated build is
    // explained by its own artifact record (FastLED/fbuild#1974).
    assert_eq!(index["cache"], "cold-only");
    let files = index["files"].as_array().unwrap();
    assert_eq!(files.len(), 7);
    let names: Vec<&str> = files
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "app.bin",
            "app.elf",
            "app.map",
            "bootloader.bin",
            "partition-table.bin",
            "sdkconfig",
            "compile_commands.json",
        ]
    );
    assert_eq!(files[3]["bytes"], 21_056);
    assert!(
        files[3]["path"]
            .as_str()
            .unwrap()
            .ends_with("bootloader.bin"),
        "{:?}",
        files[3]
    );
}

/// An `idf.py` exit 0 with no build directory is a structured failure naming
/// the missing file, not an empty success.
#[test]
fn missing_flasher_args_is_an_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = NormalizedPath::new(dir.path());
    let error = discover_and_publish(&root, &root).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("flasher_args.json"), "{message}");
    assert!(message.contains("unreadable"), "{message}");
}

/// A region IDF named but did not write is reported by name.
#[test]
fn missing_app_image_is_reported_by_name() {
    let fake = FakeBuildRoot::new("component_demo");
    std::fs::remove_file(fake.path().join("component_demo.bin")).unwrap();
    let error = discover_and_publish(&fake.root(), &fake.project()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("app image"), "{message}");
    assert!(message.contains("component_demo.bin"), "{message}");
}

/// The compile database is part of the contract (#1973 re-roots it later), so
/// its absence must fail here rather than in a follow-up.
#[test]
fn missing_compile_database_is_an_error() {
    let fake = FakeBuildRoot::new("component_demo");
    std::fs::remove_file(fake.path().join("compile_commands.json")).unwrap();
    let error = discover_and_publish(&fake.root(), &fake.project()).unwrap_err();
    assert!(error.to_string().contains("compile database"), "{error}");
}

/// A flasher-args file without the required regions names what is missing
/// instead of publishing a partial set.
#[test]
fn flasher_args_without_a_bootloader_is_an_error() {
    let fake = FakeBuildRoot::new("component_demo");
    std::fs::write(
        fake.path().join("flasher_args.json"),
        serde_json::json!({
            "app": {"offset": "0x10000", "file": "component_demo.bin"},
        })
        .to_string(),
    )
    .unwrap();
    let error = discover_and_publish(&fake.root(), &fake.project()).unwrap_err();
    assert!(error.to_string().contains("no bootloader image"), "{error}");
}
