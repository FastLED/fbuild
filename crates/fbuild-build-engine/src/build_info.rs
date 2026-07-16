//! Post-link emitter for `build_info_<env>.json`.
//!
//! Writes a PlatformIO-compatible `build_info_<env>.json` (and a duplicate
//! `build_info.json` fallback) to the project directory after a successful
//! link. The schema matches `pio project metadata --json-output`: the outer
//! object is keyed by environment name, and the inner object carries the
//! toolchain binaries and effective flags fbuild already knows about.
//!
//! FastLED's `ci/compiled_size.py::_create_board_info`, `ci/inspect_binary.py`,
//! `ci/symbol_analysis_runner.py`, and similar size/symbol tooling consume
//! this file unmodified. Without it, every fbuild-driven size check silently
//! fails because the consumer's `_find_build_info()` lookup can't locate the
//! metadata file PlatformIO would have written.
//!
//! See FastLED/fbuild#297.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};

/// PlatformIO-shape build metadata for one environment.
///
/// All paths are emitted as strings (matching `pio project metadata`
/// output); empty / missing toolchain entries are emitted as empty strings
/// so consumers that do `Path(board_info["objcopy_path"])` never KeyError.
///
/// The ten `*_path` fields are [`NormalizedPath`] (Phase 2 of #437), so
/// the emitted JSON is byte-identical across Linux/macOS/Windows even
/// when the source paths were constructed via `PathBuf::join` (which
/// would otherwise leak `\` separators on Windows — the original
/// regression from PR #436).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    /// Absolute path to the final firmware/program file (`.elf` if no
    /// `.hex`/`.bin` was produced, otherwise the converted firmware).
    pub prog_path: NormalizedPath,
    /// Absolute path to the C compiler (`gcc`).
    pub cc_path: NormalizedPath,
    /// Absolute path to the C++ compiler (`g++`).
    pub cxx_path: NormalizedPath,
    /// Absolute path to `ar` (empty when the linker doesn't expose it).
    pub ar_path: NormalizedPath,
    /// Absolute path to `objcopy` (empty when the platform has no objcopy step,
    /// e.g. ESP8266 which produces ELF directly).
    pub objcopy_path: NormalizedPath,
    /// Absolute path to `size`.
    pub size_path: NormalizedPath,
    /// Absolute path to `nm` (GCC convention: derived from `size_path` by
    /// replacing the `size` suffix with `nm`). Consumed by `fbuild symbols`
    /// (see #428) so users don't have to pass `--nm` on every invocation.
    #[serde(default)]
    pub nm_path: NormalizedPath,
    /// Absolute path to `c++filt` (GCC convention). Used by the symbol
    /// analyzer to demangle the names `nm` emits.
    #[serde(default)]
    pub cppfilt_path: NormalizedPath,
    /// Absolute path to `readelf`. Useful for downstream section/segment
    /// inspection.
    #[serde(default)]
    pub readelf_path: NormalizedPath,
    /// Absolute path to `objdump`. Useful for downstream disassembly /
    /// section dumps.
    #[serde(default)]
    pub objdump_path: NormalizedPath,
    /// Effective C compile flags as seen by the compiler driver.
    pub cc_flags: Vec<String>,
    /// Effective C++ compile flags as seen by the compiler driver.
    pub cxx_flags: Vec<String>,
    /// Effective link flags (does not include object files or `-l<lib>` libs).
    pub link_flags: Vec<String>,
    /// `-D` defines extracted from `cxx_flags`.
    pub defines: Vec<String>,
    /// `-I` includes extracted from `cxx_flags`.
    pub includes: Vec<String>,
    /// Libraries passed to the linker (e.g. `-lc`, `-lm`).
    pub libs: Vec<String>,
    /// Platform identifier (e.g. `atmelavr`, `ststm32`).
    pub platform: String,
    /// Board identifier (e.g. `uno`, `teensy41`).
    pub board: String,
    /// PlatformIO env name (e.g. `uno`, `teensy41-debug`).
    pub env: String,
    /// PIO-shape mirror of the toolchain paths. PlatformIO emits an
    /// `aliases` block keyed by short tool names (`nm`, `c++filt`,
    /// `readelf`, `objdump`, `gcc`, etc.); FastLED's
    /// `ci/util/symbol_analysis.py` and `ci/inspect_binary.py` read from
    /// it. Mirroring keeps existing PIO consumers working drop-in
    /// against fbuild-built artifacts. See #428.
    ///
    /// Values are [`NormalizedPath`] (Phase 2 of #437) so on-disk JSON
    /// stays byte-identical across platforms; the `NormalizedPath`
    /// serde impls produce / accept plain JSON strings, so Python
    /// consumers see no schema change.
    #[serde(default)]
    pub aliases: BTreeMap<String, NormalizedPath>,
}

