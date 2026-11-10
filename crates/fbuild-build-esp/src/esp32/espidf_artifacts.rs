//! Artifact discovery and publication for a delegated ESP-IDF build
//! (FastLED/fbuild#1971).
//!
//! `idf.py` owns its own output layout and is free to change it; fbuild must
//! not hard-code a second copy of that knowledge. Everything here is derived
//! from the two files the IDF build itself writes at the end of a successful
//! build:
//!
//! - `flasher_args.json` names the app image, the bootloader and the partition
//!   table by the *relative* paths the build actually used, so a project that
//!   renames its binary or nests the bootloader is still discovered.
//! - `project_description.json` is not needed: the app image's stem already is
//!   the project name (IDF names the image after `project()`).
//!
//! The eight artifacts #1971 names are then expected at a stable flat location
//! under `<build>/idf/`, so follow-ups (#1972 coverage, #1973 compile-db
//! re-rooting, #1974 caching) and the flasher can address them by name instead
//! of re-reading IDF's tree. `idf-artifacts.json` records what was published,
//! with byte counts, which is the artifact proof those follow-ups consume.

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};
use serde::Deserialize;

/// Record of the published artifacts, relative to the delegated build root.
pub(super) const INDEX_FILE: &str = "idf-artifacts.json";

/// The seven artifacts a delegated build must produce (FastLED/fbuild#1971).
#[derive(Debug, Clone)]
pub(crate) struct IdfArtifacts {
    /// Project name as IDF named the image (`<name>.bin`).
    pub project: String,
    pub app_bin: NormalizedPath,
    pub elf: NormalizedPath,
    pub map: NormalizedPath,
    pub bootloader_bin: NormalizedPath,
    pub partition_table_bin: NormalizedPath,
    pub sdkconfig: NormalizedPath,
    pub compile_commands: NormalizedPath,
}

impl IdfArtifacts {
    /// Every artifact, by the name #1971 states, in a stable order.
    pub(super) fn files(&self) -> [(&'static str, &NormalizedPath); 7] {
        [
            ("app.bin", &self.app_bin),
            ("app.elf", &self.elf),
            ("app.map", &self.map),
            ("bootloader.bin", &self.bootloader_bin),
            ("partition-table.bin", &self.partition_table_bin),
            ("sdkconfig", &self.sdkconfig),
            ("compile_commands.json", &self.compile_commands),
        ]
    }

    /// Total bytes of the published artifacts (not of IDF's intermediate tree).
    pub(super) fn total_bytes(&self) -> u64 {
        self.files()
            .iter()
            .filter_map(|(_, path)| std::fs::metadata(path.as_path()).ok())
            .map(|metadata| metadata.len())
            .sum()
    }

    /// Write [`INDEX_FILE`] beside the artifacts.
    pub(super) fn write_index(&self, chip: &str, root: &NormalizedPath) -> Result<NormalizedPath> {
        let files = self
            .files()
            .iter()
            .map(|(name, path)| {
                let bytes = std::fs::metadata(path.as_path())
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                FileEntry {
                    name,
                    path: path.display_slash(),
                    bytes,
                }
            })
            .collect();
        let index = Index {
            project: &self.project,
            chip,
            root: root.display_slash(),
            files,
        };
        let path = root.join(INDEX_FILE);
        let json = serde_json::to_vec_pretty(&index)
            .map_err(|error| FbuildError::Other(format!("idf artifacts index: {error}")))?;
        fbuild_core::fs::write_atomic_sync(path.as_path(), json)
            .map_err(|error| FbuildError::Other(format!("writing {path}: {error}")))?;
        Ok(path)
    }
}

/// One published artifact, as recorded in the index.
#[derive(Debug, serde::Serialize)]
struct FileEntry {
    name: &'static str,
    path: String,
    bytes: u64,
}

#[derive(Debug, serde::Serialize)]
struct Index<'a> {
    project: &'a str,
    chip: &'a str,
    root: String,
    files: Vec<FileEntry>,
}

/// The subset of `flasher_args.json` this module reads.
#[derive(Debug, Deserialize)]
struct FlasherArgs {
    #[serde(default)]
    app: Option<FlashRegion>,
    #[serde(default)]
    bootloader: Option<FlashRegion>,
    #[serde(default, rename = "partition-table")]
    partition_table: Option<FlashRegion>,
}

#[derive(Debug, Deserialize)]
struct FlashRegion {
    file: String,
}

