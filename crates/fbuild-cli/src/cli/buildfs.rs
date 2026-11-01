//! `fbuild buildfs` — assemble a flash filesystem image from a project's `data/`.
//!
//! PlatformIO's `buildfs` target has no fbuild equivalent (FastLED/fbuild#1358):
//! there was no path from a sketch's `data/` directory to an on-device
//! filesystem. This command builds that image:
//!
//! * inputs are the files under `data/` (or `data_dir` from `platformio.ini`),
//!   with fbuild blob pointers (`.fetch`/`.lnk`) resolved into the image under
//!   their projected name and the pointer sidecar never shipped. Plain-text
//!   `.lnk` files that are not fbuild manifests (FastLED's on-device asset
//!   links) are ordinary data and do ship;
//! * the filesystem partition (offset + size) is read from the board's
//!   partition CSV, so the image is sized for the region the firmware mounts;
//! * a size check runs *before* the image tool, so an oversized `data/` fails
//!   the build instead of failing to mount at runtime;
//! * `mklittlefs` / `mkspiffs` produce `<build dir>/<filesystem>.bin`.
//!
//! Flashing the image (`uploadfs`) is the deploy half of #1358 and is not part
//! of this command.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use clap::Args;
use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use crate::cli::buildfs_esp8266;
use crate::cli::buildfs_geometry::FsGeometry;
use crate::cli::buildfs_rp2040;
use crate::output;

/// Which flash filesystem the image is formatted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsKind {
    LittleFs,
    Spiffs,
}

impl FsKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LittleFs => "littlefs",
            Self::Spiffs => "spiffs",
        }
    }

    /// Image file name inside the build directory (PlatformIO's convention).
    pub(crate) fn image_name(self) -> &'static str {
        match self {
            Self::LittleFs => "littlefs.bin",
            Self::Spiffs => "spiffs.bin",
        }
    }

    /// PlatformIO package that ships the image tool.
    pub(crate) fn tool_package(self) -> &'static str {
        match self {
            Self::LittleFs => "tool-mklittlefs",
            Self::Spiffs => "tool-mkspiffs",
        }
    }

    /// Image tool binary name, with the host executable suffix.
    pub(crate) fn tool_binary(self) -> String {
        let stem = match self {
            Self::LittleFs => "mklittlefs",
            Self::Spiffs => "mkspiffs",
        };
        format!("{stem}{}", std::env::consts::EXE_SUFFIX)
    }

    /// Parse a `board_build.filesystem` / `--filesystem` value.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "littlefs" | "lfs" => Some(Self::LittleFs),
            "spiffs" => Some(Self::Spiffs),
            _ => None,
        }
    }
}

/// Map a partition-table subtype (`spiffs`, `littlefs`, or the numeric ESP-IDF
/// encodings `0x82`/`0x83`) onto a filesystem kind.
pub(crate) fn fs_kind_for_subtype(subtype: &str) -> Option<FsKind> {
    match subtype.trim().to_ascii_lowercase().as_str() {
        "spiffs" | "0x82" => Some(FsKind::Spiffs),
        "littlefs" | "0x83" => Some(FsKind::LittleFs),
        _ => None,
    }
}

/// One row of an ESP32 partition CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Partition {
    pub label: String,
    pub kind: String,
    pub subtype: String,
    pub offset: u64,
    pub size: u64,
}

/// `gen_esp32part.py` size syntax: decimal, `0x` hex, or a `K`/`M` suffix.
pub(crate) fn parse_partition_size(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok();
    }
    let (digits, scale) = match value.char_indices().last()? {
        (i, 'k' | 'K') => (&value[..i], 1024),
        (i, 'm' | 'M') => (&value[..i], 1024 * 1024),
        _ => (value, 1),
    };
    digits.trim().parse::<u64>().ok().map(|n| n * scale)
}

/// Parse a partition CSV. Comment, blank and short rows are skipped; a row that
/// carries an unparseable offset or size is an error, because silently
/// dropping it could size the image from the wrong partition.
pub(crate) fn parse_partition_table(csv: &str) -> Result<Vec<Partition>> {
    let mut out = Vec::new();
    for (index, raw) in csv.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 5 {
            continue;
        }
        let offset = parse_partition_size(cols[3]).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "partition table line {}: cannot parse offset {:?}",
                index + 1,
                cols[3]
            ))
        })?;
        let size = parse_partition_size(cols[4]).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "partition table line {}: cannot parse size {:?}",
                index + 1,
                cols[4]
            ))
        })?;
        out.push(Partition {
            label: cols[0].trim_matches('"').to_string(),
            kind: cols[1].to_ascii_lowercase(),
            subtype: cols[2].to_ascii_lowercase(),
            offset,
            size,
        });
    }
    Ok(out)
}

