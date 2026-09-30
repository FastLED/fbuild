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

use fbuild_core::path::NormalizedPath;
use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use crate::build_fingerprint::{
    BUILD_FINGERPRINT_VERSION, BinArtifactCache, FileStamp, SizeArtifactCache, load_json, save_json,
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

/// GNU ld matches the SDK linker scripts' section wildcards with `fnmatch`,
/// which under a UTF-8 locale converts every pattern and name to wide
/// characters: a third of an ESP32-S3 link. The C locale matches the same
/// (ASCII) names and produces a byte-identical ELF (FastLED/fbuild#1537).
pub(crate) const LINK_ENV: &[(&str, &str)] = &[("LC_ALL", "C")];

/// Build the argv for an esptool `elf2image` invocation.
///
/// When esptool was provisioned, the standalone binary is invoked directly;
/// otherwise it falls back to an `esptool` on PATH. Shared by the firmware
/// conversion path here and the bootloader conversion path in
/// `orchestrator::boot_artifacts` so both honor the same provisioned tool
/// (FastLED/fbuild#954). The `--chip` flag is a global option and therefore
/// precedes the `elf2image` subcommand, matching the esptool v4/v5 CLI.
#[allow(clippy::too_many_arguments)]
pub(crate) fn esptool_elf2image_argv(
    esptool_bin: Option<&Path>,
    chip: &str,
    flash_mode: &str,
    flash_freq: &str,
    flash_size: &str,
    elf: &str,
    out_bin: &str,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    // pioarduino's older pinned source package installs the v4 `esptool.py`
    // entry point. Its elf2image options use underscores; v5 standalone
    // executables use hyphens. The binary name comes from the exact selected
    // package, so this does not substitute a different esptool release.
    let source_v4 = esptool_bin.is_some_and(|bin| {
        bin.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("esptool.py"))
    });
    let (mode_flag, freq_flag, size_flag) = if source_v4 {
        ("--flash_mode", "--flash_freq", "--flash_size")
    } else {
        ("--flash-mode", "--flash-freq", "--flash-size")
    };
    match esptool_bin {
        Some(bin) => argv.push(bin.to_string_lossy().to_string()),
        None => argv.push("esptool".to_string()),
    }
    argv.extend([
        "--chip".to_string(),
        chip.to_string(),
        "elf2image".to_string(),
        mode_flag.to_string(),
        flash_mode.to_string(),
        freq_flag.to_string(),
        flash_freq.to_string(),
        size_flag.to_string(),
        flash_size.to_string(),
        elf.to_string(),
        "-o".to_string(),
        out_bin.to_string(),
    ]);
    argv
}

/// Build the error message for an esptool `elf2image` spawn failure.
///
/// The two cases are genuinely different faults and used to share one
/// misleading message (FastLED/fbuild#1220):
///
/// * `Some(bin)` — a selected executable, provisioned OR supplied via
///   `FBUILD_ESPTOOL_PATH`, won't launch. Telling the user to
///   `pip install esptool` is wrong either way.
/// * `None` — provisioning already failed (and said so, at error level, with
///   the URL it tried), and the bare-`esptool` PATH fallback found nothing.
///   The actionable fix is the override, not a `pip install` that the daemon's
///   `env_clear`ed PATH may not even see.
pub(crate) fn esptool_spawn_failure_message(esptool_bin: Option<&Path>, error: &str) -> String {
    match esptool_bin {
        Some(bin) => format!(
            "selected esptool executable could not be launched — cannot convert \
             firmware.elf to firmware.bin.\n  \
             executable: {}\n  \
             Set {} to a working esptool to override.\nError: {error}",
            bin.display(),
            fbuild_packages::library::ESPTOOL_PATH_ENV_VAR,
        ),
        None => format!(
            "esptool provisioning failed earlier in this build and the fallback \
             `esptool` on PATH could not be launched either — cannot convert \
             firmware.elf to firmware.bin.\n  \
             See the earlier `esptool provisioning failed` log line for the URL \
             that was tried and the version that was parsed.\n  \
             Set {} to an esptool executable to bypass provisioning (this is the \
             only override that survives the daemon's environment scrub).\nError: {error}",
            fbuild_packages::library::ESPTOOL_PATH_ENV_VAR,
        ),
    }
}

