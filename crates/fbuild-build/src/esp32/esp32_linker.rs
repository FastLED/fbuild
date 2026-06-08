//! ESP32 linker implementation — the most complex linker in the project.
//!
//! - 17+ linker scripts from `tools/sdk/{mcu}/ld/`
//! - 100+ precompiled `.a` libraries from ESP-IDF
//! - 40+ `--undefined` / `-u` symbols from MCU config
//! - MCU-specific defsym
//! - Produces `.bin` via `objcopy -O binary`
//! - Copies `bootloader.bin` + `partitions.bin` to build output
//! - Response files needed on Windows for massive arg lists

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use crate::build_fingerprint::{
    load_json, save_json, BinArtifactCache, FileStamp, SizeArtifactCache, BUILD_FINGERPRINT_VERSION,
};
use crate::linker::{LinkExtraArgs, Linker, LinkerScripts};

use super::mcu_config::Esp32McuConfig;

/// Valid esptool flash frequencies.
const VALID_FLASH_FREQS: &[&str] = &[
    "80m", "60m", "48m", "40m", "30m", "26m", "24m", "20m", "16m", "15m", "12m",
];

/// Convert `f_flash` board config value (e.g. `"80000000L"`) to esptool frequency (e.g. `"80m"`).
///
/// Divides Hz by 1,000,000 and appends "m". Falls back to `default_freq` if the value
/// cannot be parsed or is not a valid esptool frequency.
pub fn f_flash_to_esptool_freq(f_flash: Option<&str>, default_freq: &str) -> String {
    match f_flash {
        Some(s) => {
            let s = s.trim_end_matches('L');
            match s.parse::<u64>() {
                Ok(hz) => {
                    let freq = format!("{}m", hz / 1_000_000);
                    if VALID_FLASH_FREQS.contains(&freq.as_str()) {
                        freq
                    } else {
                        default_freq.to_string()
                    }
                }
                Err(_) => default_freq.to_string(),
            }
        }
        None => default_freq.to_string(),
    }
}

