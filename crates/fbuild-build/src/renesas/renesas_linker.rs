//! Renesas RA ARM linker implementation.
//!
//! Links ARM Cortex-M4 object files into firmware.elf, converts to firmware.bin,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::RenesasMcuConfig;
use crate::linker::{LinkExtraArgs, Linker};

/// Renesas-specific linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct RenesasLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_script_path: PathBuf,
    mcu_config: RenesasMcuConfig,
    profile: BuildProfile,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
}

impl RenesasLinker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        linker_script_path: PathBuf,
        mcu_config: RenesasMcuConfig,
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
            mcu_config,
            profile,
            max_flash,
            max_ram,
            verbose,
        }
    }
}

impl Linker for RenesasLinker {
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

        // Add variant directory to linker search path so INCLUDE directives
        // in fsp.ld can find memory_regions.ld
        if let Some(variant_dir) = self.linker_script_path.parent() {
            args.push(format!("-L{}", variant_dir.display()));
            // Link with libfsp.a if present in the variant's libs/ directory
            let libfsp = variant_dir.join("libs").join("libfsp.a");
            if libfsp.exists() {
                args.push("-Wl,--whole-archive".to_string());
                args.push(libfsp.to_string_lossy().to_string());
                args.push("-Wl,--no-whole-archive".to_string());
            }
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

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, Some(&env_slice), None)?;

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
    use crate::renesas::mcu_config::get_renesas_config_for_mcu;

    #[test]
    fn test_renesas_linker_creation() {
        let linker = RenesasLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/renesas/ra4m1.ld"),
            get_renesas_config_for_mcu("ra4m1").unwrap(),
            BuildProfile::Release,
            Some(262144),
            Some(32768),
            false,
        );
        assert_eq!(linker.max_flash, Some(262144));
        assert_eq!(linker.max_ram, Some(32768));
    }

    #[test]
    fn test_renesas_linker_has_linker_script() {
        let linker = RenesasLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/renesas/ra4m1.ld"),
            get_renesas_config_for_mcu("ra4m1").unwrap(),
            BuildProfile::Release,
            Some(262144),
            Some(32768),
            false,
        );
        assert!(linker
            .linker_script_path
            .to_string_lossy()
            .contains("ra4m1"));
    }
}
