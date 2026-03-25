//! Teensy ARM linker implementation.
//!
//! Links ARM Cortex-M7 object files into firmware.elf, converts to firmware.hex,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{Result, SizeInfo};

use super::mcu_config::TeensyMcuConfig;
use crate::linker::Linker;

/// Teensy-specific linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct TeensyLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_script_path: PathBuf,
    mcu_config: TeensyMcuConfig,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl TeensyLinker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        linker_script_path: PathBuf,
        mcu_config: TeensyMcuConfig,
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
            mcu_config,
            max_flash,
            max_ram,
            verbose,
        }
    }
}

impl Linker for TeensyLinker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove existing archive to avoid stale objects
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
                "arm-none-eabi-ar failed: {}",
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

        let mut args: Vec<String> = vec![self.gcc_path.to_string_lossy().to_string()];

        // Linker flags from config
        args.extend(self.mcu_config.linker_flags.iter().cloned());

        // Profile link flags
        if let Some(profile) = self.mcu_config.get_profile("release") {
            args.extend(profile.link_flags.iter().cloned());
        }

        args.extend([
            format!("-T{}", self.linker_script_path.display()),
            "-o".to_string(),
            elf_path.to_string_lossy().to_string(),
        ]);

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

        if self.verbose {
            tracing::info!("link: {}", args.join(" "));
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "arm-none-eabi-gcc link failed:\n{}",
                result.stderr
            )));
        }

        Ok(elf_path)
    }

    fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf> {
        let hex_path = output_dir.join("firmware.hex");

        let mut args = vec![
            self.objcopy_path.to_string_lossy().to_string(),
            "-O".to_string(),
            self.mcu_config.objcopy.output_format.clone(),
        ];

        for section in &self.mcu_config.objcopy.remove_sections {
            args.push("-R".to_string());
            args.push(section.clone());
        }

        args.push(elf_path.to_string_lossy().to_string());
        args.push(hex_path.to_string_lossy().to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "arm-none-eabi-objcopy failed: {}",
                result.stderr
            )));
        }

        Ok(hex_path)
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
                "arm-none-eabi-size failed: {}",
                result.stderr
            )));
        }

        SizeInfo::parse(&result.stdout, self.max_flash, self.max_ram).ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to parse arm-none-eabi-size output:\n{}",
                result.stdout
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teensy::mcu_config::get_teensy_config;

    #[test]
    fn test_teensy_linker_creation() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/teensy4/imxrt1062_t41.ld"),
            get_teensy_config().unwrap(),
            Some(8126464),
            Some(1048576),
            false,
        );
        assert_eq!(linker.max_flash, Some(8126464));
        assert_eq!(linker.max_ram, Some(1048576));
    }

    #[test]
    fn test_teensy_linker_has_linker_script() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/teensy4/imxrt1062_t41.ld"),
            get_teensy_config().unwrap(),
            Some(8126464),
            Some(1048576),
            false,
        );
        assert!(linker
            .linker_script_path
            .to_string_lossy()
            .contains("imxrt1062"));
    }
}