/// Pick the filesystem partition: an explicit label wins, otherwise the first
/// data partition whose subtype names a filesystem we can build.
pub(crate) fn select_fs_partition(parts: &[Partition], label: Option<&str>) -> Result<Partition> {
    if let Some(label) = label {
        return parts
            .iter()
            .find(|p| p.label == label)
            .cloned()
            .ok_or_else(|| {
                FbuildError::ConfigError(format!(
                    "no partition labelled {label:?} in the partition table; available: {}",
                    summarize(parts)
                ))
            });
    }
    parts
        .iter()
        .find(|p| p.kind == "data" && p.size > 0 && fs_kind_for_subtype(&p.subtype).is_some())
        .cloned()
        .ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "the partition table has no data partition with a spiffs/littlefs subtype; \
                 available: {}. Add one (e.g. `spiffs, data, spiffs, 0x290000, 0x160000`) or pass \
                 --fs-partition",
                summarize(parts)
            ))
        })
}

fn summarize(parts: &[Partition]) -> String {
    if parts.is_empty() {
        return "none".to_string();
    }
    parts
        .iter()
        .map(|p| format!("{} ({}/{}, {} bytes)", p.label, p.kind, p.subtype, p.size))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A file that will be packed, relative to the image root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct StagedFile {
    pub relative: String,
    pub bytes: u64,
}

/// Fail when the staged payload cannot fit the filesystem partition.
pub(crate) fn check_capacity(required: u64, partition: &Partition) -> Result<()> {
    if required <= partition.size {
        return Ok(());
    }
    Err(FbuildError::BuildFailed(format!(
        "data/ does not fit the '{}' filesystem partition: needs ~{} bytes, partition holds {} \
         (over by {} bytes). Use a partition table with a larger filesystem region, or shrink data/.",
        partition.label,
        required,
        partition.size,
        required - partition.size
    )))
}

/// `fbuild buildfs` arguments.
#[derive(Args, Debug)]
pub struct BuildfsArgs {
    /// Project directory (defaults to the enclosing project).
    pub project_dir: Option<String>,
    /// Target environment.
    #[arg(short = 'e', long)]
    pub environment: Option<String>,
    /// Directory to pack (defaults to `data_dir` from platformio.ini, else `data/`).
    #[arg(long = "data-dir")]
    pub data_dir: Option<String>,
    /// Partition table CSV (defaults to `board_build.partitions`, else the
    /// framework's `default.csv`).
    #[arg(long)]
    pub partitions: Option<String>,
    /// Filesystem partition label (defaults to the first data partition with a
    /// spiffs/littlefs subtype).
    #[arg(long = "fs-partition")]
    pub fs_partition: Option<String>,
    /// Image format: `littlefs` or `spiffs` (defaults to the partition subtype).
    #[arg(long)]
    pub filesystem: Option<String>,
    /// Path to `mklittlefs`/`mkspiffs` (defaults to the PlatformIO package, then PATH).
    #[arg(long)]
    pub tool: Option<String>,
    /// Output image path (defaults to `<build dir>/<filesystem>.bin`).
    #[arg(short = 'o', long)]
    pub output: Option<String>,
    /// Resolve, stage and size-check only; do not run the image tool.
    #[arg(long = "check-only")]
    pub check_only: bool,
    /// Emit a machine-readable JSON summary.
    #[arg(long)]
    pub json: bool,
}

pub(crate) struct ProjectContext {
    pub(crate) project_dir: NormalizedPath,
    pub(crate) env: String,
    pub(crate) data_dir: NormalizedPath,
    /// ESP32 partition CSV. `None` on a platform that does not declare one
    /// (RP2040 sizes its filesystem from the arduino-pico `flash` menu).
    pub(crate) partitions_csv: Option<NormalizedPath>,
    /// Platform of the target environment (`platform = ...`).
    pub(crate) platform: Option<fbuild_core::Platform>,
    /// `board_build.filesystem`, when the environment declares one.
    pub(crate) configured_fs: Option<String>,
    /// The environment's full `[env:...]` map. `uploadfs` needs `platform` and
    /// `board` from it, and the esptool resolver needs the whole thing.
    pub(crate) env_config: HashMap<String, String>,
    /// `board_build.*` / `board_upload.*` overrides — `board`, `partitions`,
    /// flash parameters, and the board-config lookup `uploadfs` builds its
    /// deployer from.
    pub(crate) board_overrides: HashMap<String, String>,
}

/// Resolve the project's environment, data directory and partition table.
fn resolve_context(args: &BuildfsArgs, project_dir: &Path) -> Result<ProjectContext> {
    let ini_path = project_dir.join("platformio.ini");
    let ini = if ini_path.is_file() {
        Some(fbuild_config::PlatformIOConfig::from_path(&ini_path)?)
    } else {
        None
    };

    let env = match &args.environment {
        Some(env) => env.clone(),
        None => ini
            .as_ref()
            .and_then(|c| c.get_default_environment().map(str::to_string))
            .unwrap_or_else(|| "default".to_string()),
    };

    let mut board_overrides = HashMap::new();
    let mut env_config = HashMap::new();
    let mut data_dir_name = None;
    if let Some(config) = ini.as_ref() {
        if !config.has_environment(&env) {
            return Err(FbuildError::ConfigError(format!(
                "environment '{env}' not found in {}; available: {}",
                ini_path.display(),
                config.get_environments().join(", ")
            )));
        }
        board_overrides = config.get_board_overrides(&env)?;
        env_config = config.get_env_config(&env)?.clone();
        data_dir_name = env_config
            .get("data_dir")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }

    let platform = env_config
        .get("platform")
        .and_then(|value| fbuild_core::Platform::from_platform_str(value));

    let data_dir = match &args.data_dir {
        Some(dir) => absolute(project_dir, dir),
        None => absolute(project_dir, data_dir_name.as_deref().unwrap_or("data")),
    };

    // RP2040 and ESP8266 projects have no partition CSV: arduino-pico's `flash`
    // menu and esp8266's `eagle.flash.*.ld` own the filesystem bounds, and
    // `plan_image` reads them from the framework instead.
    let partitions_csv = if matches!(
        platform,
        Some(fbuild_core::Platform::RaspberryPi | fbuild_core::Platform::Espressif8266)
    ) {
        None
    } else {
        Some(match &args.partitions {
            Some(path) => {
                let path = absolute(project_dir, path);
                if !path.is_file() {
                    return Err(FbuildError::ConfigError(format!(
                        "--partitions {} does not exist",
                        path.as_path().display()
                    )));
                }
                path
            }
            None => resolve_partitions_csv(
                project_dir,
                board_overrides.get("partitions").map(String::as_str),
            )?,
        })
    };

    Ok(ProjectContext {
        project_dir: NormalizedPath::new(project_dir),
        env,
        data_dir,
        partitions_csv,
        platform,
        configured_fs: board_overrides.get("filesystem").cloned(),
        env_config,
        board_overrides,
    })
}

/// The `board` an environment targets, from `board_build.board` or the
/// environment's own `board` key.
pub(crate) fn board_id(context: &ProjectContext) -> Option<String> {
    context
        .board_overrides
        .get("board")
        .or_else(|| context.env_config.get("board"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn absolute(project_dir: &Path, value: &str) -> NormalizedPath {
    let path = Path::new(value);
    if path.is_absolute() {
        NormalizedPath::new(path)
    } else {
        NormalizedPath::new(project_dir.join(path))
    }
}

/// PlatformIO semantics: a project-relative path first, then the framework's
/// `tools/partitions/` directory (which serves the built-in `default.csv`,
/// `huge_app.csv`, ...). An explicit name that resolves nowhere is an error.
fn resolve_partitions_csv(project_dir: &Path, configured: Option<&str>) -> Result<NormalizedPath> {
    let name = configured.unwrap_or("default.csv");
    let mut candidates = vec![
        NormalizedPath::new(project_dir.join(name)),
        NormalizedPath::new(project_dir.join("partitions").join(name)),
    ];
    for root in framework_roots() {
        candidates.push(root.join("tools").join("partitions").join(name));
    }
    let mut tried = Vec::new();
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.as_path().display().to_string());
    }
    Err(FbuildError::ConfigError(format!(
        "partition table '{name}' not found; checked {}. Pass --partitions <csv>.",
        tried.join(", ")
    )))
}

/// Candidate framework install roots worth checking for a partition CSV: the
/// PlatformIO package directory and fbuild's own extracted platform cache.
fn framework_roots() -> Vec<NormalizedPath> {
    let mut roots = vec![NormalizedPath::new(fbuild_paths::get_platformio_package(
        "framework-arduinoespressif32",
    ))];
    let platform_cache = fbuild_paths::get_cache_root().join("platforms");
    if let Ok(entries) = std::fs::read_dir(&platform_cache) {
        let mut found: Vec<NormalizedPath> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| NormalizedPath::new(entry.path()))
            .filter(|path| {
                path.as_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("framework-arduinoespressif32"))
            })
            .collect();
        found.sort();
        roots.extend(found);
    }
    roots
}

