//! NRF52 ARM linker implementation.
//!
//! Links ARM Cortex-M4F object files into firmware.elf, converts to firmware.hex,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::Nrf52McuConfig;
use crate::linker::{LinkExtraArgs, Linker};

/// NRF52-specific linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct Nrf52Linker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_script_path: PathBuf,
    linker_search_dirs: Vec<PathBuf>,
    mcu_config: Nrf52McuConfig,
    profile: BuildProfile,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl Nrf52Linker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        linker_script_path: PathBuf,
        linker_search_dirs: Vec<PathBuf>,
        mcu_config: Nrf52McuConfig,
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
            linker_search_dirs,
            mcu_config,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }
}

#[async_trait::async_trait]
impl Linker for Nrf52Linker {
    async fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "arm-none-eabi-ar").await
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

        let mut args: Vec<String> = vec![self.gcc_path.to_string_lossy().to_string()];

        // Linker flags from config
        args.extend(self.mcu_config.linker_flags.iter().cloned());

        // Profile-specific link flags
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            args.extend(profile.link_flags.iter().cloned());
        }
        args.extend(extra.flags.iter().cloned());

        // Linker search dirs (for INCLUDE directives in linker scripts)
        for dir in &self.linker_search_dirs {
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
            tracing::debug!(target: "fbuild_build::linker::nrf52", "link: {}", args.join(" "));
        }

        // GCC LTO temp dir for MSYS-safe paths â€” see FastLED/fbuild#261.
        let lto_env = fbuild_core::subprocess::link_env_for_build(output_dir)?;
        let env_slice: Vec<(&str, &str)> = lto_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        // FastLED/fbuild#809: bound the link step at 3 min.
        let result = run_command(
            &args_ref,
            None,
            Some(&env_slice),
            Some(std::time::Duration::from_secs(180)),
        )
        .await?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "arm-none-eabi-gcc link failed:\n{}",
                result.stderr
            )));
        }

        Ok(elf_path)
    }

    async fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf> {
        crate::linker::LinkerBase::objcopy_firmware(
            &self.objcopy_path,
            elf_path,
            output_dir,
            &self.mcu_config.objcopy.output_format,
            &self.mcu_config.objcopy.remove_sections,
            "arm-none-eabi-objcopy",
        )
        .await
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
        crate::linker::LinkerBase::report_size(
            &self.size_path,
            elf_path,
            self.max_flash,
            self.max_ram,
            "arm-none-eabi-size",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nrf52::mcu_config::get_nrf52_config_for_mcu;

    #[test]
    fn test_nrf52_linker_creation() {
        let linker = Nrf52Linker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/nrf52/nrf52840.ld"),
            vec![],
            get_nrf52_config_for_mcu("nrf52840").unwrap(),
            BuildProfile::Release,
            Some(1048576),
            Some(262144),
            false,
        );
        assert_eq!(linker.max_flash, Some(1048576));
        assert_eq!(linker.max_ram, Some(262144));
    }

    #[test]
    fn test_nrf52_linker_has_linker_script() {
        let linker = Nrf52Linker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/nrf52/nrf52840.ld"),
            vec![],
            get_nrf52_config_for_mcu("nrf52840").unwrap(),
            BuildProfile::Release,
            Some(1048576),
            Some(262144),
            false,
        );
        assert!(linker
            .linker_script_path
            .to_string_lossy()
            .contains("nrf52840"));
    }
}