impl BuildInfo {
    /// Construct a `BuildInfo` from already-collected pieces. Splits
    /// `-D` defines and `-I` includes out of `cxx_flags` (matching
    /// PlatformIO's metadata-emitter convention) without removing them
    /// from `cxx_flags` itself.
    ///
    /// The four diagnostic toolchain paths (`nm`, `c++filt`, `readelf`,
    /// `objdump`) are derived from `size_path` using the GCC cross-tool
    /// naming convention (`<prefix>-size` → `<prefix>-nm`, etc.). The
    /// `aliases` block mirrors these onto short keys for PIO consumer
    /// compatibility. See #428.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        prog_path: &Path,
        cc_path: Option<&Path>,
        cxx_path: Option<&Path>,
        ar_path: Option<&Path>,
        objcopy_path: Option<&Path>,
        size_path: &Path,
        cc_flags: Vec<String>,
        cxx_flags: Vec<String>,
        link_flags: Vec<String>,
        libs: Vec<String>,
        platform: String,
        board: String,
        env: String,
    ) -> Self {
        let defines = extract_prefixed(&cxx_flags, "-D");
        let includes = extract_prefixed(&cxx_flags, "-I");
        let nm_path = derive_gcc_tool_path(size_path, "nm");
        let cppfilt_path = derive_gcc_tool_path(size_path, "c++filt");
        let readelf_path = derive_gcc_tool_path(size_path, "readelf");
        let objdump_path = derive_gcc_tool_path(size_path, "objdump");
        let aliases = build_aliases(
            cc_path,
            cxx_path,
            ar_path,
            objcopy_path,
            size_path,
            &nm_path,
            &cppfilt_path,
            &readelf_path,
            &objdump_path,
        );
        Self {
            prog_path: NormalizedPath::new(prog_path),
            cc_path: normalize_optional(cc_path),
            cxx_path: normalize_optional(cxx_path),
            ar_path: normalize_optional(ar_path),
            objcopy_path: normalize_optional(objcopy_path),
            size_path: NormalizedPath::new(size_path),
            nm_path: NormalizedPath::new(&nm_path),
            cppfilt_path: NormalizedPath::new(&cppfilt_path),
            readelf_path: NormalizedPath::new(&readelf_path),
            objdump_path: NormalizedPath::new(&objdump_path),
            cc_flags,
            cxx_flags,
            link_flags,
            defines,
            includes,
            libs,
            platform,
            board,
            env,
            aliases,
        }
    }
}

/// Derive a sibling GCC cross-tool path from `size_path`.
///
/// For a `size_path` like `/toolchain/bin/xtensa-esp32-elf-size[.exe]`,
/// returns `/toolchain/bin/xtensa-esp32-elf-<target>[.exe]`. When the
/// stem doesn't end in `size` (rare — bare `size` on PATH), returns
/// `<parent>/<target>` preserving any extension.
pub fn derive_gcc_tool_path(size_path: &Path, target: &str) -> PathBuf {
    let parent = size_path.parent().unwrap_or(Path::new("."));
    let stem = size_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let new_stem = if let Some(prefix) = stem.strip_suffix("size") {
        format!("{prefix}{target}")
    } else {
        target.to_string()
    };
    match size_path.extension() {
        Some(ext) if !ext.is_empty() => {
            parent.join(format!("{new_stem}.{}", ext.to_string_lossy()))
        }
        _ => parent.join(new_stem),
    }
}