/// Copy `data/` into a fresh staging tree, resolving fbuild blob pointers to
/// their blobs (at the projected name) and never shipping the pointer itself.
fn stage_data(data_dir: &NormalizedPath, staging: &NormalizedPath) -> Result<()> {
    if !data_dir.as_path().is_dir() {
        return Err(FbuildError::ConfigError(format!(
            "data directory {} does not exist; create it or pass --data-dir",
            data_dir.as_path().display()
        )));
    }
    if staging.as_path().exists() {
        std::fs::remove_dir_all(staging.as_path()).map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to clear {}: {e}",
                staging.as_path().display()
            ))
        })?;
    }
    std::fs::create_dir_all(staging.as_path()).map_err(|e| {
        FbuildError::BuildFailed(format!(
            "failed to create {}: {e}",
            staging.as_path().display()
        ))
    })?;

    let entries = collect_entries(data_dir)?;
    if entries.is_empty() {
        output::warn(format!(
            "{} is empty; the image will be empty",
            data_dir.as_path().display()
        ));
    }

    // Blob leases must outlive the pack so hardlinked children stay valid.
    let mut leases = Vec::new();
    let mut cache = None;
    for (relative, source) in entries {
        let target = staging.join(&relative);
        if let Some(parent) = target.as_path().parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                FbuildError::BuildFailed(format!("failed to create {}: {e}", parent.display()))
            })?;
        }
        if let Some(lnk) = classify_pointer(source.as_path())? {
            if cache.is_none() {
                cache = Some(fbuild_packages::DiskCache::open().map_err(|e| {
                    FbuildError::PackageError(format!("failed to open lnk disk cache: {e}"))
                })?);
            }
            let Some(cache) = cache.as_ref() else {
                unreachable!("cache was just opened");
            };
            let projected = strip_pointer_suffix(&relative);
            leases.push(fbuild_packages::lnk::materialize_one(
                &source,
                &lnk,
                &staging.join(projected),
                cache,
            )?);
            continue;
        }
        std::fs::copy(source.as_path(), target.as_path()).map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to stage {} -> {}: {e}",
                source.as_path().display(),
                target.as_path().display()
            ))
        })?;
    }
    drop(leases);
    Ok(())
}

