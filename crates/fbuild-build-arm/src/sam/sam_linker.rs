//! SAM ARM linker implementation.
//!
//! Links ARM Cortex-M3 object files into firmware.elf, converts to firmware.bin,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::SamMcuConfig;
use crate::linker::{LinkExtraArgs, Linker};

/// SAM-specific linker using arm-none-eabi-gcc (link driver), ar, objcopy, size.
pub struct SamLinker {
    gcc_path: PathBuf,
    ar_path: PathBuf,
    objcopy_path: PathBuf,
    size_path: PathBuf,
    linker_script_path: PathBuf,
    mcu_config: SamMcuConfig,
    profile: BuildProfile,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
    verbose: bool,
    extra_lib_dirs: Vec<PathBuf>,
    extra_libs: Vec<String>,
}

impl SamLinker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gcc_path: PathBuf,
        ar_path: PathBuf,
        objcopy_path: PathBuf,
        size_path: PathBuf,
        linker_script_path: PathBuf,
        mcu_config: SamMcuConfig,
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
            extra_lib_dirs: Vec::new(),
            extra_libs: Vec::new(),
        }
    }

    /// Add extra library search directories (passed as `-L` to linker).
    pub fn add_lib_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.extra_lib_dirs.extend(dirs);
    }

    /// Add extra libraries to link (passed as `-l<name>` to linker).
    pub fn add_libs(&mut self, libs: Vec<String>) {
        self.extra_libs.extend(libs);
    }
}

#[async_trait::async_trait]
impl Linker for SamLinker {
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

        // Extra library search paths
        for dir in &self.extra_lib_dirs {
            args.push(format!("-L{}", dir.display()));
        }

        // Extra libraries (e.g. variant system lib)
        for lib in &self.extra_libs {
            if lib.starts_with("-l") || lib.contains(std::path::MAIN_SEPARATOR) || lib.contains('/')
            {
                args.push(lib.clone());
            } else {
                args.push(format!("-l{}", lib));
            }
        }

        // Linker libraries from config
        args.extend(self.mcu_config.linker_libs.iter().cloned());
        args.extend(extra.libs.iter().cloned());

        if self.verbose {
            tracing::debug!(target: "fbuild_build::linker::sam", "link: {}", args.join(" "));
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
    use crate::sam::mcu_config::get_sam_config_for_mcu;

    #[test]
    fn test_sam_linker_creation() {
        let linker = SamLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/sam/sam3x8e.ld"),
            get_sam_config_for_mcu("at91sam3x8e").unwrap(),
            BuildProfile::Release,
            Some(524288),
            Some(98304),
            false,
        );
        assert_eq!(linker.max_flash, Some(524288));
        assert_eq!(linker.max_ram, Some(98304));
    }

    #[test]
    fn test_sam_linker_has_linker_script() {
        let linker = SamLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            PathBuf::from("/sam/sam3x8e.ld"),
            get_sam_config_for_mcu("at91sam3x8e").unwrap(),
            BuildProfile::Release,
            Some(524288),
            Some(98304),
            false,
        );
        assert!(
            linker
                .linker_script_path
                .to_string_lossy()
                .contains("sam3x8e")
        );
    }
}