/// Build the PIO-shape `aliases` block. Keys match PlatformIO's
/// `pio project metadata` output so FastLED's existing Python
/// consumers (`ci/util/symbol_analysis.py`, `ci/inspect_binary.py`)
/// keep working unchanged. Empty paths are skipped so consumers can
/// trust `aliases["nm"]` is non-empty whenever the key is present.
#[allow(clippy::too_many_arguments)]
fn build_aliases(
    cc_path: Option<&Path>,
    cxx_path: Option<&Path>,
    ar_path: Option<&Path>,
    objcopy_path: Option<&Path>,
    size_path: &Path,
    nm_path: &Path,
    cppfilt_path: &Path,
    readelf_path: &Path,
    objdump_path: &Path,
) -> BTreeMap<String, NormalizedPath> {
    let mut aliases = BTreeMap::new();
    let entries: &[(&str, Option<&Path>)] = &[
        ("gcc", cc_path),
        ("g++", cxx_path),
        ("ar", ar_path),
        ("objcopy", objcopy_path),
        ("size", Some(size_path)),
        ("nm", Some(nm_path)),
        ("c++filt", Some(cppfilt_path)),
        ("readelf", Some(readelf_path)),
        ("objdump", Some(objdump_path)),
    ];
    for (key, path) in entries {
        let Some(p) = *path else {
            continue;
        };
        // Skip the empty-path sentinel so `aliases["nm"]` is
        // guaranteed non-empty whenever the key is present.
        if p.as_os_str().is_empty() {
            continue;
        }
        aliases.insert((*key).to_string(), NormalizedPath::new(p));
    }
    aliases
}

/// Convert an optional `&Path` to [`NormalizedPath`], preserving the
/// "missing → empty path" sentinel that `BuildInfo`'s schema requires
/// for downstream Python consumers (`Path(board_info["objcopy_path"])`
/// never throws KeyError).
fn normalize_optional(p: Option<&Path>) -> NormalizedPath {
    p.map(NormalizedPath::new).unwrap_or_default()
}

fn extract_prefixed(flags: &[String], prefix: &str) -> Vec<String> {
    flags
        .iter()
        .filter_map(|f| f.strip_prefix(prefix).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Emit `build_info_<env>.json` and `build_info.json` next to the project's
/// `platformio.ini`.
///
/// Both files carry the same payload — `build_info.json` is the
/// no-example-name fallback FastLED's `_find_build_info()` walks. Writing
/// failures degrade to `tracing::warn!` rather than failing the build; an
/// otherwise-successful link should never be reported as failed because the
/// downstream metadata file couldn't be written.
///
/// `env_name` may contain forward or backward slashes when the caller is
/// driving fbuild per-example with nested example names (e.g. FastLED's
/// `Fx/FxNoisePalette`, `Multiple/ArrayOfLedArrays`, or
/// `SpecialDrivers/Adafruit/AdafruitBridge`). FastLED's `_find_build_info()`
/// expands those into a nested subdirectory path via
/// `Path(".build/pio/<board>") / f"build_info_{example}.json"`, so to make
/// the file land where the consumer looks for it we must create the parent
/// directory tree before writing. See FastLED/fbuild#406.
pub fn emit_build_info(project_dir: &Path, env_name: &str, info: &BuildInfo) -> Result<()> {
    let outer = std::collections::BTreeMap::from([(env_name.to_string(), info.clone())]);
    let json = serde_json::to_string_pretty(&outer).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("failed to serialize build_info: {e}"))
    })?;

    let env_specific = project_dir.join(format!("build_info_{env_name}.json"));
    let generic = project_dir.join("build_info.json");

    for path in [&env_specific, &generic] {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "failed to create parent directory {} for build_info: {}",
                    parent.display(),
                    e
                );
                continue;
            }
        }
        // Atomic write — FastLED/fbuild#844 bridge pair 6.
        // `emit_build_info` is a sync function called from sync build
        // pipeline code, but always under the daemon's tokio runtime.
        // Bridge to the async `write_atomic` via `block_in_place`. Same
        // pattern as `fbuild_packages::toolchain::esp32_metadata`.
        let write_res = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(fbuild_core::fs::write_atomic(path, json.as_bytes()))
            })
        } else {
            // No runtime — happens in unit tests of this module.
            // Fall back to plain `std::fs::write`; the integration
            // path always has a runtime so the atomic guarantee is
            // preserved where it matters.
            std::fs::write(path, &json)
        };
        if let Err(e) = write_res {
            tracing::warn!("failed to write {}: {}", path.display(), e);
        }
    }
    Ok(())
}