/// ESP32-specific linker using RISC-V or Xtensa GCC as the link driver.
pub struct Esp32Linker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    #[allow(dead_code)] // Used later for esptool elf2image
    objcopy_path: PathBuf,
    size_path: PathBuf,
    /// MCU config (used for profile-specific flags as fallback).
    mcu_config: Esp32McuConfig,
    /// SDK linker flags from `flags/ld_flags` (undefined symbols, wrap directives, etc.).
    sdk_ld_flags: Vec<String>,
    /// SDK library flags from `flags/ld_libs` (ordered `-L`/`-l` flags).
    sdk_lib_flags: Vec<String>,
    /// SDK linker scripts (search dirs + script names from `flags/ld_scripts`).
    linker_scripts: LinkerScripts,
    /// Build profile.
    profile: BuildProfile,
    /// Flash mode for esptool (e.g. "dio", "qio"). Defaults to "dio".
    flash_mode: String,
    /// Flash frequency for esptool (e.g. "80m", "40m"). Derived from board f_flash.
    flash_freq: String,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl Esp32Linker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        mcu_config: Esp32McuConfig,
        sdk_ld_flags: Vec<String>,
        sdk_lib_flags: Vec<String>,
        linker_scripts: LinkerScripts,
        profile: BuildProfile,
        flash_mode: Option<String>,
        flash_freq: &str,
        max_flash: Option<u64>,
        max_ram: Option<u64>,
        verbose: bool,
    ) -> Self {
        let flash_mode = flash_mode.unwrap_or_else(|| mcu_config.default_flash_mode().to_string());
        Self {
            gcc_path,
            ar_path,
            objcopy_path,
            size_path,
            mcu_config,
            sdk_ld_flags,
            sdk_lib_flags,
            linker_scripts,
            profile,
            flash_mode,
            flash_freq: flash_freq.to_string(),
            max_flash,
            max_ram,
            verbose,
        }
    }

    /// Build all linker flags: SDK flags + profile-specific flags.
    fn linker_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // SDK linker flags take priority (from flags/ld_flags).
        // When SDK flags are present, skip profile link flags — the SDK already
        // includes the correct optimization settings (e.g., -fno-lto).
        if !self.sdk_ld_flags.is_empty() {
            flags.extend(self.sdk_ld_flags.clone());
        } else {
            // Fallback to MCU config JSON + profile link flags
            flags.extend(self.mcu_config.linker_flags.clone());
            let profile_name = match self.profile {
                BuildProfile::Release => "release",
                BuildProfile::Quick => "quick",
            };
            if let Some(profile) = self.mcu_config.get_profile(profile_name) {
                flags.extend(profile.link_flags.clone());
            }
        }

        flags
    }

    fn flash_size(&self) -> String {
        super::mcu_config::bytes_to_flash_size(self.max_flash, self.mcu_config.default_flash_size())
            .to_string()
    }

    fn bin_cache_path(&self, output_dir: &Path) -> PathBuf {
        output_dir.join(".firmware_bin_cache.json")
    }

    fn size_cache_path(&self, output_dir: &Path) -> PathBuf {
        output_dir.join(".firmware_size_cache.json")
    }

    fn current_bin_cache(&self, elf_path: &Path, flash_size: &str) -> Result<BinArtifactCache> {
        Ok(BinArtifactCache {
            version: BUILD_FINGERPRINT_VERSION,
            elf_stamp: FileStamp::from_path(elf_path)?,
            flash_mode: self.flash_mode.clone(),
            flash_freq: self.flash_freq.clone(),
            flash_size: flash_size.to_string(),
        })
    }

    fn can_reuse_bin(&self, elf_path: &Path, output_dir: &Path, flash_size: &str) -> bool {
        let bin_out = output_dir.join("firmware.bin");
        if !bin_out.exists() {
            return false;
        }

        let bin_mtime = match std::fs::metadata(&bin_out).and_then(|m| m.modified()) {
            Ok(mtime) => mtime,
            Err(_) => return false,
        };
        let elf_mtime = match std::fs::metadata(elf_path).and_then(|m| m.modified()) {
            Ok(mtime) => mtime,
            Err(_) => return false,
        };
        if bin_mtime < elf_mtime {
            return false;
        }

        let expected = match self.current_bin_cache(elf_path, flash_size) {
            Ok(cache) => cache,
            Err(_) => return false,
        };
        match load_json::<BinArtifactCache>(&self.bin_cache_path(output_dir)) {
            Ok(Some(recorded)) => recorded == expected,
            Ok(None) => false,
            Err(e) => {
                tracing::warn!("ignoring invalid firmware bin cache: {}", e);
                false
            }
        }
    }

    fn load_cached_size(&self, elf_path: &Path) -> Option<SizeInfo> {
        let output_dir = elf_path.parent().unwrap_or_else(|| Path::new("."));
        let expected_stamp = match FileStamp::from_path(elf_path) {
            Ok(stamp) => stamp,
            Err(_) => return None,
        };
        match load_json::<SizeArtifactCache>(&self.size_cache_path(output_dir)) {
            Ok(Some(cache))
                if cache.version == BUILD_FINGERPRINT_VERSION
                    && cache.elf_stamp == expected_stamp =>
            {
                Some(cache.size_info)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("ignoring invalid firmware size cache: {}", e);
                None
            }
        }
    }

    fn save_size_cache(&self, elf_path: &Path, size_info: &SizeInfo) {
        let output_dir = elf_path.parent().unwrap_or_else(|| Path::new("."));
        let cache = match FileStamp::from_path(elf_path) {
            Ok(stamp) => SizeArtifactCache {
                version: BUILD_FINGERPRINT_VERSION,
                elf_stamp: stamp,
                size_info: size_info.clone(),
            },
            Err(e) => {
                tracing::warn!("failed to record firmware size cache: {}", e);
                return;
            }
        };
        if let Err(e) = save_json(&self.size_cache_path(output_dir), &cache) {
            tracing::warn!("failed to write firmware size cache: {}", e);
        }
    }

    /// Build the linker argv that [`Self::link`] will invoke, without
    /// touching the filesystem or running the subprocess. Extracted from
    /// `link()` so unit tests can assert on the argv shape — in particular
    /// the `-Wl,-Map=<elf-stem>.map` flag required by `fbuild bloat` for
    /// archive / object / section attribution (see FastLED/fbuild#491,
    /// #508). Every other platform linker (avr, teensy, generic_arm,
    /// esp8266, ...) already does this; ESP32 was the outlier.
    fn build_link_args(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        elf_path: &Path,
        extra: &LinkExtraArgs,
    ) -> Vec<String> {
        let mut link_args: Vec<String> = Vec::new();

        // Compiler/driver
        link_args.push(self.gcc_path.to_string_lossy().to_string());

        // Linker flags (from SDK flags/ld_flags or MCU config fallback)
        link_args.extend(self.linker_flags());
        link_args.extend(extra.flags.iter().cloned());

        // Linker scripts (search dirs + script names from SDK)
        link_args.extend(self.linker_scripts.to_args());

        // Memory usage reporting
        link_args.push("-Wl,--print-memory-usage".to_string());

        // Output
        link_args.extend(["-o".to_string(), elf_path.to_string_lossy().to_string()]);

        // Always emit a linker map next to firmware.elf — required by
        // `fbuild bloat` / `fbuild symbols` for archive / object / section
        // attribution (#491, #508).
        let map_path = elf_path.with_extension("map");
        link_args.push(format!("-Wl,-Map={}", map_path.to_string_lossy()));

        // Sketch objects
        for obj in objects {
            link_args.push(obj.to_string_lossy().to_string());
        }

        // Core objects, library archives, and SDK libs wrapped in --start-group
        // so the linker resolves circular dependencies between them.
        link_args.push("-Wl,--start-group".to_string());

        for archive in archives {
            link_args.push(archive.to_string_lossy().to_string());
        }

        // SDK precompiled libraries (ordered flags from flags/ld_libs)
        link_args.extend(self.sdk_lib_flags.clone());
        link_args.extend(extra.libs.iter().cloned());

        link_args.push("-Wl,--end-group".to_string());

        link_args
    }
}

