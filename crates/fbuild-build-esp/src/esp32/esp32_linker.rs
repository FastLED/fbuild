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
    match esptool_bin {
        Some(bin) => argv.push(bin.to_string_lossy().to_string()),
        None => argv.push("esptool".to_string()),
    }
    argv.extend([
        "--chip".to_string(),
        chip.to_string(),
        "elf2image".to_string(),
        "--flash-mode".to_string(),
        flash_mode.to_string(),
        "--flash-freq".to_string(),
        flash_freq.to_string(),
        "--flash-size".to_string(),
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
    /// Path to the provisioned standalone esptool binary, if available. `None`
    /// falls back to an `esptool` on PATH. See FastLED/fbuild#954.
    esptool_bin: Option<PathBuf>,
    verbose: bool,
    /// The CLI caller's PATH, so the bare-`esptool` fallback resolves
    /// against the caller's environment instead of the daemon's
    /// spawn-time PATH (FastLED/fbuild#1219). `None` = daemon env.
    caller_path: Option<String>,
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
        esptool_bin: Option<PathBuf>,
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
        }
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
        let result = if cfg!(windows) {
            let flags_for_rsp: Vec<String> = link_args[1..].to_vec();
            let rsp_dir = output_dir.join("tmp");
            let rsp_path = fbuild_core::response_file::write_response_file(
                &flags_for_rsp,
                &rsp_dir,
                "esp32_link",
            )
            .await?;
            let rsp_args = [link_args[0].as_str(), &format!("@{}", rsp_path.display())];
            run_command(&rsp_args, None, None, link_timeout).await?
        } else {
            let args_ref: Vec<&str> = link_args.iter().map(|s| s.as_str()).collect();
            run_command(&args_ref, None, None, link_timeout).await?
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

    async fn report_size(&self, elf_path: &Path) -> Result<SizeInfo> {
        if let Some(size_info) = self.load_cached_size(elf_path) {
            tracing::info!("size: firmware.elf is unchanged, reusing cached size report");
            return Ok(size_info);
        }

        let size_info = esp32_report_size(
            &self.size_path,
            elf_path,
            self.max_flash,
            self.max_ram,
        )
        .await?;
        self.save_size_cache(elf_path, &size_info);
        Ok(size_info)
    }
}

/// Run `size -A` (SysV format) for ESP32 targets instead of the default
/// Berkeley format. SysV lists every section individually so flash-resident
/// sections (`.flash.*`, `.rodata`) stay out of the RAM total.
///
/// The Berkeley format lumps `.flash.rodata` into the `data` column
/// alongside `.dram0.data`, inflating the RAM figure — for ESP32-C6
/// this can report 602% RAM usage (FastLED/fbuild#1261).
async fn esp32_report_size(
    size_path: &Path,
    elf_path: &Path,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> fbuild_core::Result<fbuild_core::SizeInfo> {
    use fbuild_core::subprocess::run_command;

    let args = [
        size_path.to_string_lossy().to_string(),
        "-A".to_string(),
        elf_path.to_string_lossy().to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = run_command(
        &args_ref,
        None,
        None,
        Some(std::time::Duration::from_secs(15)),
    )
    .await?;

    if !result.success() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "size -A failed: {}",
            result.stderr
        )));
    }

    parse_esp32_size_output(&result.stdout, max_flash, max_ram).ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "failed to parse ESP32 size -A output:\n{}",
            result.stdout
        ))
    })
}

