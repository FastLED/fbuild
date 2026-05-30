//! `build_info.json` emitter for PlatformIO-compatible downstream tooling.
//!
//! Mirrors the JSON blob produced by `pio project metadata --json-output`
//! (a single top-level map keyed by env name → per-env metadata). Consumed
//! by FastLED's `ci/compiled_size.py` and related size/symbol scripts so
//! they keep working when the build is driven by fbuild instead of PIO.
//!
//! Tracking: FastLED/fbuild#297.
//!
//! ## File layout
//!
//! Written to `<project>/.build/pio/<board>/build_info_<example>.json` when
//! an example name is supplied, otherwise `build_info.json` in the same
//! directory. This matches the path candidates that the FastLED consumer
//! scripts probe.
//!
//! ## Gating
//!
//! Generation is opt-in via the `--emit-build-info` CLI flag (plumbed
//! through `BuildParams::emit_build_info`). Non-CI builds skip the I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use fbuild_core::{FbuildError, Result};

/// Per-env metadata blob written under the env-name key.
///
/// Field set mirrors what `pio project metadata --json-output` returns for
/// each environment. Any field that fbuild cannot populate is left empty
/// (empty string, empty vec) rather than omitted, so consumers can probe
/// keys unconditionally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildInfoMetadata {
    /// Absolute path to the linked firmware ELF/HEX/BIN — the primary
    /// artifact consumers reach for via `prog_path`.
    pub prog_path: String,
    /// Absolute path to the C compiler (e.g. `avr-gcc`, `xtensa-esp32-elf-gcc`).
    pub cc_path: String,
    /// Absolute path to the C++ compiler (e.g. `avr-g++`).
    pub cxx_path: String,
    /// Absolute path to the archiver (`ar`).
    pub ar_path: String,
    /// Absolute path to `objcopy`.
    pub objcopy_path: String,
    /// Absolute path to `objdump`.
    pub objdump_path: String,
    /// Absolute path to `addr2line`.
    pub addr2line_path: String,
    /// Absolute path to the `size` tool.
    pub size_path: String,

    /// C compile flags (no `-D` / `-I`, those go to `defines` / `includes`).
    pub cc_flags: Vec<String>,
    /// C++ compile flags.
    pub cxx_flags: Vec<String>,
    /// Link-time flags.
    pub link_flags: Vec<String>,
    /// Link libraries (e.g. `c`, `m`, `gcc`).
    pub libs: Vec<String>,
    /// Preprocessor defines as `KEY=VALUE` strings (PIO emits a flat list).
    pub defines: Vec<String>,
    /// Include directory paths.
    pub includes: Vec<String>,

    /// Raw `board.build.extra_flags` from board JSON / `platformio.ini`.
    pub extra_flags: Vec<String>,
    /// Sketch source files compiled into the firmware.
    pub srcs: Vec<String>,
    /// Frameworks resolved for this env (e.g. `arduino`).
    pub frameworks: Vec<String>,
    /// PlatformIO platform string (e.g. `atmelavr`, `espressif32`).
    pub platform: String,
    /// Board id (e.g. `uno`, `esp32dev`).
    pub board: String,
}

/// Builder that accumulates per-field data and writes the final
/// `build_info_<example>.json` (or `build_info.json`) to disk.
///
/// Designed as a `with_*` chain so orchestrators can populate only the
/// fields they have without juggling 20-argument helper signatures.
#[derive(Debug, Clone, Default)]
pub struct BuildInfoBuilder {
    metadata: BuildInfoMetadata,
}

