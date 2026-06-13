//! Generic ARM linker implementation.
//!
//! Links ARM Cortex-M object files into firmware.elf, converts to firmware.hex,
//! and reports size using arm-none-eabi-size. Used by STM32, RP2040, NRF52, etc.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::ArmMcuConfig;
use crate::linker::{LinkExtraArgs, Linker};

/// Generic ARM linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct ArmLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_script_path: PathBuf,
    lib_search_dirs: Vec<PathBuf>,
    mcu_config: ArmMcuConfig,
    profile: BuildProfile,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl ArmLinker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        linker_script_path: PathBuf,
        mcu_config: ArmMcuConfig,
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
            linker_script_path,
            lib_search_dirs: Vec::new(),
            mcu_config,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }

    /// Add linker search directories emitted as `-L<dir>`. Needed when the
    /// linker script uses a relative `INCLUDE` directive (ld searches `-L`
    /// paths for it), e.g. ArduinoCore-LPC8xx's `lpc845_flash.ld` which does
    /// `INCLUDE linker_scripts/gcc/lpc8xx_common.ld`.
    pub fn with_lib_search_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.lib_search_dirs = dirs;
        self
    }
}

impl Linker for ArmLinker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "arm-none-eabi-ar")
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

        let mut args: Vec<String> = vec![self.gcc_path.to_string_lossy().to_string()];

        // Linker flags from config
        args.extend(self.mcu_config.linker_flags.iter().cloned());

        // Profile-specific link flags
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            args.extend(profile.link_flags.iter().cloned());
        }
        args.extend(extra.flags.iter().cloned());

        // Linker-script search dirs (-L). ld resolves a relative `INCLUDE`
        // in the linker script against these paths.
        for dir in &self.lib_search_dirs {
            args.push(format!("-L{}", dir.display()));
        }

        args.extend([
            format!("-T{}", self.linker_script_path.display()),
            "-o".to_string(),
            elf_path.to_string_lossy().to_string(),
        ]);

        // Always emit a linker map next to firmware.elf for debugging (#305).
        let map_path = output_dir.join("firmware.map");
        args.push(format!("-Wl,-Map={}", map_path.to_string_lossy()));

        // Sketch objects first
        for obj in objects {
            args.push(obj.to_string_lossy().to_string());
        }

        // Core objects passed directly (not archived) for LTO compatibility
        for archive in archives {
            args.push(archive.to_string_lossy().to_string());
        }

        // Linker libraries from config
        args.extend(self.mcu_config.linker_libs.iter().cloned());
        args.extend(extra.libs.iter().cloned());

        if self.verbose {
            eprintln!("link: {}", args.join(" "));
            tracing::info!("link: {}", args.join(" "));
        }

        // GCC LTO temp dir for MSYS-safe paths — see FastLED/fbuild#261.
        let lto_env = fbuild_core::subprocess::link_env_for_build(output_dir)?;
        let env_slice: Vec<(&str, &str)> = lto_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // On Windows, use a response file to avoid command-line length limits
        // (STM32 HAL/LL wrappers produce hundreds of .o files).
        let result = if cfg!(windows) && args.len() > 50 {
            let temp_dir = output_dir.join("tmp");
            std::fs::create_dir_all(&temp_dir)?;
            let rsp_content: Vec<String> = args[1..].iter().map(|a| a.replace('\\', "/")).collect();
            let rsp_path = fbuild_core::response_file::write_response_file(
                &rsp_content,
                &temp_dir,
                "arm_link",
            )?;
            let rsp_arg = format!("@{}", rsp_path.display());
            run_command(&[args[0].as_str(), &rsp_arg], None, Some(&env_slice), None)?
        } else {
            let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            run_command(&args_ref, None, Some(&env_slice), None)?
        };

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "arm-none-eabi-gcc link failed:\n{}",
                result.stderr
            )));
        }

        Ok(elf_path)
    }

    fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf> {
        crate::linker::LinkerBase::objcopy_firmware(
            &self.objcopy_path,
            elf_path,
            output_dir,
            &self.mcu_config.objcopy.output_format,
            &self.mcu_config.objcopy.remove_sections,
            "arm-none-eabi-objcopy",
        )
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
        crate::linker::LinkerBase::report_size(
            &self.size_path,
            elf_path,
            self.max_flash,
            self.max_ram,
            "arm-none-eabi-size",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ArmMcuConfig {
        serde_json::from_str(
            r#"{
                "name": "TestARM",
                "architecture": "arm-cortex-m3",
                "compiler_flags": {
                    "common": ["-mcpu=cortex-m3", "-mthumb"],
                    "c": ["-std=gnu11"],
                    "cxx": ["-std=gnu++17"]
                },
                "linker_flags": ["-mcpu=cortex-m3", "-mthumb", "-Wl,--gc-sections"],
                "linker_libs": ["-lgcc", "-lstdc++", "-lm", "-lc"],
                "objcopy": {"output_format": "ihex", "remove_sections": [".eeprom"]},
                "profiles": {
                    "release": {"compile_flags": ["-Os"], "link_flags": ["-flto"]},
                    "quick": {"compile_flags": ["-Os"], "link_flags": []}
                },
                "defines": []
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_arm_linker_creation() {
        let linker = ArmLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/cores/variant/linker.ld"),
            test_config(),
            BuildProfile::Release,
            Some(65536),
            Some(20480),
            false,
        );
        assert_eq!(linker.max_flash, Some(65536));
        assert_eq!(linker.max_ram, Some(20480));
    }

    #[test]
    fn test_arm_linker_has_linker_script() {
        let linker = ArmLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/cores/variant/stm32f103.ld"),
            test_config(),
            BuildProfile::Release,
            Some(65536),
            Some(20480),
            false,
        );
        assert!(linker
            .linker_script_path
            .to_string_lossy()
            .contains("stm32f103"));
    }
}