/// Decide what a `.lnk`/`.fetch` file is.
///
/// * `Ok(None)` — ordinary data that ships into the image. This covers
///   FastLED's on-device asset links, which are plain text read by
///   `fl::parse_lnk`, and any non-pointer file.
/// * `Ok(Some(lnk))` — an fbuild blob pointer: resolve it and ship the blob
///   under the projected name, never the pointer itself.
/// * `Err(..)` — a pointer we cannot parse. `.fetch` files are unambiguously
///   fbuild's, as is any `.lnk` starting with `{` (the JSON manifest), so a
///   broken one is a build error: shipping it would hand the device a pointer
///   where it expects content.
fn classify_pointer(path: &Path) -> Result<Option<fbuild_packages::LnkFile>> {
    if !fbuild_packages::lnk::is_blob_pointer(path) {
        return Ok(None);
    }
    let is_fetch = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fetch"));
    if !is_fetch && !starts_with_json_object(path)? {
        // FastLED's runtime asset link: plain data, not an fbuild manifest.
        return Ok(None);
    }
    match fbuild_packages::LnkFile::from_path(path) {
        Ok(lnk) => Ok(Some(lnk)),
        Err(err) => Err(FbuildError::BuildFailed(format!(
            "{} is a blob pointer that does not parse: {err}. Fix it (or run `fbuild lnk check`) \
             — it must never ship to the device as data.",
            path.display()
        ))),
    }
}

