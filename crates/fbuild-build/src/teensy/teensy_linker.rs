//! Teensy ARM linker implementation.
//!
//! Links ARM Cortex-M7 object files into firmware.elf, converts to firmware.hex,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::TeensyMcuConfig;
use crate::linker::{Linker, LinkerScripts};

/// Teensy-specific linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct TeensyLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_scripts: LinkerScripts,
    mcu_config: TeensyMcuConfig,
    profile: BuildProfile,
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
        linker_scripts: LinkerScripts,
        mcu_config: TeensyMcuConfig,
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
            linker_scripts,
            mcu_config,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }
}

impl Linker for TeensyLinker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "arm-none-eabi-ar")
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

        // Profile-specific link flags
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            args.extend(profile.link_flags.iter().cloned());
        }

        args.extend(self.linker_scripts.to_args());
        args.extend(["-o".to_string(), elf_path.to_string_lossy().to_string()]);

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
    use crate::teensy::mcu_config::get_teensy_config;

    #[test]
    fn test_teensy_linker_creation() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            LinkerScripts::single(PathBuf::from("/teensy4"), "imxrt1062_t41.ld"),
            get_teensy_config().unwrap(),
            BuildProfile::Release,
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
            LinkerScripts::single(PathBuf::from("/teensy4"), "imxrt1062_t41.ld"),
            get_teensy_config().unwrap(),
            BuildProfile::Release,
            Some(8126464),
            Some(1048576),
            false,
        );
        assert!(linker
            .linker_scripts
            .scripts
            .iter()
            .any(|s| s.contains("imxrt1062")));
    }
}