/// ESP32-specific linker using RISC-V or Xtensa GCC as the link driver.
pub struct Esp32Linker {
    gcc_path: NormalizedPath,
    ar_path: NormalizedPath,
    #[allow(dead_code)] // Used later for esptool elf2image
    objcopy_path: NormalizedPath,
    size_path: NormalizedPath,
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
    /// Path to the provisioned standalone esptool binary, if available. `None`
    /// falls back to an `esptool` on PATH. See FastLED/fbuild#954.
    esptool_bin: Option<NormalizedPath>,
    verbose: bool,
    /// The CLI caller's PATH, so the bare-`esptool` fallback resolves
    /// against the caller's environment instead of the daemon's
    /// spawn-time PATH (FastLED/fbuild#1219). `None` = daemon env.
    caller_path: Option<String>,
    /// The app partition's size, which is what the image must fit. `None`
    /// falls back to `max_flash` (FastLED/fbuild#1409).
    app_size_limit: Option<u64>,
}

impl Esp32Linker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: NormalizedPath,
        ar_path: NormalizedPath,
        objcopy_path: NormalizedPath,
        size_path: NormalizedPath,
        mcu_config: Esp32McuConfig,
        sdk_ld_flags: Vec<String>,
        sdk_lib_flags: Vec<String>,
        linker_scripts: LinkerScripts,
        profile: BuildProfile,
        flash_mode: Option<String>,
        flash_freq: &str,
        max_flash: Option<u64>,
        max_ram: Option<u64>,
        esptool_bin: Option<NormalizedPath>,
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
            esptool_bin,
            verbose,
            caller_path: None,
            app_size_limit: None,
        }
    }

    /// Builder: size-check the image against the app partition instead of
    /// the board's flash size. `max_flash` still drives esptool's
    /// `--flash-size`, so it cannot carry this limit.
    pub fn with_app_size_limit(mut self, limit: Option<u64>) -> Self {
        self.app_size_limit = limit;
        self
    }

    fn flash_limit(&self) -> Option<u64> {
        self.app_size_limit.or(self.max_flash)
    }

    /// Builder: forward the CLI caller's PATH to the esptool `elf2image`
    /// spawn so the bare-name fallback resolves against the caller's
    /// environment (FastLED/fbuild#1219).
    pub fn with_caller_path(mut self, caller_path: Option<String>) -> Self {
        self.caller_path = caller_path;
        self
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

        // Keep section-level dead-code elimination enabled even when the SDK
        // supplies a complete `flags/ld_flags` file.  The SDK flags replace
        // the JSON fallback above, and older SDK packages do not all include
        // `--gc-sections`.  This is the important size guard for quick/no-LTO
        // builds: every function/data section can still be removed when it is
        // unreachable from the firmware roots.
        if !flags.iter().any(|flag| flag == "-Wl,--gc-sections") {
            flags.push("-Wl,--gc-sections".to_string());
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

    /// Fingerprint the esptool resolution feeding the BIN cache. A
    /// provisioned absolute path cannot drift → empty (matches serde's
    /// default for pre-existing cache records). A bare `esptool` resolved
    /// against a caller PATH gets a short hash of that PATH so requests
    /// with different caller PATHs never share a cached firmware.bin
    /// (FastLED/fbuild#1238).
    fn esptool_fingerprint(&self) -> String {
        if self.esptool_bin.is_some() {
            return String::new();
        }
        match self.caller_path.as_deref() {
            Some(path) if !path.is_empty() => {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(path.as_bytes());
                digest[..8].iter().map(|b| format!("{b:02x}")).collect()
            }
            _ => String::new(),
        }
    }

    fn current_bin_cache(&self, elf_path: &Path, flash_size: &str) -> Result<BinArtifactCache> {
        Ok(BinArtifactCache {
            version: BUILD_FINGERPRINT_VERSION,
            elf_stamp: FileStamp::from_path(elf_path)?,
            flash_mode: self.flash_mode.clone(),
            flash_freq: self.flash_freq.clone(),
            flash_size: flash_size.to_string(),
            esptool_fingerprint: self.esptool_fingerprint(),
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
        if !self.sdk_lib_flags.iter().any(|flag| flag == "-lstdc++") {
            link_args.extend(self.mcu_config.linker_libs.iter().cloned());
        }
        link_args.extend(extra.libs.iter().cloned());

        link_args.push("-Wl,--end-group".to_string());

        link_args
    }
}

#[async_trait::async_trait]
impl Linker for Esp32Linker {
    async fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "ar").await
    }

    async fn link(
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
        //
        // FastLED/fbuild#809: ESP32 links are the longest legitimate
        // link step in the codebase (LTO + large SDK archive). 5 min
        // is a generous upper bound — anything past that is a wedge.
        let link_timeout = Some(std::time::Duration::from_secs(300));
        let result = if fbuild_core::platform::host::is_windows() {
            let flags_for_rsp: Vec<String> = link_args[1..].to_vec();
            let rsp_dir = output_dir.join("tmp");
            let rsp_path = fbuild_core::response_file::write_response_file(
                &flags_for_rsp,
                &rsp_dir,
                "esp32_link",
            )
            .await?;
            let rsp_args = [link_args[0].as_str(), &format!("@{}", rsp_path.display())];
            run_command(&rsp_args, None, Some(LINK_ENV), link_timeout).await?
        } else {
            let args_ref: Vec<&str> = link_args.iter().map(|s| s.as_str()).collect();
            run_command(&args_ref, None, Some(LINK_ENV), link_timeout).await?
        };

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "ESP32 link failed:\n{}",
                result.stderr
            )));
        }

        Ok(elf_path)
    }

    async fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf> {
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
        // Prefer the provisioned standalone esptool binary; fall back to an
        // `esptool` on PATH (FastLED/fbuild#954).
        let argv = esptool_elf2image_argv(
            self.esptool_bin.as_deref(),
            chip,
            &self.flash_mode,
            &self.flash_freq,
            &flash_size,
            &elf_str,
            &bin_str,
        );
        let args: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();

        tracing::info!("elf2image: {}", argv.join(" "));

        // FastLED/fbuild#1219: resolve/run esptool under the caller's PATH.
        let env: Option<Vec<(&str, &str)>> = self.caller_path.as_deref().map(|p| vec![("PATH", p)]);
        match run_command(
            &args,
            None,
            env.as_deref(),
            Some(std::time::Duration::from_secs(60)),
        )
        .await
        {
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
            Err(e) => Err(fbuild_core::FbuildError::BuildFailed(
                esptool_spawn_failure_message(self.esptool_bin.as_deref(), &e.to_string()),
            )),
        }
    }

    fn size_tool_path(&self) -> &Path {
        self.size_path.as_path()
    }

    fn ar_tool_path(&self) -> Option<&Path> {
        Some(self.ar_path.as_path())
    }

    fn objcopy_tool_path(&self) -> Option<&Path> {
        Some(self.objcopy_path.as_path())
    }

    fn link_driver_path(&self) -> Option<&Path> {
        Some(self.gcc_path.as_path())
    }

    async fn report_size(&self, elf_path: &Path) -> Result<SizeInfo> {
        if let Some(mut size_info) = self.load_cached_size(elf_path) {
            tracing::info!("size: firmware.elf is unchanged, reusing cached size report");
            // The limits come from config, which can change without a relink.
            size_info.max_flash = self.flash_limit();
            size_info.max_ram = self.max_ram;
            return Ok(size_info);
        }

        let size_info = super::size_report::esp32_report_size(
            &self.size_path,
            elf_path,
            self.flash_limit(),
            self.max_ram,
        )
        .await?;
        self.save_size_cache(elf_path, &size_info);
        Ok(size_info)
    }
}

#[cfg(test)]
#[path = "esp32_linker_tests.rs"]
mod tests;
