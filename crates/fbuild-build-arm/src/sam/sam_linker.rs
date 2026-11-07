//! SAM ARM linker implementation.
//!
//! Links ARM Cortex-M3 object files into firmware.elf, converts to firmware.bin,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::SamMcuConfig;
use crate::linker::{LinkExtraArgs, Linker, link_cwd_for};

/// Declared link symbols that are absent from the linked image.
///
/// A link can exit 0 and still produce a firmware that contains no
/// application: with `--gc-sections`, any section nothing references is
/// discarded, so a missing startup object leaves an image with no reset
/// handler, no `main` and no sketch (FastLED/fbuild#1939). The exit status
/// cannot see that, so platforms declare the symbols their image must contain
/// (`required_link_symbols` in the MCU config) and the linker verifies them.
fn missing_required_symbols<'a>(
    required: &'a [String],
    present: impl Iterator<Item = &'a str>,
) -> Vec<&'a str> {
    let present: std::collections::HashSet<&str> = present.collect();
    required
        .iter()
        .map(String::as_str)
        .filter(|symbol| !present.contains(symbol))
        .collect()
}

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
    group_archives: bool,
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
            group_archives: false,
        }
    }

    /// Rescan archived cores and libraries only for cores built as archives.
    pub fn with_archive_group(mut self, enabled: bool) -> Self {
        self.group_archives = enabled;
        self
    }

    /// Add extra library search directories (passed as `-L` to linker).
    pub fn add_lib_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.extra_lib_dirs.extend(dirs);
    }

    /// Add extra libraries to link (passed as `-l<name>` to linker).
    pub fn add_libs(&mut self, libs: Vec<String>) {
        self.extra_libs.extend(libs);
    }

    /// Fail the build when the linked image is missing a symbol the platform
    /// declares mandatory. No-op when the MCU config declares none.
    async fn require_link_symbols(&self, elf_path: &Path) -> Result<()> {
        if self.mcu_config.required_link_symbols.is_empty() {
            return Ok(());
        }
        let map = crate::linker::LinkerBase::analyze_symbols(&self.size_path, elf_path).await?;
        let missing = missing_required_symbols(
            &self.mcu_config.required_link_symbols,
            map.symbols.iter().map(|entry| entry.name.as_str()),
        );
        if missing.is_empty() {
            return Ok(());
        }
        Err(fbuild_core::FbuildError::BuildFailed(format!(
            "link succeeded but firmware.elf is missing {} ({}). Nothing rooted {}, so \
             --gc-sections discarded the application: this image would boot into nothing. \
             Check the linker script's ENTRY and the platform's entry flags.",
            if missing.len() == 1 {
                "symbol"
            } else {
                "symbols"
            },
            missing.join(", "),
            if missing.len() == 1 { "it" } else { "them" }
        )))
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

        // Rescan archives so dependencies between core and library members
        // resolve without forcing unreferenced interrupt handlers into the ELF.
        if self.group_archives {
            args.push("-Wl,--start-group".into());
        }
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
        if self.group_archives {
            args.push("-Wl,--end-group".into());
        }

        if self.verbose {
            tracing::debug!(target: "fbuild_build::linker::sam", "link: {}", args.join(" "));
        }

        // GCC LTO temp dir for MSYS-safe paths â€” see FastLED/fbuild#261.
        let lto_env = fbuild_core::subprocess::link_env_for_build(output_dir)?;
        let env_slice: Vec<(&str, &str)> = lto_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // Run the link in the firmware's own output directory rather than
        // inheriting the daemon's cwd (typically the user's project root):
        // arm-none-eabi-gcc hands off to collect2 / lto-wrapper, which write
        // scratch files relative to the process cwd. See FastLED/fbuild#1269
        // (and #1268, which fixed the same bug for teensy).
        let mut cwd_sensitive: Vec<&Path> = vec![&self.linker_script_path];
        cwd_sensitive.extend(objects.iter().map(|p| p.as_path()));
        cwd_sensitive.extend(archives.iter().map(|p| p.as_path()));
        cwd_sensitive.extend(self.extra_lib_dirs.iter().map(|p| p.as_path()));
        // `extra_libs` entries that name a file rather than a `-l<name>` are
        // passed through verbatim, so they are cwd-sensitive too.
        cwd_sensitive.extend(
            self.extra_libs
                .iter()
                .filter(|lib| {
                    !lib.starts_with("-l")
                        && (lib.contains(std::path::MAIN_SEPARATOR) || lib.contains('/'))
                })
                .map(Path::new),
        );
        let link_cwd = link_cwd_for(output_dir, &cwd_sensitive);

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        // FastLED/fbuild#809: bound the link step at 3 min.
        let result = run_command(
            &args_ref,
            link_cwd,
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

        // FastLED/fbuild#1939: a zero exit status is not proof the application
        // was linked -- `--gc-sections` will happily emit an image holding only
        // library leftovers. Verify the symbols this platform's firmware must
        // define before reporting success.
        self.require_link_symbols(&elf_path).await?;

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

    /// Atmel's Due `flash.ld` declares no `ENTRY()`, and Arduino's own SAM3X
    /// recipe supplies the entry on the link line. Without it the link pulls
    /// no startup object and `--gc-sections` discards the whole application.
    #[test]
    fn sam3x_link_roots_the_atmel_startup_entry() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert!(
            config
                .linker_flags
                .iter()
                .any(|flag| flag == "-Wl,--entry=Reset_Handler"),
            "SAM3X must set the ELF entry to Reset_Handler: {}",
            config.linker_flags.join(" ")
        );
        assert!(
            config
                .linker_flags
                .iter()
                .any(|flag| flag == "-Wl,--undefined=Reset_Handler"),
            "SAM3X must reference Reset_Handler so a missing startup fails the link"
        );
    }

    /// The classic-SAM image is inert without its reset handler, so the config
    /// must declare it and the link must root it.
    #[test]
    fn sam3x_requires_the_reset_handler_in_the_image() {
        let config = get_sam_config_for_mcu("at91sam3x8e").unwrap();
        assert_eq!(config.required_link_symbols, vec!["Reset_Handler"]);
    }

    #[test]
    fn missing_required_symbols_names_only_the_absent_ones() {
        let required = ["Reset_Handler".to_string(), "main".to_string()];
        let present = ["main", "setup"];
        assert_eq!(
            missing_required_symbols(&required, present.into_iter()),
            vec!["Reset_Handler"]
        );
        assert!(
            missing_required_symbols(&required, ["Reset_Handler", "main"].into_iter()).is_empty()
        );
    }

    #[test]
    fn mcu_configs_without_declared_symbols_skip_the_check() {
        // SAMD/ClearCore scripts declare ENTRY(), so they need no config-level
        // declaration and must not pay an extra nm invocation.
        for mcu in ["samd21g18a", "samd51j19a", "same53n19a"] {
            let config = get_sam_config_for_mcu(mcu).unwrap();
            assert!(config.required_link_symbols.is_empty(), "{mcu}");
        }
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