fn starts_with_json_object(path: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| FbuildError::BuildFailed(format!("failed to read {}: {e}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| line.starts_with('{')))
}

fn strip_pointer_suffix(relative: &str) -> String {
    for suffix in [".lnk", ".fetch"] {
        if let Some(stripped) = relative.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    relative.to_string()
}

/// Recursively list files as `(relative slash path, absolute source)`.
fn collect_entries(root: &NormalizedPath) -> Result<Vec<(String, NormalizedPath)>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root.as_path())
        .follow_links(true)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = NormalizedPath::new(entry.path());
        let Some(relative) = path.relative_to(root) else {
            return Err(FbuildError::BuildFailed(format!(
                "{} is not under {}",
                path.as_path().display(),
                root.as_path().display()
            )));
        };
        out.push((relative.display_slash(), path));
    }
    out.sort();
    Ok(out)
}

/// Measure the staged tree; this is what actually gets packed.
fn measure_staging(staging: &NormalizedPath) -> Result<Vec<StagedFile>> {
    let mut files = Vec::new();
    for (relative, source) in collect_entries(staging)? {
        let bytes = std::fs::metadata(source.as_path())
            .map_err(|e| {
                FbuildError::BuildFailed(format!(
                    "failed to stat {}: {e}",
                    source.as_path().display()
                ))
            })?
            .len();
        files.push(StagedFile { relative, bytes });
    }
    Ok(files)
}

/// Locate the image tool: explicit path, then the PlatformIO package dir, then
/// PATH.
fn resolve_tool(kind: FsKind, explicit: Option<&Path>) -> Result<NormalizedPath> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(NormalizedPath::new(path));
        }
        return Err(FbuildError::ConfigError(format!(
            "--tool {} is not a file",
            path.display()
        )));
    }
    let binary = kind.tool_binary();
    let mut candidates = vec![
        NormalizedPath::new(
            fbuild_paths::get_platformio_package(kind.tool_package()).join(&binary),
        ),
        NormalizedPath::new(
            fbuild_paths::get_cache_root()
                .join("toolchains")
                .join(kind.tool_package())
                .join(&binary),
        ),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&path).map(|dir| NormalizedPath::new(dir.join(&binary))));
    }
    for candidate in &candidates {
        if candidate.as_path().is_file() {
            return Ok(candidate.clone());
        }
    }
    Err(FbuildError::PackageError(format!(
        "{binary} not found ({}). Install PlatformIO package {} or pass --tool.",
        kind.as_str(),
        kind.tool_package()
    )))
}

/// Everything `fbuild buildfs` and `fbuild uploadfs` must agree on: the
/// partition the image belongs in, the format, and where the image lands.
///
/// `uploadfs` flashes the file recorded here at the offset recorded here, so
/// the two commands can never disagree about which region a `data/` tree ends
/// up in (FastLED/fbuild#1358).
pub(crate) struct ImagePlan {
    pub(crate) context: ProjectContext,
    pub(crate) partition: Partition,
    pub(crate) kind: FsKind,
    /// Block/page geometry the image must be formatted with.
    pub(crate) geometry: FsGeometry,
    pub(crate) build_dir: NormalizedPath,
    pub(crate) image: NormalizedPath,
}

/// ESP8266's plan: the `eesz` menu's `eagle.flash.*.ld` owns the region *and*
/// the block/page geometry, so both come from one file.
fn esp8266_plan(
    context: &ProjectContext,
    args: &BuildfsArgs,
) -> Result<(Partition, FsKind, FsGeometry)> {
    let board_id = board_id(context).ok_or_else(|| {
        FbuildError::ConfigError(format!(
            "environment '{}' declares no `board`; `fbuild buildfs` needs one to find the \
             esp8266 flash menu option that reserves the filesystem",
            context.env
        ))
    })?;
    let kind = buildfs_esp8266::resolve_kind(
        args.filesystem.as_deref(),
        context.configured_fs.as_deref(),
    )?;
    let region = buildfs_esp8266::resolve_region(&board_id, &context.board_overrides)?;
    Ok((
        Partition {
            label: "filesystem".to_string(),
            kind: "data".to_string(),
            subtype: kind.as_str().to_string(),
            offset: region.offset,
            size: region.size,
        },
        kind,
        FsGeometry::from_parts(region.block, region.page),
    ))
}