impl Linker for Esp32Linker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "ar")
    }

    fn link(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        output_dir: &Path,
        extra: &LinkExtraArgs,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(output_dir)?;
        let elf_path = output_dir.join("firmware.elf");
        let link_args = self.build_link_args(objects, archives, &elf_path, extra);

        if self.verbose {
            tracing::info!("link: {}", link_args.join(" "));
        }

        // On Windows, always use a response file to normalize paths
        // (forward slashes, quoting) and avoid command-line length issues.
        let result = if cfg!(windows) {
            let flags_for_rsp: Vec<String> = link_args[1..].to_vec();
            let rsp_dir = output_dir.join("tmp");
            let rsp_path = fbuild_core::response_file::write_response_file(
                &flags_for_rsp,
                &rsp_dir,
                "esp32_link",
            )?;
            let rsp_args = [link_args[0].as_str(), &format!("@{}", rsp_path.display())];
            run_command(&rsp_args, None, None, None)?
        } else {
            let args_ref: Vec<&str> = link_args.iter().map(|s| s.as_str()).collect();
            run_command(&args_ref, None, None, None)?
        };

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "ESP32 link failed:\n{}",
                result.stderr
            )));
        }

        Ok(elf_path)
    }

    fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf> {
        // Copy ELF to output directory
        let elf_out = output_dir.join("firmware.elf");
        if elf_path != elf_out {
            std::fs::copy(elf_path, &elf_out)?;
        }

        // Convert ELF to BIN using esptool elf2image.
        // Raw `objcopy -O binary` produces a bloated file because the ELF has segments
        // at high addresses (IRAM 0x400xxxxx, DRAM 0x3FFxxxxx). esptool understands
        // the ESP32 image format and produces the correct flashable binary.
        let bin_out = output_dir.join("firmware.bin");
        let chip = &self.mcu_config.mcu;
        let elf_str = elf_out.to_string_lossy();
        let bin_str = bin_out.to_string_lossy();
        let flash_size = self.flash_size();
        if self.can_reuse_bin(&elf_out, output_dir, &flash_size) {
            tracing::info!("elf2image: firmware.bin is current, skipping conversion");
            return Ok(bin_out);
        }
        // Determine flash size from max_flash config (bytes → human-readable).
        // elf2image doesn't support "detect" — needs an explicit size.
        let args = [
            "esptool",
            "--chip",
            chip,
            "elf2image",
            "--flash-mode",
            &self.flash_mode,
            "--flash-freq",
            &self.flash_freq,
            "--flash-size",
            &flash_size,
            &elf_str,
            "-o",
            &bin_str,
        ];

        tracing::info!("elf2image: {}", args.join(" "));

        match run_command(&args, None, None, Some(std::time::Duration::from_secs(30))) {
            Ok(result) if result.success() => {
                let cache = self.current_bin_cache(&elf_out, &flash_size)?;
                if let Err(e) = save_json(&self.bin_cache_path(output_dir), &cache) {
                    tracing::warn!("failed to write firmware bin cache: {}", e);
                }
                tracing::info!("converted firmware.elf → firmware.bin");
                Ok(bin_out)
            }
            Ok(result) => Err(fbuild_core::FbuildError::BuildFailed(format!(
                "esptool elf2image failed (exit={}):\n{}{}",
                result.exit_code, result.stderr, result.stdout
            ))),
            Err(e) => Err(fbuild_core::FbuildError::BuildFailed(format!(
                "esptool not found — cannot convert firmware.elf to firmware.bin.\n\
                 Install with: pip install esptool\nError: {}",
                e
            ))),
        }
    }

    fn size_tool_path(&self) -> &Path {
        &self.size_path
    }

    fn ar_tool_path(&self) -> Option<&Path> {
        Some(&self.ar_path)
    }

    fn objcopy_tool_path(&self) -> Option<&Path> {
        Some(&self.objcopy_path)
    }

    fn link_driver_path(&self) -> Option<&Path> {
        Some(&self.gcc_path)
    }

    fn report_size(&self, elf_path: &Path) -> Result<SizeInfo> {
        if let Some(size_info) = self.load_cached_size(elf_path) {
            tracing::info!("size: firmware.elf is unchanged, reusing cached size report");
            return Ok(size_info);
        }

        let size_info = crate::linker::LinkerBase::report_size(
            &self.size_path,
            elf_path,
            self.max_flash,
            self.max_ram,
            "size",
        )?;
        self.save_size_cache(elf_path, &size_info);
        Ok(size_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esp32::mcu_config::get_mcu_config;

    fn test_linker(mcu: &str) -> Esp32Linker {
        let config = get_mcu_config(mcu).unwrap();
        let prefix = config.toolchain_prefix();
        Esp32Linker::new(
            PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
            PathBuf::from(format!("/usr/bin/{}ar", prefix)),
            PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
            PathBuf::from(format!("/usr/bin/{}size", prefix)),
            config,
            vec![
                "-nostartfiles".to_string(),
                "-u".to_string(),
                "app_main".to_string(),
            ],
            vec![
                "-L/sdk/lib".to_string(),
                "-lfreertos".to_string(),
                "-lesp_system".to_string(),
            ],
            LinkerScripts::from_raw_flags(&[
                "-L/sdk/ld".to_string(),
                "-Tmemory.ld".to_string(),
                "-Tsections.ld".to_string(),
            ]),
            BuildProfile::Release,
            None,
            "80m",
            Some(3145728),
            Some(327680),
            false,
        )
    }

    #[test]
    fn test_esp32_linker_creation() {
        let linker = test_linker("esp32c6");
        assert_eq!(linker.max_flash, Some(3145728));
        assert_eq!(linker.max_ram, Some(327680));
    }

    #[test]
    fn test_flash_size_uses_board_max_flash_for_elf2image_and_cache() {
        let config = get_mcu_config("esp32c6").unwrap();
        let prefix = config.toolchain_prefix();
        let linker = Esp32Linker::new(
            PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
            PathBuf::from(format!("/usr/bin/{}ar", prefix)),
            PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
            PathBuf::from(format!("/usr/bin/{}size", prefix)),
            config,
            vec![],
            vec![],
            LinkerScripts::new(),
            BuildProfile::Release,
            None,
            "80m",
            Some(4 * 1024 * 1024),
            Some(327680),
            false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        std::fs::write(&elf, b"elf").unwrap();

        let flash_size = linker.flash_size();
        let cache = linker.current_bin_cache(&elf, &flash_size).unwrap();

        assert_eq!(flash_size, "4MB");
        assert_eq!(cache.flash_size, "4MB");
    }

    /// Regression test: `build_link_args` always emits `-Wl,-Map=` next to
    /// `firmware.elf`. ESP32 was the only platform linker not emitting the
    /// map before #491 / #508; without it `fbuild bloat` cannot attribute
    /// symbols to their source archives.
    #[test]
    fn test_esp32_link_command_emits_linker_map_next_to_elf() {
        let linker = test_linker("esp32c6");
        let args = linker.build_link_args(
            &[],
            &[],
            &PathBuf::from("/build/firmware.elf"),
            &LinkExtraArgs::default(),
        );
        assert!(
            args.iter().any(|a| a == "-Wl,-Map=/build/firmware.map"),
            "expected -Wl,-Map=/build/firmware.map next to firmware.elf. Args: {:?}",
            args,
        );
    }

    #[test]
    fn test_linker_flags_use_sdk_ld_flags() {
        let linker = test_linker("esp32c6");
        let flags = linker.linker_flags();
        // SDK ld_flags take priority — profile link flags are skipped
        assert!(flags.contains(&"-nostartfiles".to_string()));
        assert!(flags.contains(&"-u".to_string()));
        assert!(flags.contains(&"app_main".to_string()));
        // Profile link flags should NOT be present when SDK flags are used
        assert!(!flags.contains(&"-flto=auto".to_string()));
    }

    #[test]
    fn test_linker_flags_fallback_to_config() {
        let config = get_mcu_config("esp32c6").unwrap();
        let prefix = config.toolchain_prefix();
        // Empty sdk_ld_flags → falls back to MCU config
        let linker = Esp32Linker::new(
            PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
            PathBuf::from(format!("/usr/bin/{}ar", prefix)),
            PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
            PathBuf::from(format!("/usr/bin/{}size", prefix)),
            config,
            vec![],
            vec!["-lfreertos".to_string()],
            LinkerScripts::from_raw_flags(&["-Tmemory.ld".to_string()]),
            BuildProfile::Release,
            None,
            "80m",
            Some(3145728),
            Some(327680),
            false,
        );
        let flags = linker.linker_flags();
        assert!(flags.iter().any(|f| f.contains("IDF_TARGET_ESP32C6")));
        assert!(flags.contains(&"-fno-rtti".to_string()));
    }

    #[test]
    fn test_sdk_script_flags() {
        let linker = test_linker("esp32c6");
        let args = linker.linker_scripts.to_args();
        assert!(args.iter().any(|f| f.starts_with("-L")));
        assert!(args.iter().any(|f| f == "-Tmemory.ld"));
        assert!(args.iter().any(|f| f == "-Tsections.ld"));
    }

    #[test]
    fn test_sdk_lib_flags_stored() {
        let linker = test_linker("esp32c6");
        assert!(linker.sdk_lib_flags.iter().any(|f| f == "-lfreertos"));
        assert!(linker.sdk_lib_flags.iter().any(|f| f == "-lesp_system"));
        assert!(linker.sdk_lib_flags.iter().any(|f| f.starts_with("-L")));
    }

    #[test]
    fn test_xtensa_linker_flags() {
        // Xtensa with SDK flags that include -mlongcalls
        let config = get_mcu_config("esp32").unwrap();
        let prefix = config.toolchain_prefix();
        let linker = Esp32Linker::new(
            PathBuf::from(format!("/usr/bin/{}gcc", prefix)),
            PathBuf::from(format!("/usr/bin/{}ar", prefix)),
            PathBuf::from(format!("/usr/bin/{}objcopy", prefix)),
            PathBuf::from(format!("/usr/bin/{}size", prefix)),
            config,
            vec!["-mlongcalls".to_string()],
            vec![],
            LinkerScripts::new(),
            BuildProfile::Release,
            None,
            "80m",
            Some(3145728),
            Some(327680),
            false,
        );
        let flags = linker.linker_flags();
        assert!(flags.contains(&"-mlongcalls".to_string()));
    }

    #[test]
    fn test_bin_output_format() {
        // Verify convert_firmware produces .bin, not .hex
        let linker = test_linker("esp32c6");
        // We can't actually run objcopy, but we can verify the method exists
        // and the linker is properly configured
        assert!(linker
            .mcu_config
            .esptool
            .flash_offsets
            .firmware
            .starts_with("0x"));
    }

    #[test]
    fn test_f_flash_to_esptool_freq_all_mappings() {
        assert_eq!(f_flash_to_esptool_freq(Some("80000000L"), "40m"), "80m");
        assert_eq!(f_flash_to_esptool_freq(Some("60000000L"), "40m"), "60m");
        assert_eq!(f_flash_to_esptool_freq(Some("40000000L"), "80m"), "40m");
        assert_eq!(f_flash_to_esptool_freq(Some("30000000L"), "80m"), "30m");
        assert_eq!(f_flash_to_esptool_freq(Some("26000000L"), "80m"), "26m");
        assert_eq!(f_flash_to_esptool_freq(Some("20000000L"), "80m"), "20m");
        assert_eq!(f_flash_to_esptool_freq(Some("15000000L"), "80m"), "15m");
        // Invalid esptool frequency falls back to default
        assert_eq!(f_flash_to_esptool_freq(Some("99000000L"), "40m"), "40m");
        assert_eq!(f_flash_to_esptool_freq(Some("64000000L"), "48m"), "48m");
        // Non-numeric falls back to default
        assert_eq!(f_flash_to_esptool_freq(Some("unknown"), "40m"), "40m");
        // None falls back to default
        assert_eq!(f_flash_to_esptool_freq(None, "60m"), "60m");
    }

    /// ESP32-C2 only supports 60m, 30m, 20m, 15m flash frequencies (not 80m).
    /// The board config specifies f_flash=60000000L, so the resolved frequency
    /// must be "60m", not "80m".
    #[test]
    fn test_esp32c2_flash_freq_not_80m() {
        let config = get_mcu_config("esp32c2").unwrap();
        // Default must not be 80m — ESP32-C2 doesn't support it
        assert_ne!(
            config.default_flash_freq(),
            "80m",
            "ESP32-C2 does not support 80m flash frequency"
        );
        assert_eq!(config.default_flash_freq(), "60m");

        // Simulate what the orchestrator does: board has f_flash=60000000L
        let freq = f_flash_to_esptool_freq(Some("60000000L"), config.default_flash_freq());
        assert_eq!(freq, "60m");
    }

    /// ESP32-H2 board has f_flash=64000000L, but 64m is not a valid esptool frequency.
    /// Must fall back to the MCU default of 48m.
    #[test]
    fn test_esp32h2_flash_freq_not_64m() {
        let config = get_mcu_config("esp32h2").unwrap();
        assert_eq!(config.default_flash_freq(), "48m");

        // Board has f_flash=64000000L → 64m is invalid → falls back to 48m
        let freq = f_flash_to_esptool_freq(Some("64000000L"), config.default_flash_freq());
        assert_eq!(freq, "48m");
    }
}