/// Build a `prog_path` candidate by preferring the firmware file (`.hex`/`.bin`)
/// when present and falling back to the ELF. Matches FastLED's expectation
/// that `prog_path.parent / firmware.{bin,uf2,hex}` resolves to actual flash.
pub fn pick_prog_path(
    elf: Option<&Path>,
    hex: Option<&Path>,
    bin: Option<&Path>,
) -> Option<PathBuf> {
    bin.map(Path::to_path_buf)
        .or_else(|| hex.map(Path::to_path_buf))
        .or_else(|| elf.map(Path::to_path_buf))
}

/// Load a `build_info_<env>.json` (or `build_info.json`) file and return
/// the inner [`BuildInfo`] for its single environment.
///
/// The PlatformIO + fbuild emitter convention is a one-key outer object
/// (`{ "<env>": { ...BuildInfo } }`); this helper unwraps that
/// transparently and errors out if the file doesn't match the shape.
/// Consumed by `fbuild symbols` (#428) to resolve toolchain paths
/// without the user passing `--nm`.
pub fn load_build_info(path: &Path) -> Result<(String, BuildInfo)> {
    let bytes = std::fs::read(path).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("read {}: {e}", path.display()))
    })?;
    let outer: BTreeMap<String, BuildInfo> = serde_json::from_slice(&bytes).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "{}: not a valid build_info.json: {e}",
            path.display()
        ))
    })?;
    let mut it = outer.into_iter();
    let (env, info) = it.next().ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "{}: build_info.json is empty (expected exactly one env)",
            path.display()
        ))
    })?;
    if it.next().is_some() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "{}: build_info.json carries more than one env (expected exactly one)",
            path.display()
        )));
    }
    Ok((env, info))
}

/// Walk upward from `start` looking for a `build_info.json` (or any
/// `build_info_<env>.json`). PIO and fbuild both write these next to
/// `platformio.ini`, but `start` is typically the directory containing
/// `firmware.elf` (`.fbuild/build/<env>/` or `.pio/build/<env>/`). We
/// search `start` itself first, then walk parent directories.
///
/// Returns `None` when no metadata file is found before reaching the
/// filesystem root. `fbuild symbols` falls back to PATH-based `nm`
/// lookup in that case so the command keeps working against bare ELFs.
pub fn find_build_info_near(start: &Path) -> Option<PathBuf> {
    let mut cursor: Option<&Path> = Some(start);
    while let Some(dir) = cursor {
        if let Some(found) = scan_dir_for_build_info(dir) {
            return Some(found);
        }
        cursor = dir.parent();
    }
    None
}

