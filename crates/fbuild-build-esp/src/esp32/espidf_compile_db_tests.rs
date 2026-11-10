//! Publication tests for the delegated compile database (FastLED/fbuild#1973).

use fbuild_core::path::NormalizedPath;

use super::*;

/// A project, its fbuild build directory and the delegated root under it,
/// laid out the way a real run leaves them.
struct Layout {
    dir: tempfile::TempDir,
}

impl Layout {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let layout = Self { dir };
        std::fs::create_dir_all(layout.build_dir().as_path()).unwrap();
        std::fs::create_dir_all(layout.delegated_root().as_path()).unwrap();
        layout
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn project(&self) -> NormalizedPath {
        NormalizedPath::new(self.path()).join("proj")
    }

    fn build_dir(&self) -> NormalizedPath {
        self.project().join("wb").join("esp32s3")
    }

    fn delegated_root(&self) -> NormalizedPath {
        self.build_dir().join("idf")
    }

    fn idf_root(&self) -> NormalizedPath {
        NormalizedPath::new(self.path()).join("idf-release")
    }

    /// The delegated database, in the shape `idf.py` writes it: one scratch
    /// `directory`, project sources beside it, framework sources in the IDF
    /// checkout, and a generated source inside the build root.
    fn write_delegated_database(&self) {
        let project = self.project().display_slash();
        let build = self.delegated_root().display_slash();
        let idf = self.idf_root().display_slash();
        let entries = serde_json::json!([
            entry(
                &format!("{build}/esp-idf/main/CMakeFiles/__idf_main.dir/main.c.obj"),
                &format!("{project}/main/main.c"),
                &build,
            ),
            entry(
                &format!("{build}/esp-idf/freertos/CMakeFiles/__idf_freertos.dir/tasks.c.obj"),
                &format!("{idf}/components/freertos/tasks.c"),
                &build,
            ),
            entry(
                &format!("{build}/CMakeFiles/app.dir/project_elf_src.c.obj"),
                &format!("{build}/project_elf_src.c"),
                &build,
            ),
        ]);
        self.write(
            &self.delegated_root().join("compile_commands.json"),
            &entries.to_string(),
        );
    }

    fn write(&self, path: &NormalizedPath, text: &str) {
        if let Some(parent) = path.as_path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path.as_path(), text).unwrap();
    }

    fn publish(&self) -> PublishedDatabase {
        publish(&self.delegated_root(), &self.build_dir(), &self.project()).unwrap()
    }

    fn merged(&self, path: &NormalizedPath) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path.as_path()).unwrap()).unwrap()
    }
}

fn entry(output: &str, file: &str, directory: &str) -> serde_json::Value {
    serde_json::json!({
        "command": format!("/tools/gcc -I{directory}/config -o {output} -c {file}"),
        "directory": directory,
        "file": file,
        "output": output,
    })
}

/// fbuild's own database entry, in the spelling its generator writes.
fn native(file: &str, output: &str) -> serde_json::Value {
    serde_json::json!({
        "arguments": ["/tools/g++", "-std=gnu++17", "-c", file, "-o", output],
        "directory": "/elsewhere",
        "file": file,
        "output": output,
    })
}

/// The database lands where fbuild documents it, re-rooted onto the project.
#[test]
fn publishes_at_the_documented_project_path() {
    let layout = Layout::new();
    layout.write_delegated_database();

    let published = layout.publish();

    assert_eq!(
        published.path,
        layout.project().join("compile_commands.json")
    );
    assert_eq!(published.entries, 3);
    assert_eq!(published.from_fbuild, 0);
    assert_eq!(published.from_delegated, 3);
    assert_eq!(published.digest.len(), 64);

    let merged = layout.merged(&published.path);
    let project = layout.project().display_slash();
    for entry in merged.as_array().unwrap() {
        assert_eq!(entry["directory"], project);
        let file = entry["file"].as_str().unwrap();
        assert!(!file.starts_with('/'), "still absolute: {file}");
        assert!(
            !file.contains(&layout.delegated_root().display_slash()),
            "delegated build prefix survived: {file}"
        );
    }
    assert!(
        merged
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["file"] == "main/main.c"),
        "the sketch is spelled relative to the project: {merged}"
    );
}

/// fbuild's own entries and the delegated ones end up in one database, one
/// entry per translation unit, and a second identical run is byte-identical.
#[test]
fn merges_fbuilds_database_and_is_reproducible() {
    let layout = Layout::new();
    layout.write_delegated_database();
    let project = layout.project().display_slash();
    layout.write(
        &layout.build_dir().join("compile_commands.json"),
        &serde_json::json!([
            native(
                &format!("{project}/main/main.c"),
                &format!("{}/obj/main.o", layout.build_dir().display_slash())
            ),
            native(
                &format!("{project}/lib/util.cpp"),
                &format!("{}/obj/util.o", layout.build_dir().display_slash())
            ),
        ])
        .to_string(),
    );

    let first = layout.publish();
    let bytes = std::fs::read(first.path.as_path()).unwrap();

    assert_eq!(first.from_fbuild, 2);
    assert_eq!(first.from_delegated, 2, "framework + generated are new");
    assert_eq!(first.entries, 4);

    let merged = layout.merged(&first.path);
    let sketch = merged
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["file"] == format!("{project}/main/main.c").as_str())
        .expect("the sketch entry survives");
    assert!(
        sketch["arguments"].is_array(),
        "fbuild's spelling wins the overlap: {sketch}"
    );

    let second = layout.publish();
    assert_eq!(first.digest, second.digest);
    assert_eq!(bytes, std::fs::read(second.path.as_path()).unwrap());
}

/// A library project publishes at the build directory, which is also where
/// fbuild's own database lives: the stale duplicate of this build's own entry
/// is dropped, fbuild's unrelated entries stay.
#[test]
fn library_project_reclaims_its_own_previous_output() {
    let layout = Layout::new();
    layout.write_delegated_database();
    layout.write(&layout.project().join("library.json"), "{}");
    let project = layout.project().display_slash();
    let build = layout.build_dir().display_slash();
    layout.write(
        &layout.build_dir().join("compile_commands.json"),
        &serde_json::json!([
            // Last run's re-rooted delegated entry for the sketch: stale.
            native(
                &format!("{project}/main/main.c"),
                &format!("{build}/old/main.o")
            ),
            native(
                &format!("{project}/lib/util.cpp"),
                &format!("{build}/obj/util.o")
            ),
        ])
        .to_string(),
    );

    let published = layout.publish();

    assert_eq!(
        published.path,
        layout.build_dir().join("compile_commands.json")
    );
    assert_eq!(published.from_fbuild, 1, "only the unrelated entry stays");
    assert_eq!(published.entries, 4);
    let merged = layout.merged(&published.path);
    let sketches = merged
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| {
            entry["file"]
                .as_str()
                .is_some_and(|file| file.ends_with("main/main.c"))
        })
        .count();
    assert_eq!(sketches, 1, "one entry per translation unit");
}

/// A delegated build that produced no entries cannot be merged into anything.
#[test]
fn empty_delegated_database_is_an_error() {
    let layout = Layout::new();
    layout.write(&layout.delegated_root().join("compile_commands.json"), "[]");

    let error = publish(
        &layout.delegated_root(),
        &layout.build_dir(),
        &layout.project(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("compile_commands.json"), "{error}");
}