/// Discover, validate and publish a completed build's artifacts.
///
/// `root` is the delegated build directory (the one passed to `idf.py -B`) and
/// `project_dir` the project it was configured from. Everything IDF writes in
/// place stays there; the artifacts IDF keeps *outside* the build directory
/// (today: the generated `sdkconfig`, which lives in the project root and is
/// what a user edits and commits) and the two nested ones (bootloader,
/// partition table) are published flat so every name #1971 lists lives in one
/// directory.
pub(super) fn discover_and_publish(
    root: &NormalizedPath,
    project_dir: &NormalizedPath,
) -> Result<IdfArtifacts> {
    let args_path = root.join("flasher_args.json");
    let raw = std::fs::read(args_path.as_path()).map_err(|error| {
        FbuildError::Other(format!(
            "the delegated ESP-IDF build finished but {args_path} is unreadable ({error}); \
             the build directory is not an ESP-IDF application build"
        ))
    })?;
    let args: FlasherArgs = serde_json::from_slice(&raw).map_err(|error| {
        FbuildError::Other(format!(
            "{args_path} is not valid flasher arguments: {error}"
        ))
    })?;

    let app = region_file(&args.app, "app", &args_path)?;
    let bootloader = region_file(&args.bootloader, "bootloader", &args_path)?;
    let partition_table = region_file(&args.partition_table, "partition-table", &args_path)?;

    let app_bin = required_file(root, &app, "app image")?;
    let project = app_bin
        .as_path()
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .ok_or_else(|| FbuildError::Other(format!("the app image {app_bin} has no file stem")))?;

    let nested_bootloader = required_file(root, &bootloader, "bootloader image")?;
    let nested_partition_table = required_file(root, &partition_table, "partition table")?;

    Ok(IdfArtifacts {
        app_bin: app_bin.clone(),
        elf: required_sibling(&app_bin, "elf", "app ELF")?,
        map: required_sibling(&app_bin, "map", "app map")?,
        bootloader_bin: publish_flat(&nested_bootloader, &root.join("bootloader.bin"))?,
        partition_table_bin: publish_flat(
            &nested_partition_table,
            &root.join("partition-table.bin"),
        )?,
        sdkconfig: publish_flat(
            &generated_sdkconfig(root, project_dir)?,
            &root.join("sdkconfig"),
        )?,
        compile_commands: required_file(root, "compile_commands.json", "compile database")?,
        project,
    })
}

/// The generated `sdkconfig`.
///
/// `idf.py` writes it in the **project** directory (`<project>/sdkconfig`),
/// next to `sdkconfig.defaults`, not in the build directory: it is the resolved
/// configuration the user edits and commits. Older trees also carry a copy in
/// the build directory, so that is accepted as a fallback; both are named in
/// the error when neither exists.
fn generated_sdkconfig(
    root: &NormalizedPath,
    project_dir: &NormalizedPath,
) -> Result<NormalizedPath> {
    let in_project = project_dir.join("sdkconfig");
    if in_project.as_path().is_file() {
        return Ok(in_project);
    }
    let in_build = root.join("sdkconfig");
    if in_build.as_path().is_file() {
        return Ok(in_build);
    }
    Err(FbuildError::Other(format!(
        "the delegated ESP-IDF build did not produce the generated sdkconfig: \
         neither {in_project} nor {in_build} exists"
    )))
}

/// The `file` of one flasher-args region, or an error naming the region.
fn region_file(
    region: &Option<FlashRegion>,
    name: &str,
    args_path: &NormalizedPath,
) -> Result<String> {
    region
        .as_ref()
        .map(|region| region.file.clone())
        .ok_or_else(|| {
            FbuildError::Other(format!(
                "{args_path} declares no {name} image; the delegated build produced an \
                 application layout fbuild cannot publish"
            ))
        })
}

/// Resolve a path IDF recorded relative to the build root and require it.
fn required_file(root: &NormalizedPath, relative: &str, label: &str) -> Result<NormalizedPath> {
    let path = root.join(relative);
    if path.as_path().is_file() {
        return Ok(path);
    }
    Err(FbuildError::Other(format!(
        "the delegated ESP-IDF build did not produce the {label}: {path} is missing"
    )))
}

/// The app's sibling with another extension (`<name>.elf`, `<name>.map`).
fn required_sibling(
    app_bin: &NormalizedPath,
    extension: &str,
    label: &str,
) -> Result<NormalizedPath> {
    let path = app_bin.as_path().with_extension(extension);
    let path = NormalizedPath::new(&path);
    if path.as_path().is_file() {
        return Ok(path);
    }
    Err(FbuildError::Other(format!(
        "the delegated ESP-IDF build did not produce the {label}: {path} is missing"
    )))
}

/// Copy a nested artifact to its flat name, unless it already is that file.
fn publish_flat(source: &NormalizedPath, target: &NormalizedPath) -> Result<NormalizedPath> {
    if source.key() == target.key() {
        return Ok(target.clone());
    }
    std::fs::copy(source.as_path(), target.as_path()).map_err(|error| {
        FbuildError::Other(format!("publishing {source} as {target} failed: {error}"))
    })?;
    Ok(target.clone())
}

#[cfg(test)]
#[path = "espidf_artifacts_tests.rs"]
mod espidf_artifacts_tests;