fn scan_dir_for_build_info(dir: &Path) -> Option<PathBuf> {
    let generic = dir.join("build_info.json");
    if generic.is_file() {
        return Some(generic);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("build_info_") && name.ends_with(".json") && path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_info() -> BuildInfo {
        BuildInfo::new(
            Path::new("/build/firmware.elf"),
            Some(Path::new("/bin/avr-gcc")),
            Some(Path::new("/bin/avr-g++")),
            Some(Path::new("/bin/avr-ar")),
            Some(Path::new("/bin/avr-objcopy")),
            Path::new("/bin/avr-size"),
            vec!["-Os".to_string(), "-DUSER=1".to_string()],
            vec![
                "-Os".to_string(),
                "-I/inc".to_string(),
                "-DFOO=bar".to_string(),
                "-DUSER=1".to_string(),
            ],
            vec!["-Wl,--gc-sections".to_string()],
            vec!["-lm".to_string(), "-lc".to_string()],
            "atmelavr".to_string(),
            "uno".to_string(),
            "uno".to_string(),
        )
    }

    #[test]
    fn build_info_splits_defines_and_includes() {
        let info = sample_info();
        assert_eq!(
            info.defines,
            vec!["FOO=bar".to_string(), "USER=1".to_string()]
        );
        assert_eq!(info.includes, vec!["/inc".to_string()]);
        // cxx_flags must still carry the originals (defines/includes are a
        // *projection* — PlatformIO's metadata-emitter emits both).
        assert!(info.cxx_flags.iter().any(|f| f == "-DFOO=bar"));
        assert!(info.cxx_flags.iter().any(|f| f == "-I/inc"));
    }

    #[test]
    fn build_info_handles_missing_optional_tools() {
        let info = BuildInfo::new(
            Path::new("/build/firmware.elf"),
            Some(Path::new("/bin/gcc")),
            Some(Path::new("/bin/g++")),
            None, // no ar
            None, // no objcopy
            Path::new("/bin/size"),
            vec![],
            vec![],
            vec![],
            vec![],
            "esp8266".to_string(),
            "nodemcuv2".to_string(),
            "nodemcuv2".to_string(),
        );
        // Missing → `NormalizedPath::default()` (empty path). This is
        // the schema's "absent" sentinel that downstream Python
        // consumers (`Path(board_info["objcopy_path"])`) rely on.
        assert_eq!(info.ar_path, NormalizedPath::default());
        assert_eq!(info.objcopy_path, NormalizedPath::default());
        assert_eq!(info.ar_path.as_path().as_os_str(), "");
        assert_eq!(info.objcopy_path.as_path().as_os_str(), "");
    }

    #[test]
    fn emit_writes_both_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();
        assert!(tmp.path().join("build_info_uno.json").exists());
        assert!(tmp.path().join("build_info.json").exists());
    }

    /// The motivating test for #437 Phase 2: when `BuildInfo`'s ten
    /// `*_path` fields are constructed from `PathBuf::join` (which
    /// uses the platform separator), the emitted JSON must still
    /// contain only forward slashes — no `\` leakage on Windows.
    ///
    /// PR #436 had to gate this exact assertion behind a platform
    /// `pj()` helper because the field type was `String` and just
    /// carried whatever separator the source used. The
    /// `NormalizedPath` migration removes that workaround entirely.
    #[test]
    fn emit_json_uses_forward_slashes_regardless_of_input_separators() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Construct the source paths via `Path::join` so that on
        // Windows the in-memory strings contain `\`. The serialized
        // JSON must still come out slash-form.
        let bin = PathBuf::from("/bin");
        let info = BuildInfo::new(
            &bin.join("firmware.elf"),
            Some(&bin.join("avr-gcc")),
            Some(&bin.join("avr-g++")),
            Some(&bin.join("avr-ar")),
            Some(&bin.join("avr-objcopy")),
            &bin.join("avr-size"),
            vec![],
            vec![],
            vec![],
            vec![],
            "atmelavr".to_string(),
            "uno".to_string(),
            "uno".to_string(),
        );
        emit_build_info(tmp.path(), "uno", &info).unwrap();
        let bytes = std::fs::read(tmp.path().join("build_info_uno.json")).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();

        // The literal Windows separator must not appear in any path
        // string. (A JSON escape sequence `\\` is two backslashes in
        // the raw file, so search for *any* backslash byte.)
        assert!(
            !s.contains('\\'),
            "build_info JSON must not contain backslashes; got:\n{s}"
        );
        // Every path field is present in slash-form.
        assert!(s.contains("\"/bin/avr-gcc\""));
        assert!(s.contains("\"/bin/avr-nm\""));
        assert!(s.contains("\"/bin/avr-c++filt\""));
        assert!(s.contains("\"/bin/avr-readelf\""));
        assert!(s.contains("\"/bin/avr-objdump\""));
    }

    /// FastLED/fbuild#406: when FastLED drives fbuild per-example using a
    /// nested example name like `Fx/FxNoisePalette`, the env-specific file
    /// path expands to `build_info_Fx/FxNoisePalette.json` — i.e. a nested
    /// subdirectory. Previously the write silently failed with
    /// `Exception generating build_info.json: [Errno 2] No such file or
    /// directory`, dropping 161 per-example metadata files across the ESP32
    /// audit matrix. The emitter must create the parent directory tree
    /// before writing.
    #[test]
    fn emit_creates_parent_dirs_for_nested_env_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        // Single-level nesting (e.g. FastLED `Fx/FxNoisePalette`).
        emit_build_info(tmp.path(), "Fx/FxNoisePalette", &info).unwrap();
        assert!(
            tmp.path()
                .join("build_info_Fx")
                .join("FxNoisePalette.json")
                .exists(),
            "build_info_Fx/FxNoisePalette.json must exist after emit"
        );
        // Generic fallback also written at top level.
        assert!(tmp.path().join("build_info.json").exists());
    }

    /// Two-level nesting matching FastLED's
    /// `SpecialDrivers/Adafruit/AdafruitBridge` example layout. The emitter
    /// must walk and create every intermediate directory, not just the
    /// immediate parent.
    #[test]
    fn emit_creates_deeply_nested_parent_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "SpecialDrivers/Adafruit/AdafruitBridge", &info).unwrap();
        assert!(
            tmp.path()
                .join("build_info_SpecialDrivers")
                .join("Adafruit")
                .join("AdafruitBridge.json")
                .exists(),
            "build_info_SpecialDrivers/Adafruit/AdafruitBridge.json must exist after emit"
        );
    }

    /// The outer JSON key must preserve the full nested env name — FastLED's
    /// `_create_board_info` asserts there is exactly one outer key and
    /// reads the inner value via `next(iter(...))`. Sanitizing the env name
    /// would break that contract.
    #[test]
    fn emit_preserves_nested_env_name_as_json_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "Fx/FxNoisePalette", &info).unwrap();

        let path = tmp.path().join("build_info_Fx").join("FxNoisePalette.json");
        let bytes = std::fs::read(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let obj = parsed.as_object().expect("outer is object");
        assert_eq!(obj.len(), 1);
        assert!(
            obj.contains_key("Fx/FxNoisePalette"),
            "nested env name must be preserved as the outer JSON key"
        );
    }

    #[test]
    fn emit_outer_dict_has_single_env_key() {
        // FastLED's _create_board_info asserts exactly one outer key
        // and pulls the inner value via next(iter(...)).
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();

        let bytes = std::fs::read(tmp.path().join("build_info_uno.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let obj = parsed.as_object().expect("outer is object");
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("uno"));

        let inner = obj
            .get("uno")
            .unwrap()
            .as_object()
            .expect("inner is object");
        // Every key FastLED's _create_board_info / check_firmware_size reaches for.
        for required in [
            "prog_path",
            "cc_path",
            "cxx_path",
            "ar_path",
            "objcopy_path",
            "size_path",
        ] {
            assert!(inner.contains_key(required), "missing key: {required}");
        }
    }

    #[test]
    fn emit_round_trips_to_struct() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();

        let bytes = std::fs::read(tmp.path().join("build_info_uno.json")).unwrap();
        let parsed: std::collections::BTreeMap<String, BuildInfo> =
            serde_json::from_slice(&bytes).unwrap();
        let inner = parsed.get("uno").unwrap();
        assert_eq!(inner, &info);
    }

    #[test]
    fn pick_prog_path_prefers_bin_then_hex_then_elf() {
        let elf = PathBuf::from("/b/firmware.elf");
        let hex = PathBuf::from("/b/firmware.hex");
        let bin = PathBuf::from("/b/firmware.bin");

        assert_eq!(
            pick_prog_path(Some(&elf), Some(&hex), Some(&bin)),
            Some(bin.clone())
        );
        assert_eq!(
            pick_prog_path(Some(&elf), Some(&hex), None),
            Some(hex.clone())
        );
        assert_eq!(pick_prog_path(Some(&elf), None, None), Some(elf.clone()));
        assert_eq!(pick_prog_path(None, None, None), None);
    }

    #[test]
    fn extract_prefixed_handles_empty_and_missing() {
        assert_eq!(extract_prefixed(&[], "-D"), Vec::<String>::new());
        assert_eq!(
            extract_prefixed(&["-Os".to_string()], "-D"),
            Vec::<String>::new()
        );
        // Bare "-D" (no value) is filtered out — empty defines aren't useful.
        assert_eq!(
            extract_prefixed(&["-D".to_string(), "-DX".to_string()], "-D"),
            vec!["X".to_string()]
        );
    }

    // ---- #428: toolchain path derivation + aliases mirror ----

    // Phase 2 of #437 retired the `pj()` Windows-vs-Unix helper: all
    // `BuildInfo` path fields are `NormalizedPath` now, so equality
    // assertions can compare against literal slash-form strings on
    // every platform. The `serialize_*` regression test below pins
    // that contract directly.

    #[test]
    fn derive_gcc_tool_path_avr_prefix() {
        let size = PathBuf::from("/bin/avr-size");
        assert_eq!(
            derive_gcc_tool_path(&size, "nm"),
            PathBuf::from("/bin").join("avr-nm")
        );
        assert_eq!(
            derive_gcc_tool_path(&size, "c++filt"),
            PathBuf::from("/bin").join("avr-c++filt")
        );
        assert_eq!(
            derive_gcc_tool_path(&size, "readelf"),
            PathBuf::from("/bin").join("avr-readelf")
        );
        assert_eq!(
            derive_gcc_tool_path(&size, "objdump"),
            PathBuf::from("/bin").join("avr-objdump")
        );
    }

    #[test]
    fn derive_gcc_tool_path_xtensa_with_exe_suffix() {
        let size = PathBuf::from("C:/toolchain/bin/xtensa-esp32s3-elf-size.exe");
        assert_eq!(
            derive_gcc_tool_path(&size, "nm"),
            PathBuf::from("C:/toolchain/bin").join("xtensa-esp32s3-elf-nm.exe")
        );
        assert_eq!(
            derive_gcc_tool_path(&size, "c++filt"),
            PathBuf::from("C:/toolchain/bin").join("xtensa-esp32s3-elf-c++filt.exe")
        );
    }

    #[test]
    fn derive_gcc_tool_path_bare_size_falls_back_to_bare_target() {
        // PATH-resolved `size` with no prefix.
        let size = PathBuf::from("/usr/bin/size");
        assert_eq!(
            derive_gcc_tool_path(&size, "nm"),
            PathBuf::from("/usr/bin").join("nm")
        );
    }

    #[test]
    fn build_info_populates_new_toolchain_fields() {
        let info = sample_info();
        // GCC convention: /bin/avr-size → /bin/avr-{nm,c++filt,readelf,objdump}.
        // Stored as `NormalizedPath` (#437 Phase 2), so the comparison
        // is platform-stable forward-slash form everywhere — no `pj()`
        // helper needed.
        assert_eq!(info.nm_path, NormalizedPath::new("/bin/avr-nm"));
        assert_eq!(info.cppfilt_path, NormalizedPath::new("/bin/avr-c++filt"));
        assert_eq!(info.readelf_path, NormalizedPath::new("/bin/avr-readelf"));
        assert_eq!(info.objdump_path, NormalizedPath::new("/bin/avr-objdump"));
    }

    #[test]
    fn build_info_aliases_block_mirrors_paths() {
        let info = sample_info();
        // The aliases block carries short PIO-shape keys. Every present
        // long-form path must also appear under its alias key, in
        // `NormalizedPath` slash-form so the values are byte-identical
        // across Linux, macOS, and Windows. See #437 Phase 2 —
        // formerly this test had to use `pj()` to track the platform
        // separator that `Path::join` leaked into the value strings.
        assert_eq!(
            info.aliases.get("gcc"),
            Some(&NormalizedPath::new("/bin/avr-gcc")),
        );
        assert_eq!(
            info.aliases.get("g++"),
            Some(&NormalizedPath::new("/bin/avr-g++")),
        );
        assert_eq!(
            info.aliases.get("ar"),
            Some(&NormalizedPath::new("/bin/avr-ar")),
        );
        assert_eq!(
            info.aliases.get("objcopy"),
            Some(&NormalizedPath::new("/bin/avr-objcopy")),
        );
        assert_eq!(
            info.aliases.get("size"),
            Some(&NormalizedPath::new("/bin/avr-size")),
        );
        assert_eq!(
            info.aliases.get("nm"),
            Some(&NormalizedPath::new("/bin/avr-nm")),
        );
        assert_eq!(
            info.aliases.get("c++filt"),
            Some(&NormalizedPath::new("/bin/avr-c++filt")),
        );
        assert_eq!(
            info.aliases.get("readelf"),
            Some(&NormalizedPath::new("/bin/avr-readelf")),
        );
        assert_eq!(
            info.aliases.get("objdump"),
            Some(&NormalizedPath::new("/bin/avr-objdump")),
        );
    }

    #[test]
    fn build_info_aliases_omit_empty_optional_tools() {
        // ESP8266-style: no ar, no objcopy. Their alias keys must be absent
        // so consumers can rely on `key in aliases` meaning the path is real.
        let info = BuildInfo::new(
            Path::new("/build/firmware.elf"),
            Some(Path::new("/bin/gcc")),
            Some(Path::new("/bin/g++")),
            None,
            None,
            Path::new("/bin/size"),
            vec![],
            vec![],
            vec![],
            vec![],
            "esp8266".to_string(),
            "nodemcuv2".to_string(),
            "nodemcuv2".to_string(),
        );
        assert!(!info.aliases.contains_key("ar"));
        assert!(!info.aliases.contains_key("objcopy"));
        // …but nm / c++filt / readelf / objdump are derived from size_path
        // and always present.
        assert!(info.aliases.contains_key("nm"));
        assert!(info.aliases.contains_key("c++filt"));
        assert!(info.aliases.contains_key("readelf"));
        assert!(info.aliases.contains_key("objdump"));
    }

    /// #428: walking up from an ELF path should find the project's
    /// `build_info.json`. Mirrors the FastLED `_find_build_info()` lookup.
    #[test]
    fn find_build_info_walks_up_from_elf_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let build_dir = project.join(".fbuild").join("build").join("uno");
        std::fs::create_dir_all(&build_dir).unwrap();
        // Emit at the project root (where platformio.ini would live).
        let info = sample_info();
        emit_build_info(project, "uno", &info).unwrap();

        // Search from the ELF directory walks up to the project root.
        let found = find_build_info_near(&build_dir)
            .expect("walk should reach the project root's build_info.json");
        assert_eq!(found, project.join("build_info.json"));
    }

    #[test]
    fn find_build_info_prefers_env_specific_in_same_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        // Only write build_info_uno.json, no generic build_info.json.
        let info = sample_info();
        let outer = BTreeMap::from([("uno".to_string(), info)]);
        let json = serde_json::to_string_pretty(&outer).unwrap();
        std::fs::write(dir.join("build_info_uno.json"), json).unwrap();

        let found = find_build_info_near(dir).expect("should find env-specific file");
        assert_eq!(found.file_name().unwrap(), "build_info_uno.json");
    }

    #[test]
    fn find_build_info_returns_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(find_build_info_near(&nested).is_none());
    }

    #[test]
    fn load_build_info_unwraps_single_env() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();
        let (env, loaded) = load_build_info(&tmp.path().join("build_info_uno.json")).unwrap();
        assert_eq!(env, "uno");
        assert_eq!(loaded, info);
    }

    /// #428: existing PIO consumers (FastLED `ci/util/symbol_analysis.py`)
    /// read tool paths from `board_info["aliases"]["<short>"]`. Make sure
    /// the emitted JSON carries that block.
    #[test]
    fn emit_writes_aliases_into_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();

        let bytes = std::fs::read(tmp.path().join("build_info_uno.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let inner = parsed
            .as_object()
            .unwrap()
            .get("uno")
            .unwrap()
            .as_object()
            .unwrap();
        let aliases = inner
            .get("aliases")
            .expect("aliases block must be emitted")
            .as_object()
            .expect("aliases must be an object");
        // Phase 2 of #437: `NormalizedPath::Serialize` emits forward
        // slashes regardless of host platform, so these expected
        // values are literal slash-form strings — the old `pj()`
        // helper that papered over the cross-platform drift is gone.
        assert_eq!(
            aliases.get("nm").and_then(|v| v.as_str()),
            Some("/bin/avr-nm"),
        );
        assert_eq!(
            aliases.get("c++filt").and_then(|v| v.as_str()),
            Some("/bin/avr-c++filt"),
        );
        assert_eq!(
            aliases.get("readelf").and_then(|v| v.as_str()),
            Some("/bin/avr-readelf"),
        );
        assert_eq!(
            aliases.get("objdump").and_then(|v| v.as_str()),
            Some("/bin/avr-objdump"),
        );
    }
}