/// Resolve the environment, data directory, partition CSV, target partition,
/// image format and output path — the read-only half of `buildfs`.
pub(crate) fn plan_image(args: &BuildfsArgs, project_dir: &Path) -> Result<ImagePlan> {
    let context = resolve_context(args, project_dir)?;
    let (partition, kind, geometry) = match context.platform {
        // arduino-pico declares the filesystem bounds outright, per `flash`
        // menu option; there is no partition table to read.
        Some(fbuild_core::Platform::RaspberryPi) => {
            let board_id = board_id(&context).ok_or_else(|| {
                FbuildError::ConfigError(format!(
                    "environment '{}' declares no `board`; `fbuild buildfs` needs one to find \
                         the arduino-pico flash menu option that reserves the filesystem",
                    context.env
                ))
            })?;
            let kind = buildfs_rp2040::resolve_kind(
                args.filesystem.as_deref(),
                context.configured_fs.as_deref(),
            )?;
            let region = buildfs_rp2040::resolve_region(&board_id, &context.board_overrides)?;
            (
                Partition {
                    label: "filesystem".to_string(),
                    kind: "data".to_string(),
                    subtype: kind.as_str().to_string(),
                    offset: region.offset,
                    size: region.size,
                },
                kind,
                FsGeometry::ESP32,
            )
        }
        Some(fbuild_core::Platform::Espressif8266) => esp8266_plan(&context, args)?,
        Some(fbuild_core::Platform::Espressif32) | None => {
            let partitions_csv = context.partitions_csv.as_ref().ok_or_else(|| {
                FbuildError::ConfigError(format!(
                    "environment '{}' resolved no partition table; pass --partitions <csv>",
                    context.env
                ))
            })?;
            let csv = std::fs::read_to_string(partitions_csv.as_path()).map_err(|e| {
                FbuildError::ConfigError(format!(
                    "failed to read {}: {e}",
                    partitions_csv.as_path().display()
                ))
            })?;
            let table = parse_partition_table(&csv)?;
            let partition = select_fs_partition(&table, args.fs_partition.as_deref())?;
            let kind = resolve_kind(
                args.filesystem.as_deref(),
                context.configured_fs.as_deref(),
                &partition,
            )?;
            (partition, kind, FsGeometry::ESP32)
        }
        Some(other) => {
            return Err(FbuildError::ConfigError(format!(
                "`fbuild buildfs` builds filesystem images for ESP32 (partition table), ESP8266 \
                 (eagle.flash linker script) and RP2040 (arduino-pico flash menu) only; \
                 environment '{}' targets {other:?}",
                context.env
            )));
        }
    };

    let build_dir = NormalizedPath::new(
        fbuild_paths::BuildLayout::new(
            context.project_dir.clone().into_path_buf(),
            context.env.clone(),
            fbuild_core::BuildProfile::Release,
        )
        .resolve(),
    );
    let image = match &args.output {
        Some(path) => absolute(context.project_dir.as_path(), path),
        None => build_dir.join(kind.image_name()),
    };

    Ok(ImagePlan {
        context,
        partition,
        kind,
        geometry,
        build_dir,
        image,
    })
}

/// Resolve, stage, size-check and build the filesystem image.
pub async fn run(args: BuildfsArgs, project_dir: &Path) -> Result<()> {
    let plan = plan_image(&args, project_dir)?;
    let context = &plan.context;
    let partition = &plan.partition;
    let kind = plan.kind;
    let staging = plan.build_dir.join("fs_stage");
    stage_data(&context.data_dir, &staging)?;
    let files = measure_staging(&staging)?;
    let required = plan.geometry.estimate_bytes(&files);
    check_capacity(required, partition)?;

    let tool = if args.check_only {
        None
    } else {
        Some(resolve_tool(kind, args.tool.as_deref().map(Path::new))?)
    };
    let image = &plan.image;

    if let Some(tool) = tool.as_ref() {
        if let Some(parent) = image.as_path().parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                FbuildError::BuildFailed(format!("failed to create {}: {e}", parent.display()))
            })?;
        }
        run_image_tool(tool, &staging, partition.size, image, plan.geometry).await?;
    }

    let image_bytes = if tool.is_some() {
        std::fs::metadata(image.as_path())
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    report(&Report {
        args: &args,
        context,
        partition,
        kind,
        files: &files,
        required,
        image,
        image_bytes,
        tool: tool.as_ref(),
    });
    Ok(())
}

