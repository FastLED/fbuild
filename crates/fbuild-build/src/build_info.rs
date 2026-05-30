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

use std::path::{Path, PathBuf};

use fbuild_core::Result;
use serde::{Deserialize, Serialize};

/// PlatformIO-shape build metadata for one environment.
///
/// All paths are emitted as strings (matching `pio project metadata`
/// output); empty / missing toolchain entries are emitted as empty strings
/// so consumers that do `Path(board_info["objcopy_path"])` never KeyError.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    /// Absolute path to the final firmware/program file (`.elf` if no
    /// `.hex`/`.bin` was produced, otherwise the converted firmware).
    pub prog_path: String,
    /// Absolute path to the C compiler (`gcc`).
    pub cc_path: String,
    /// Absolute path to the C++ compiler (`g++`).
    pub cxx_path: String,
    /// Absolute path to `ar` (empty when the linker doesn't expose it).
    pub ar_path: String,
    /// Absolute path to `objcopy` (empty when the platform has no objcopy step,
    /// e.g. ESP8266 which produces ELF directly).
    pub objcopy_path: String,
    /// Absolute path to `size`.
    pub size_path: String,
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
}

impl BuildInfo {
    /// Construct a `BuildInfo` from already-collected pieces. Splits
    /// `-D` defines and `-I` includes out of `cxx_flags` (matching
    /// PlatformIO's metadata-emitter convention) without removing them
    /// from `cxx_flags` itself.
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
        Self {
            prog_path: path_to_string(Some(prog_path)),
            cc_path: path_to_string(cc_path),
            cxx_path: path_to_string(cxx_path),
            ar_path: path_to_string(ar_path),
            objcopy_path: path_to_string(objcopy_path),
            size_path: path_to_string(Some(size_path)),
            cc_flags,
            cxx_flags,
            link_flags,
            defines,
            includes,
            libs,
            platform,
            board,
            env,
        }
    }
}

fn path_to_string(p: Option<&Path>) -> String {
    p.map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
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
pub fn emit_build_info(project_dir: &Path, env_name: &str, info: &BuildInfo) -> Result<()> {
    let outer = std::collections::BTreeMap::from([(env_name.to_string(), info.clone())]);
    let json = serde_json::to_string_pretty(&outer).map_err(|e| {
        fbuild_core::FbuildError::BuildFailed(format!("failed to serialize build_info: {e}"))
    })?;

    let env_specific = project_dir.join(format!("build_info_{env_name}.json"));
    let generic = project_dir.join("build_info.json");

    for path in [&env_specific, &generic] {
        if let Err(e) = std::fs::write(path, &json) {
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
        assert_eq!(info.ar_path, "");
        assert_eq!(info.objcopy_path, "");
    }

    #[test]
    fn emit_writes_both_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let info = sample_info();
        emit_build_info(tmp.path(), "uno", &info).unwrap();
        assert!(tmp.path().join("build_info_uno.json").exists());
        assert!(tmp.path().join("build_info.json").exists());
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
}