impl BuildInfoBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_prog_path(mut self, path: Option<&Path>) -> Self {
        if let Some(p) = path {
            self.metadata.prog_path = p.to_string_lossy().to_string();
        }
        self
    }

    pub fn with_cc_path(mut self, path: &Path) -> Self {
        self.metadata.cc_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_cxx_path(mut self, path: &Path) -> Self {
        self.metadata.cxx_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_ar_path(mut self, path: &Path) -> Self {
        self.metadata.ar_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_objcopy_path(mut self, path: &Path) -> Self {
        self.metadata.objcopy_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_objdump_path(mut self, path: &Path) -> Self {
        self.metadata.objdump_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_addr2line_path(mut self, path: &Path) -> Self {
        self.metadata.addr2line_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_size_path(mut self, path: &Path) -> Self {
        self.metadata.size_path = path.to_string_lossy().to_string();
        self
    }

    pub fn with_cc_flags(mut self, flags: Vec<String>) -> Self {
        self.metadata.cc_flags = flags;
        self
    }

    pub fn with_cxx_flags(mut self, flags: Vec<String>) -> Self {
        self.metadata.cxx_flags = flags;
        self
    }

    pub fn with_link_flags(mut self, flags: Vec<String>) -> Self {
        self.metadata.link_flags = flags;
        self
    }

    pub fn with_libs(mut self, libs: Vec<String>) -> Self {
        self.metadata.libs = libs;
        self
    }

    /// Set defines from a `HashMap<name, value>`.
    ///
    /// Output is sorted for deterministic JSON and serialized as `KEY=VALUE`
    /// (or bare `KEY` when value is `"1"`), matching how PIO emits defines.
    pub fn with_defines_map(mut self, defines: &HashMap<String, String>) -> Self {
        let mut items: Vec<String> = defines
            .iter()
            .map(|(k, v)| {
                if v == "1" {
                    k.clone()
                } else {
                    format!("{}={}", k, v)
                }
            })
            .collect();
        items.sort();
        self.metadata.defines = items;
        self
    }

    pub fn with_includes(mut self, includes: &[PathBuf]) -> Self {
        self.metadata.includes = includes
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        self
    }

    pub fn with_extra_flags(mut self, raw: Option<&str>) -> Self {
        if let Some(raw) = raw {
            self.metadata.extra_flags = fbuild_core::shell_split::split(raw);
        }
        self
    }

    pub fn with_srcs(mut self, srcs: &[PathBuf]) -> Self {
        self.metadata.srcs = srcs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        self
    }

    pub fn with_frameworks(mut self, frameworks: Vec<String>) -> Self {
        self.metadata.frameworks = frameworks;
        self
    }

    pub fn with_platform(mut self, platform: impl Into<String>) -> Self {
        self.metadata.platform = platform.into();
        self
    }

    pub fn with_board(mut self, board: impl Into<String>) -> Self {
        self.metadata.board = board.into();
        self
    }

    /// Consume the builder and return the populated metadata struct.
    pub fn build(self) -> BuildInfoMetadata {
        self.metadata
    }
}

/// Compute the path where `build_info_<example>.json` (or
/// `build_info.json` when `example_name` is `None`) should be written.
///
/// Always lands under `<project_dir>/.build/pio/<board>/` so the path
/// matches the candidate list in FastLED's `_find_build_info`.
pub fn build_info_path(project_dir: &Path, board: &str, example_name: Option<&str>) -> PathBuf {
    let dir = project_dir.join(".build").join("pio").join(board);
    let filename = match example_name {
        Some(name) if !name.is_empty() => format!("build_info_{}.json", name),
        _ => "build_info.json".to_string(),
    };
    dir.join(filename)
}

/// Wrap `metadata` under the env-name key (PIO's convention) and write
/// pretty-printed JSON to disk.
///
/// Creates the parent directory if necessary. Returns the absolute path of
/// the written file on success.
pub fn write_build_info_json(
    project_dir: &Path,
    env_name: &str,
    board: &str,
    example_name: Option<&str>,
    metadata: &BuildInfoMetadata,
) -> Result<PathBuf> {
    let path = build_info_path(project_dir, board, example_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            FbuildError::Other(format!(
                "failed to create build_info.json parent dir {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    let mut envelope: HashMap<String, &BuildInfoMetadata> = HashMap::new();
    envelope.insert(env_name.to_string(), metadata);
    let body = serde_json::to_string_pretty(&envelope)
        .map_err(|e| FbuildError::Other(format!("failed to serialize build_info.json: {}", e)))?;
    std::fs::write(&path, body).map_err(|e| {
        FbuildError::Other(format!(
            "failed to write build_info.json to {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

/// Derive a default example name from a project directory's basename.
///
/// FastLED's CI invokes fbuild with project dirs like `tests/platform/uno`,
/// `examples/Blink`, etc. The basename is the conventional example name.
/// Returns `None` for unprintable / empty basenames.
pub fn default_example_name(project_dir: &Path) -> Option<String> {
    project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Snapshot of the per-build data needed by the `build_info.json` emitter,
/// captured before the orchestrator hands off ownership of its
/// `BuildContext` to the shared pipeline.
///
/// The pipeline consumes `BuildContext` by value, so any orchestrator that
/// wants to emit `build_info.json` *after* a successful link must clone the
/// reachable fields first. This struct factors out that bookkeeping so each
/// orchestrator only has to populate one place.
#[derive(Debug, Clone)]
pub struct BuildInfoSnapshot {
    pub board: fbuild_config::BoardConfig,
    pub defines: HashMap<String, String>,
    pub include_dirs: Vec<PathBuf>,
    pub sketch_sources: Vec<PathBuf>,
    pub link_flags: Vec<String>,
    pub link_libs: Vec<String>,
}

/// Convenience inputs aggregating the data orchestrators already have
/// readily available when they want to emit `build_info.json`.
///
/// Designed to be cheap to build at the link-result handoff point: each
/// field is a borrowed slice / reference into the existing build context.
/// The orchestrator passes this to [`emit_build_info_for_orchestrator`],
/// which assembles the metadata, writes the JSON, and returns the output
/// path on success.
pub struct OrchestratorBuildInfoInputs<'a> {
    pub project_dir: &'a Path,
    pub env_name: &'a str,
    pub board: &'a fbuild_config::BoardConfig,
    pub compiler: &'a dyn crate::compiler::Compiler,
    pub linker: &'a dyn crate::linker::Linker,
    pub include_dirs: &'a [PathBuf],
    pub defines: &'a HashMap<String, String>,
    pub link_libs: &'a [String],
    pub link_flags: &'a [String],
    pub sketch_sources: &'a [PathBuf],
    pub frameworks: Vec<String>,
    pub platform: &'a str,
    pub prog_path: Option<&'a Path>,
    pub example_name: Option<&'a str>,
}

/// Assemble a [`BuildInfoMetadata`] from orchestrator-side data and write
/// the JSON to disk.
///
/// Tool paths fbuild does not natively track (`objdump`, `addr2line`) are
/// inferred from the size tool's parent directory and naming prefix when
/// possible, matching FastLED's `insert_tool_aliases` fallback logic.
///
/// Returns the path of the written file on success.
pub fn emit_build_info_for_orchestrator(
    inputs: OrchestratorBuildInfoInputs<'_>,
) -> Result<PathBuf> {
    let cc_path = inputs.compiler.gcc_path();
    let cxx_path = inputs.compiler.gxx_path();
    let size_path = inputs.linker.size_tool_path();
    // Filter out the include/define payload from compiler flags so the
    // `cc_flags` / `cxx_flags` arrays match PIO's convention (which keeps
    // `-D` and `-I` in `defines` / `includes` instead). Splitting also lets
    // size-check scripts diff flags without churn from include paths.
    let cc_flags = strip_define_include(inputs.compiler.c_flags());
    let cxx_flags = strip_define_include(inputs.compiler.cpp_flags());

    let (objdump_path, addr2line_path) = derive_objdump_addr2line(size_path);
    let ar_path = inputs
        .linker
        .ar_path()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let objcopy_path = inputs
        .linker
        .objcopy_path()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let metadata = BuildInfoBuilder::new()
        .with_prog_path(inputs.prog_path)
        .with_cc_path(cc_path)
        .with_cxx_path(cxx_path)
        .with_ar_path(&ar_path)
        .with_objcopy_path(&objcopy_path)
        .with_objdump_path(&objdump_path)
        .with_addr2line_path(&addr2line_path)
        .with_size_path(size_path)
        .with_cc_flags(cc_flags)
        .with_cxx_flags(cxx_flags)
        .with_link_flags(inputs.link_flags.to_vec())
        .with_libs(inputs.link_libs.to_vec())
        .with_defines_map(inputs.defines)
        .with_includes(inputs.include_dirs)
        .with_extra_flags(inputs.board.extra_flags.as_deref())
        .with_srcs(inputs.sketch_sources)
        .with_frameworks(inputs.frameworks)
        .with_platform(inputs.platform)
        .with_board(&inputs.board.board)
        .build();

    write_build_info_json(
        inputs.project_dir,
        inputs.env_name,
        &inputs.board.board,
        inputs.example_name,
        &metadata,
    )
}

/// Drop `-D` and `-I` flags from a compile-flag list so the remainder is
/// safe to drop into `cc_flags` / `cxx_flags` alongside the separate
/// `defines` / `includes` arrays (PIO's convention).
fn strip_define_include(flags: Vec<String>) -> Vec<String> {
    flags
        .into_iter()
        .filter(|f| !f.starts_with("-D") && !f.starts_with("-I"))
        .collect()
}

/// Derive `objdump` and `addr2line` paths from the `size` tool's path by
/// substituting the tool basename suffix. Returns empty paths when the
/// `size` basename doesn't end in `size` (in which case the consumer can
/// fall back to PATH lookups via `insert_tool_aliases`).
fn derive_objdump_addr2line(size_path: &Path) -> (PathBuf, PathBuf) {
    let Some(file_name) = size_path.file_name().and_then(|n| n.to_str()) else {
        return (PathBuf::new(), PathBuf::new());
    };
    let Some(parent) = size_path.parent() else {
        return (PathBuf::new(), PathBuf::new());
    };
    // `avr-size` → prefix `avr-`, suffix `""` (or `.exe` on Windows).
    let (prefix, suffix) = split_tool_name(file_name, "size");
    let Some((prefix, suffix)) = prefix.map(|p| (p, suffix)) else {
        return (PathBuf::new(), PathBuf::new());
    };
    let objdump = parent.join(format!("{}objdump{}", prefix, suffix));
    let addr2line = parent.join(format!("{}addr2line{}", prefix, suffix));
    (objdump, addr2line)
}

/// Split a tool basename like `arm-none-eabi-size.exe` around the substring
/// `tool` and return `(Some(prefix), suffix)` on a match. Returns
/// `(None, "")` when the substring is absent.
fn split_tool_name<'a>(file_name: &'a str, tool: &str) -> (Option<&'a str>, &'a str) {
    let Some(idx) = file_name.rfind(tool) else {
        return (None, "");
    };
    let prefix = &file_name[..idx];
    let suffix = &file_name[idx + tool.len()..];
    (Some(prefix), suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the metadata struct through serde and assert the
    /// PIO-shaped keys are all present on the wire.
    #[test]
    fn metadata_serializes_with_expected_keys() {
        let mut defines = HashMap::new();
        defines.insert("F_CPU".to_string(), "16000000L".to_string());
        defines.insert("PLATFORMIO".to_string(), "1".to_string());
        let metadata = BuildInfoBuilder::new()
            .with_prog_path(Some(Path::new("/tmp/firmware.hex")))
            .with_cc_path(Path::new("/tc/avr-gcc"))
            .with_cxx_path(Path::new("/tc/avr-g++"))
            .with_ar_path(Path::new("/tc/avr-ar"))
            .with_objcopy_path(Path::new("/tc/avr-objcopy"))
            .with_objdump_path(Path::new("/tc/avr-objdump"))
            .with_addr2line_path(Path::new("/tc/avr-addr2line"))
            .with_size_path(Path::new("/tc/avr-size"))
            .with_cc_flags(vec!["-std=gnu11".into(), "-Os".into()])
            .with_cxx_flags(vec!["-std=gnu++11".into()])
            .with_link_flags(vec!["-Wl,--gc-sections".into()])
            .with_libs(vec!["m".into()])
            .with_defines_map(&defines)
            .with_includes(&[
                PathBuf::from("/cores/arduino"),
                PathBuf::from("/variants/standard"),
            ])
            .with_extra_flags(Some("-DBOARD_X=1 -DBOARD_Y"))
            .with_srcs(&[PathBuf::from("/proj/src/main.cpp")])
            .with_frameworks(vec!["arduino".into()])
            .with_platform("atmelavr")
            .with_board("uno")
            .build();

        let json = serde_json::to_value(&metadata).unwrap();
        for key in [
            "prog_path",
            "cc_path",
            "cxx_path",
            "ar_path",
            "objcopy_path",
            "objdump_path",
            "addr2line_path",
            "size_path",
            "cc_flags",
            "cxx_flags",
            "link_flags",
            "libs",
            "defines",
            "includes",
            "extra_flags",
            "srcs",
            "frameworks",
            "platform",
            "board",
        ] {
            assert!(json.get(key).is_some(), "expected key '{}' in JSON", key);
        }

        // Defines must be sorted (deterministic output).
        let defines = json.get("defines").unwrap().as_array().unwrap();
        let strs: Vec<&str> = defines.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(strs, vec!["F_CPU=16000000L", "PLATFORMIO"]);

        // extra_flags split with shell semantics.
        let extra = json.get("extra_flags").unwrap().as_array().unwrap();
        assert_eq!(extra.len(), 2);
    }

    /// File layout: `.build/pio/<board>/build_info_<example>.json` when an
    /// example name is supplied; `build_info.json` otherwise. Matches the
    /// candidates that the FastLED consumer probes.
    #[test]
    fn path_layout_matches_fastled_consumer() {
        let project = Path::new("/some/project");
        let with_example = build_info_path(project, "uno", Some("Blink"));
        assert!(with_example.ends_with(".build/pio/uno/build_info_Blink.json"));

        let without = build_info_path(project, "uno", None);
        assert!(without.ends_with(".build/pio/uno/build_info.json"));

        let empty_example = build_info_path(project, "uno", Some(""));
        assert!(empty_example.ends_with(".build/pio/uno/build_info.json"));
    }

    /// Writing produces a parseable JSON envelope keyed by env name.
    #[test]
    fn write_creates_parseable_envelope() {
        let tmp = tempfile::TempDir::new().unwrap();
        let metadata = BuildInfoBuilder::new()
            .with_prog_path(Some(Path::new("/tmp/firmware.elf")))
            .with_cc_path(Path::new("/tc/gcc"))
            .with_cxx_path(Path::new("/tc/g++"))
            .with_platform("atmelavr")
            .with_board("uno")
            .build();

        let written =
            write_build_info_json(tmp.path(), "uno", "uno", Some("Blink"), &metadata).unwrap();
        assert!(written.exists());
        assert!(written.ends_with(".build/pio/uno/build_info_Blink.json"));

        // Parses as `{env_name: {...}}` and exposes prog_path under the env key.
        let blob: HashMap<String, BuildInfoMetadata> =
            serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert_eq!(blob.len(), 1);
        let inner = blob.get("uno").expect("envelope keyed by env name");
        assert_eq!(inner.prog_path, "/tmp/firmware.elf");
        assert_eq!(inner.cc_path, "/tc/gcc");
        assert_eq!(inner.platform, "atmelavr");
        assert_eq!(inner.board, "uno");
    }

    #[test]
    fn default_example_name_uses_basename() {
        assert_eq!(
            default_example_name(Path::new("/foo/bar/examples/Blink")),
            Some("Blink".to_string())
        );
        assert_eq!(
            default_example_name(Path::new("tests/platform/uno")),
            Some("uno".to_string())
        );
    }
}
