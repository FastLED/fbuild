//! AVR linker implementation.
//!
//! Links AVR object files into firmware.elf, converts to firmware.hex,
//! and reports size using avr-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::AvrMcuConfig;
use crate::linker::{LinkExtraArgs, Linker};

/// AVR-specific linker using avr-gcc (link driver), avr-ar, avr-objcopy, avr-size.
pub struct AvrLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    mcu: String,
    mcu_config: AvrMcuConfig,
    profile: BuildProfile,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl AvrLinker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        mcu: &str,
        mcu_config: AvrMcuConfig,
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
            mcu: mcu.to_string(),
            mcu_config,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }
}

impl Linker for AvrLinker {
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "avr-ar")
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

        let mut args: Vec<String> = vec![
            self.gcc_path.to_string_lossy().to_string(),
            format!("-mmcu={}", self.mcu),
        ];

        // Linker flags from config
        args.extend(self.mcu_config.linker_flags.iter().cloned());

        // Profile-specific link flags
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            args.extend(profile.link_flags.iter().cloned());
        }
        args.extend(extra.flags.iter().cloned());

        args.extend(["-o".to_string(), elf_path.to_string_lossy().to_string()]);

        // Sketch objects first
        for obj in objects {
            args.push(obj.to_string_lossy().to_string());
        }

        // Core objects passed directly (not archived) for LTO compatibility
        // Python fbuild does the same: archive breaks LTO symbol visibility
        for archive in archives {
            args.push(archive.to_string_lossy().to_string());
        }

        // Group for circular deps + libraries from config
        args.push("-Wl,--start-group".to_string());
        args.extend(self.mcu_config.linker_libs.iter().cloned());
        args.extend(extra.libs.iter().cloned());
        args.push("-Wl,--end-group".to_string());

        if self.verbose {
            tracing::info!("link: {}", args.join(" "));
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "avr-gcc link failed:\n{}",
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
            "avr-objcopy",
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
            "avr-size",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avr::mcu_config::get_avr_config;

    #[test]
    fn test_avr_linker_creation() {
        let linker = AvrLinker::new(
            PathBuf::from("/bin/avr-gcc"),
            PathBuf::from("/bin/avr-ar"),
            PathBuf::from("/bin/avr-objcopy"),
            PathBuf::from("/bin/avr-size"),
            "atmega328p",
            get_avr_config().unwrap(),
            BuildProfile::Release,
            Some(32256),
            Some(2048),
            false,
        );
        assert_eq!(linker.mcu, "atmega328p");
        assert_eq!(linker.max_flash, Some(32256));
        assert_eq!(linker.max_ram, Some(2048));
    }
}