/// Resolve the image format: explicit flag, `board_build.filesystem`, else the
/// partition subtype. A format that disagrees with the partition table is a
/// warning — the firmware mounts whatever the table declares.
fn resolve_kind(
    requested: Option<&str>,
    configured: Option<&str>,
    partition: &Partition,
) -> Result<FsKind> {
    let kind = match requested.or(configured) {
        Some(value) => FsKind::parse(value).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "unsupported filesystem {value:?}; this command builds littlefs or spiffs"
            ))
        })?,
        None => fs_kind_for_subtype(&partition.subtype).ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "cannot infer the filesystem from partition subtype {:?}; pass --filesystem",
                partition.subtype
            ))
        })?,
    };
    if fs_kind_for_subtype(&partition.subtype) != Some(kind) {
        output::warn(format!(
            "partition '{}' is declared {} but the image is {}; the device will not mount it \
             unless its partition table matches",
            partition.label,
            partition.subtype,
            kind.as_str()
        ));
    }
    Ok(kind)
}

struct Report<'a> {
    args: &'a BuildfsArgs,
    context: &'a ProjectContext,
    partition: &'a Partition,
    kind: FsKind,
    files: &'a [StagedFile],
    required: u64,
    image: &'a NormalizedPath,
    image_bytes: u64,
    tool: Option<&'a NormalizedPath>,
}

fn report(report: &Report<'_>) {
    let content_bytes: u64 = report.files.iter().map(|f| f.bytes).sum();
    if report.args.json {
        let summary = serde_json::json!({
            "environment": report.context.env,
            "filesystem": report.kind.as_str(),
            "partition": {
                "label": report.partition.label,
                "offset": report.partition.offset,
                "size": report.partition.size,
            },
            "data_dir": report.context.data_dir.as_path().display().to_string(),
            "files": report.files,
            "content_bytes": content_bytes,
            "estimated_image_bytes": report.required,
            "image": report.tool.map(|_| report.image.as_path().display().to_string()),
            "image_bytes": report.image_bytes,
            "tool": report.tool.map(|tool| tool.as_path().display().to_string()),
            "check_only": report.args.check_only,
        });
        output::result(serde_json::to_string_pretty(&summary).unwrap_or_default());
        return;
    }
    output::result(format!(
        "filesystem: {}   partition: {} @ 0x{:x} ({} bytes)",
        report.kind.as_str(),
        report.partition.label,
        report.partition.offset,
        report.partition.size
    ));
    output::result(format!(
        "data: {} file(s), {} bytes; image estimate {} bytes ({:.0}% of partition)",
        report.files.len(),
        content_bytes,
        report.required,
        report.required as f64 * 100.0 / report.partition.size as f64,
    ));
    match report.tool {
        Some(_) => output::result(format!(
            "image: {} ({} bytes)",
            report.image.as_path().display(),
            report.image_bytes
        )),
        None => output::result("check only: image not built"),
    }
}

async fn run_image_tool(
    tool: &NormalizedPath,
    staging: &NormalizedPath,
    size: u64,
    image: &NormalizedPath,
    geometry: FsGeometry,
) -> Result<()> {
    let tool_path = tool.as_path().to_string_lossy().into_owned();
    let argv = geometry.tool_args(staging.as_path(), size, image.as_path());
    let mut args: Vec<&str> = vec![tool_path.as_str()];
    args.extend(argv.iter().map(String::as_str));
    let result =
        fbuild_core::subprocess::run_command(&args, None, None, Some(Duration::from_secs(180)))
            .await?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "{} failed ({}):\n{}{}",
            tool.as_path().display(),
            result.exit_code,
            result.stderr,
            result.stdout
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "buildfs_tests.rs"]
mod tests;
