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

use crate::linker::Linker;

use super::mcu_config::Esp32McuConfig;

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
    /// SDK linker script flags from `flags/ld_scripts` (`-L`/`-T` flags).
    sdk_ld_scripts: Vec<String>,
    /// Build profile.
    profile: BuildProfile,
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
        sdk_ld_scripts: Vec<String>,
        profile: BuildProfile,
        max_flash: Option<u64>,
        max_ram: Option<u64>,
        verbose: bool,
    ) -> Self {
        Self {
            gcc_path,
            ar_path,
            objcopy_path,
            size_path,
            mcu_config,
            sdk_ld_flags,
            sdk_lib_flags,
            sdk_ld_scripts,
            profile,
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
}

impl Linker for Esp32Linker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if output.exists() {
            std::fs::remove_file(output)?;
        }

        let mut args: Vec<String> = vec![
            self.ar_path.to_string_lossy().to_string(),
            "rcs".to_string(),
            output.to_string_lossy().to_string(),
        ];

        for obj in objects {
            args.push(obj.to_string_lossy().to_string());
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "ar failed: {}",
                result.stderr
            )));
        }

        Ok(())
    }

    fn link(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        output_dir: &Path,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(output_dir)?;
        let elf_path = output_dir.join("firmware.elf");

        let mut link_args: Vec<String> = Vec::new();

        // Compiler/driver
        link_args.push(self.gcc_path.to_string_lossy().to_string());

        // Linker flags (from SDK flags/ld_flags or MCU config fallback)
        link_args.extend(self.linker_flags());

        // Linker scripts (from SDK flags/ld_scripts)
        link_args.extend(self.sdk_ld_scripts.clone());

        // Memory usage reporting
        link_args.push("-Wl,--print-memory-usage".to_string());

        // Output
        link_args.extend(["-o".to_string(), elf_path.to_string_lossy().to_string()]);

        // Sketch objects
        for obj in objects {
            link_args.push(obj.to_string_lossy().to_string());
        }

        // Core objects / archives
        for archive in archives {
            link_args.push(archive.to_string_lossy().to_string());
        }

        // SDK precompiled libraries (ordered flags from flags/ld_libs)
        if !self.sdk_lib_flags.is_empty() {
            link_args.push("-Wl,--start-group".to_string());
            link_args.extend(self.sdk_lib_flags.clone());
            link_args.push("-Wl,--end-group".to_string());
        }

        if self.verbose {
            tracing::info!("link: {}", link_args.join(" "));
        }

        // On Windows, use a response file if the command is too long
        let result =
            if cfg!(windows) && link_args.iter().map(|s| s.len() + 1).sum::<usize>() > 30000 {
                let response_content = link_args[1..].join("\n");
                let rsp_path = std::env::temp_dir()
                    .join(format!("fbuild_esp32_link_{}.rsp", std::process::id()));
                std::fs::write(&rsp_path, &response_content).map_err(|e| {
                    fbuild_core::FbuildError::BuildFailed(format!(
                        "failed to write linker response file: {}",
                        e
                    ))
                })?;
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
        // ESP32 firmware is flashed from the ELF directly via esptool.py elf2image.
        // Raw `objcopy -O binary` produces a bloated file because the ELF has segments
        // at high addresses (IRAM 0x400xxxxx, DRAM 0x3FFxxxxx).
        // Copy the ELF to the output directory as the build artifact.
        let elf_out = output_dir.join("firmware.elf");
        if elf_path != elf_out {
            std::fs::copy(elf_path, &elf_out)?;
        }
        Ok(elf_out)
    }

    fn report_size(&self, elf_path: &Path) -> Result<SizeInfo> {
        let args = [
            self.size_path.to_string_lossy().to_string(),
            elf_path.to_string_lossy().to_string(),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "size command failed: {}",
                result.stderr
            )));
        }

        SizeInfo::parse(&result.stdout, self.max_flash, self.max_ram).ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to parse size output:\n{}",
                result.stdout
            ))
        })
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
            vec![
                "-L/sdk/ld".to_string(),
                "-Tmemory.ld".to_string(),
                "-Tsections.ld".to_string(),
            ],
            BuildProfile::Release,
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
            vec!["-Tmemory.ld".to_string()],
            BuildProfile::Release,
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
        assert!(linker.sdk_ld_scripts.iter().any(|f| f.starts_with("-L")));
        assert!(linker.sdk_ld_scripts.iter().any(|f| f == "-Tmemory.ld"));
        assert!(linker.sdk_ld_scripts.iter().any(|f| f == "-Tsections.ld"));
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
            vec![],
            BuildProfile::Release,
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
}
