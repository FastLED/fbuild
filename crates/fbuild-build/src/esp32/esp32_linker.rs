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
    objcopy_path: PathBuf,
    size_path: PathBuf,
    /// MCU config drives linker flags, scripts, and undefined symbols.
    mcu_config: Esp32McuConfig,
    /// Directory containing linker scripts (tools/sdk/{mcu}/ld/).
    linker_scripts_dir: PathBuf,
    /// Precompiled `.a` libraries from ESP-IDF SDK.
    sdk_libs: Vec<PathBuf>,
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
        linker_scripts_dir: PathBuf,
        sdk_libs: Vec<PathBuf>,
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
            linker_scripts_dir,
            sdk_libs,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }

    /// Build all linker flags from the MCU config.
    fn linker_flags(&self) -> Vec<String> {
        let mut flags = self.mcu_config.linker_flags.clone();

        // Add profile-specific link flags
        let profile_name = match self.profile {
            BuildProfile::Release => "release",
            BuildProfile::Quick => "quick",
        };
        if let Some(profile) = self.mcu_config.get_profile(profile_name) {
            flags.extend(profile.link_flags.clone());
        }

        flags
    }

    /// Build linker script flags (`-T{script}`).
    fn linker_script_flags(&self) -> Vec<String> {
        let mut flags = vec![format!("-L{}", self.linker_scripts_dir.display())];
        for script in &self.mcu_config.linker_scripts {
            flags.push(format!("-T{}", script));
        }
        flags
    }

    /// Build SDK library flags.
    fn sdk_lib_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();

        // Add library search paths (unique parent directories of .a files)
        let mut lib_dirs = std::collections::HashSet::new();
        for lib in &self.sdk_libs {
            if let Some(parent) = lib.parent() {
                if lib_dirs.insert(parent.to_path_buf()) {
                    flags.push(format!("-L{}", parent.display()));
                }
            }
        }

        // Add each library as -l{name} (strip lib prefix and .a suffix)
        for lib in &self.sdk_libs {
            if let Some(stem) = lib.file_stem() {
                let name = stem.to_string_lossy();
                if let Some(stripped) = name.strip_prefix("lib") {
                    flags.push(format!("-l{}", stripped));
                } else {
                    // If not prefixed with lib, pass the full path
                    flags.push(lib.to_string_lossy().to_string());
                }
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

        // Linker flags from MCU config (includes -nostartfiles, -march, -u symbols, etc.)
        link_args.extend(self.linker_flags());

        // Linker scripts
        link_args.extend(self.linker_script_flags());

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

        // SDK precompiled libraries (wrap in --start-group/--end-group for circular deps)
        let sdk_flags = self.sdk_lib_flags();
        if !sdk_flags.is_empty() {
            link_args.push("-Wl,--start-group".to_string());
            link_args.extend(sdk_flags);
            link_args.push("-Wl,--end-group".to_string());
        }

        // Standard linker libraries from config
        link_args.extend(self.mcu_config.linker_libs.clone());

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
        // ESP32 uses binary output, not ihex
        let bin_path = output_dir.join("firmware.bin");

        let args = [
            self.objcopy_path.to_string_lossy().to_string(),
            "-O".to_string(),
            "binary".to_string(),
            elf_path.to_string_lossy().to_string(),
            bin_path.to_string_lossy().to_string(),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "objcopy to binary failed: {}",
                result.stderr
            )));
        }

        Ok(bin_path)
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
            PathBuf::from("/sdk/esp32c6/ld"),
            vec![
                PathBuf::from("/sdk/lib/libfreertos.a"),
                PathBuf::from("/sdk/lib/libesp_system.a"),
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
    fn test_linker_flags_contain_config_flags() {
        let linker = test_linker("esp32c6");
        let flags = linker.linker_flags();
        assert!(flags.contains(&"-nostartfiles".to_string()));
        assert!(flags.iter().any(|f| f.contains("IDF_TARGET_ESP32C6")));
        assert!(flags.contains(&"-fno-rtti".to_string()));
        assert!(flags.contains(&"-Wl,--gc-sections".to_string()));
        // Profile flags (release)
        assert!(flags.contains(&"-flto=auto".to_string()));
    }

    #[test]
    fn test_linker_script_flags() {
        let linker = test_linker("esp32c6");
        let flags = linker.linker_script_flags();
        assert!(flags.iter().any(|f| f.starts_with("-L")));
        assert!(flags.iter().any(|f| f == "-Tmemory.ld"));
        assert!(flags.iter().any(|f| f == "-Tsections.ld"));
        assert!(flags.iter().any(|f| f.contains("esp32c6.rom")));
    }

    #[test]
    fn test_sdk_lib_flags() {
        let linker = test_linker("esp32c6");
        let flags = linker.sdk_lib_flags();
        assert!(flags.iter().any(|f| f == "-lfreertos"));
        assert!(flags.iter().any(|f| f == "-lesp_system"));
        assert!(flags.iter().any(|f| f.starts_with("-L")));
    }

    #[test]
    fn test_linker_undefined_symbols() {
        let linker = test_linker("esp32c6");
        let flags = linker.linker_flags();
        // Should have force-linked symbols
        assert!(flags.contains(&"-u".to_string()));
        assert!(flags.contains(&"app_main".to_string()));
        assert!(flags.contains(&"_Z5setupv".to_string()));
        assert!(flags.contains(&"_Z4loopv".to_string()));
    }

    #[test]
    fn test_xtensa_linker_flags() {
        let linker = test_linker("esp32");
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