/// Parse `size -A` (SysV format) output for ESP32 targets.
///
/// SysV format lists every section individually:
/// ```text
/// section             size         addr
/// .flash.text        89012    0x42000020
/// .flash.rodata      45678    0x42015c34
/// .dram0.data         1234    0x3fc80000
/// .dram0.bss          5678    0x3fc81234
/// .iram0.text          789    0x40800000
/// Total              142391
/// ```
///
/// Classification:
/// - Flash: `.flash.*`, `.rodata*`, `.text*` (flash-mapped text)
/// - RAM:   `.dram0.*`, `.data*`, `.bss*`
///
/// Falls back to the standard Berkeley parser when no ESP32-prefixed
/// sections are detected (non-ESP32 targets sharing this code path).
fn parse_esp32_size_output(
    output: &str,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> Option<fbuild_core::SizeInfo> {
    let mut flash: u64 = 0;
    let mut ram_data: u64 = 0;
    let mut ram_bss: u64 = 0;
    let mut has_esp_sections = false;

    for line in output.lines() {
        // Skip the header line and "Total" line
        if line.starts_with("section") || line.starts_with("Total") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let section = parts[0];
        let Ok(size) = parts[1].parse::<u64>() else {
            continue;
        };

        if section.starts_with(".flash.") || section.starts_with(".rodata") {
            flash += size;
            has_esp_sections = true;
        } else if section.starts_with(".dram0.") || section.starts_with(".dram.") {
            // .dram0.data is initialized RAM, .dram0.bss is zeroed RAM
            if section.ends_with(".bss") || section.contains(".bss") {
                ram_bss += size;
            } else {
                ram_data += size;
            }
            has_esp_sections = true;
        } else if section.starts_with(".iram0.") || section.starts_with(".iram.") {
            // Instruction RAM — cached flash on most ESP32 variants
            flash += size;
            has_esp_sections = true;
        } else if section == ".text" {
            flash += size;
        } else if section == ".data" {
            ram_data += size;
        } else if section == ".bss" {
            ram_bss += size;
        }
    }

    if !has_esp_sections {
        return None;
    }

    Some(fbuild_core::SizeInfo {
        text: flash,
        data: ram_data,
        bss: ram_bss,
        total_flash: flash + ram_data,
        total_ram: ram_data + ram_bss,
        max_flash,
        max_ram,
    })
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
            None,
            false,
        )
    }

    #[test]
    fn test_esp32_linker_creation() {
        let linker = test_linker("esp32c6");
        assert_eq!(linker.max_flash, Some(3145728));
        assert_eq!(linker.max_ram, Some(327680));
    }

    fn test_linker_with(esptool_bin: Option<PathBuf>, caller_path: Option<String>) -> Esp32Linker {
        let mut linker = test_linker("esp32c6");
        linker.esptool_bin = esptool_bin;
        linker.caller_path = caller_path;
        linker
    }

    /// FastLED/fbuild#1238: with a bare-name esptool, two requests with
    /// different caller PATHs may resolve different esptool binaries —
    /// they must never share a cached firmware.bin.
    #[test]
    fn bare_name_esptool_bin_reuse_is_keyed_by_caller_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        std::fs::write(&elf, b"elf").unwrap();

        let linker_a = test_linker_with(None, Some("C:\\venv-a\\Scripts".to_string()));
        let flash_size = linker_a.flash_size();

        // Simulate a successful conversion by linker A.
        std::fs::write(tmp.path().join("firmware.bin"), b"bin").unwrap();
        let cache = linker_a.current_bin_cache(&elf, &flash_size).unwrap();
        save_json(&linker_a.bin_cache_path(tmp.path()), &cache).unwrap();

        assert!(
            linker_a.can_reuse_bin(&elf, tmp.path(), &flash_size),
            "same caller PATH must reuse the cached bin"
        );

        let linker_b = test_linker_with(None, Some("C:\\venv-b\\Scripts".to_string()));
        assert!(
            !linker_b.can_reuse_bin(&elf, tmp.path(), &flash_size),
            "a different caller PATH must not reuse a bin produced by another PATH's esptool"
        );

        let linker_none = test_linker_with(None, None);
        assert!(
            !linker_none.can_reuse_bin(&elf, tmp.path(), &flash_size),
            "no caller PATH (daemon-ambient resolution) must not reuse a caller-PATH bin"
        );
    }

    /// Provisioned absolute-path esptool cannot drift with the caller's
    /// PATH — caching must behave exactly as before, including across
    /// requests with different caller PATHs.
    #[test]
    fn absolute_esptool_bin_reuse_ignores_caller_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        std::fs::write(&elf, b"elf").unwrap();

        let esptool = PathBuf::from("C:\\tools\\esptool.exe");
        let linker_a = test_linker_with(Some(esptool.clone()), Some("C:\\venv-a".to_string()));
        let flash_size = linker_a.flash_size();

        std::fs::write(tmp.path().join("firmware.bin"), b"bin").unwrap();
        let cache = linker_a.current_bin_cache(&elf, &flash_size).unwrap();
        assert!(
            cache.esptool_fingerprint.is_empty(),
            "absolute esptool must record the serde-default (empty) fingerprint"
        );
        save_json(&linker_a.bin_cache_path(tmp.path()), &cache).unwrap();

        let linker_b = test_linker_with(Some(esptool), Some("C:\\venv-b".to_string()));
        assert!(
            linker_b.can_reuse_bin(&elf, tmp.path(), &flash_size),
            "absolute-path esptool must keep reusing regardless of caller PATH"
        );
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
            None,
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
        assert!(flags.contains(&"-Wl,--gc-sections".to_string()));
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
            None,
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
            None,
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
        assert!(
            linker
                .mcu_config
                .esptool
                .flash_offsets
                .firmware
                .starts_with("0x")
        );
    }

    /// FastLED/fbuild#1220: during the #1217 outage esptool 5.1.0 WAS
    /// installed — it just wasn't on the daemon's PATH — and the build told
    /// the user to `pip install esptool`. The message must never say that
    /// again, in either branch.
    #[test]
    fn esptool_spawn_failure_never_recommends_pip_install() {
        let provisioned =
            esptool_spawn_failure_message(Some(Path::new("/cache/esptool")), "ENOENT");
        let fallback = esptool_spawn_failure_message(None, "ENOENT");

        for msg in [&provisioned, &fallback] {
            assert!(!msg.contains("pip install"), "{msg}");
            assert!(msg.contains("FBUILD_ESPTOOL_PATH"), "{msg}");
            assert!(msg.contains("ENOENT"), "{msg}");
        }
    }

    /// The provisioned branch is a *different fault* from the PATH-fallback
    /// branch and must not claim provisioning failed.
    #[test]
    fn esptool_spawn_failure_distinguishes_provisioned_from_fallback() {
        let provisioned = esptool_spawn_failure_message(Some(Path::new("/cache/esptool")), "boom");
        assert!(provisioned.contains("/cache/esptool"), "{provisioned}");
        assert!(
            !provisioned.contains("provisioning failed"),
            "{provisioned}"
        );

        let fallback = esptool_spawn_failure_message(None, "boom");
        assert!(fallback.contains("provisioning failed"), "{fallback}");
        assert!(fallback.contains("on PATH"), "{fallback}");
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

    // ── parse_esp32_size_output ─────────────────────────────────────

    #[test]
    fn esp32_size_sysv_separates_flash_from_ram() {
        // Simulates `size -A` output for an ESP32-C6 build.
        // .flash.rodata (45KB in flash) must NOT inflate RAM.
        let output = "\
section             size         addr
.flash.text        89012    0x42000020
.flash.rodata      45678    0x42015c34
.dram0.data         1234    0x3fc80000
.dram0.bss          5678    0x3fc81234
.iram0.text          789    0x40800000
Total              142391
";
        let info = parse_esp32_size_output(output, Some(4_194_304), Some(327_680)).unwrap();
        assert_eq!(info.text, 89012 + 45678 + 789); // flash.text + flash.rodata + iram0.text
        assert_eq!(info.data, 1234);
        assert_eq!(info.bss, 5678);
        assert_eq!(info.total_flash, 89012 + 45678 + 789 + 1234);
        assert_eq!(info.total_ram, 1234 + 5678); // dram0.data + dram0.bss — no .flash.rodata contamination
        assert!(info.ram_percent().unwrap() < 100.0);
    }

    #[test]
    fn esp32_size_returns_none_for_non_esp_output() {
        // Standard Berkeley output without ESP32 section prefixes
        // should return None, so the caller falls back to Berkeley parser.
        let output = "\
   text    data     bss     dec     hex filename
   1234     56      78    1368     558 firmware.elf
";
        assert!(parse_esp32_size_output(output, None, None).is_none());
    }

    #[test]
    fn esp32_size_ignores_total_and_header_lines() {
        let output = "\
section             size         addr
.dram0.data         1000    0x3fc80000
.dram0.bss          2000    0x3fc81000
.flash.text        40000    0x42000020
Total               43000
";
        let info = parse_esp32_size_output(output, None, None).unwrap();
        assert_eq!(info.total_flash, 40000 + 1000);
        assert_eq!(info.total_ram, 1000 + 2000);
    }
}
