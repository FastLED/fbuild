//! Teensy ARM linker implementation.
//!
//! Links ARM Cortex-M7 object files into firmware.elf, converts to firmware.hex,
//! and reports size using arm-none-eabi-size.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{BuildProfile, Result, SizeInfo};

use super::mcu_config::TeensyMcuConfig;
use crate::linker::{LinkExtraArgs, Linker, LinkerScripts};

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
    /// Bare CMSIS-DSP math library name (e.g. `arm_cortexM4lf_math`) to link
    /// via `-l<name>`. Mirrors PlatformIO+Teensyduino's per-MCU auto-link of
    /// the appropriate `libarm_cortex*_math.a` so Teensy `Audio.h` FFT classes
    /// resolve at link time. See FastLED/fbuild#300.
    cmsis_dsp_lib: Option<String>,
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
        cmsis_dsp_lib: Option<String>,
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
            cmsis_dsp_lib,
            verbose,
        }
    }

    /// Bare CMSIS-DSP library name configured for this linker, if any.
    pub fn cmsis_dsp_lib(&self) -> Option<&str> {
        self.cmsis_dsp_lib.as_deref()
    }

    /// Build the full `arm-none-eabi-gcc` link command-line for the given
    /// inputs. Exposed for unit-testing the auto-appended CMSIS-DSP lib
    /// (FastLED/fbuild#300) without spawning the real linker.
    fn build_link_args(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        elf_path: &Path,
        extra: &LinkExtraArgs,
    ) -> Vec<String> {
        let mut args: Vec<String> = vec![self.gcc_path.to_string_lossy().to_string()];

        // Linker flags from config
        args.extend(self.mcu_config.linker_flags.iter().cloned());

        // Profile-specific link flags
        if let Some(profile) = self.mcu_config.get_profile(self.profile.as_dir_name()) {
            args.extend(profile.link_flags.iter().cloned());
        }
        args.extend(extra.flags.iter().cloned());

        args.extend(self.linker_scripts.to_args());
        args.extend(["-o".to_string(), elf_path.to_string_lossy().to_string()]);

        // Always emit a linker map next to firmware.elf for debugging (#305).
        let map_path = elf_path.with_extension("map");
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
        // Per-board CMSIS-DSP math library auto-link (FastLED/fbuild#300).
        // Mirrors PlatformIO+Teensyduino's behaviour: when the board defines
        // `build.cmsis_dsp_lib`, append `-l<name>` so the linker resolves
        // CMSIS-DSP symbols (`arm_cfft_*`, etc.) referenced by `Audio.h` FFT
        // classes from `libarm_cortex*_math.a` that ships in the Teensy core
        // dir (already on the search path via `-L<core_dir>`).
        if let Some(ref lib) = self.cmsis_dsp_lib {
            args.push(format!("-l{}", lib));
        }
        args.extend(extra.libs.iter().cloned());

        args
    }
}

#[async_trait::async_trait]
impl Linker for TeensyLinker {
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

        let args = self.build_link_args(objects, archives, &elf_path, extra);

        if self.verbose {
            tracing::debug!(target: "fbuild_build::linker::teensy", "link: {}", args.join(" "));
        }

        // Redirect GCC LTO temp files into a forward-slashed, fbuild-owned
        // dir under the build dir so MSYS `mv` doesn't collapse backslashes
        // in the recipe lines emitted by lto-wrapper. See FastLED/fbuild#261.
        let lto_env = fbuild_core::subprocess::link_env_for_build(output_dir)?;
        let env_slice: Vec<(&str, &str)> = lto_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // On Windows, use a response file to avoid command-line length limits
        // (teensy41 produces ~500 .o files; see issue #234).
        //
        // FastLED/fbuild#809: bound the link step at 3 min — teensy41
        // links comfortably under this budget.
        let link_timeout = Some(std::time::Duration::from_secs(180));
        let result = if cfg!(windows) && args.len() > 50 {
            let temp_dir = output_dir.join("tmp");
            std::fs::create_dir_all(&temp_dir)?;
            let rsp_content: Vec<String> = args[1..].iter().map(|a| a.replace('\\', "/")).collect();
            let rsp_path = fbuild_core::response_file::write_response_file(
                &rsp_content,
                &temp_dir,
                "teensy_link",
            )
            .await?;
            let rsp_arg = format!("@{}", rsp_path.display());
            run_command(
                &[args[0].as_str(), &rsp_arg],
                None,
                Some(&env_slice),
                link_timeout,
            )
            .await?
        } else {
            let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            run_command(&args_ref, None, Some(&env_slice), link_timeout).await?
        };

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
            None,
            false,
        );
        assert_eq!(linker.max_flash, Some(8126464));
        assert_eq!(linker.max_ram, Some(1048576));
        assert!(linker.cmsis_dsp_lib().is_none());
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
            None,
            false,
        );
        assert!(linker
            .linker_scripts
            .scripts
            .iter()
            .any(|s| s.contains("imxrt1062")));
    }

    /// Regression test for FastLED/fbuild#300: when a CMSIS-DSP library is
    /// configured, the linker stores it so the `-l<lib>` flag is appended at
    /// link time (mirrors PlatformIO+Teensyduino's auto-link behaviour).
    #[test]
    fn test_teensy_linker_stores_cmsis_dsp_lib() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            LinkerScripts::single(PathBuf::from("/teensy3"), "mk66fx1m0.ld"),
            crate::teensy::mcu_config::get_teensy_config_for_mcu("mk66fx1m0").unwrap(),
            BuildProfile::Release,
            Some(1048576),
            Some(262144),
            Some("arm_cortexM4lf_math".to_string()),
            false,
        );
        assert_eq!(linker.cmsis_dsp_lib(), Some("arm_cortexM4lf_math"));
    }

    /// Regression test for FastLED/fbuild#300: the constructed link command
    /// includes `-larm_cortexM4lf_math` for teensy36 (MK66FX1M0). Mirrors
    /// PlatformIO+Teensyduino's per-MCU auto-link so Teensy `Audio.h` FFT
    /// classes (e.g. `arm_cfft_radix4_q15`) resolve at link time.
    #[test]
    fn test_teensy36_link_command_includes_cmsis_dsp_lib() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            LinkerScripts::single(PathBuf::from("/teensy3"), "mk66fx1m0.ld"),
            crate::teensy::mcu_config::get_teensy_config_for_mcu("mk66fx1m0").unwrap(),
            BuildProfile::Release,
            Some(1048576),
            Some(262144),
            Some("arm_cortexM4lf_math".to_string()),
            false,
        );
        let args = linker.build_link_args(
            &[PathBuf::from("/build/sketch.o")],
            &[PathBuf::from("/build/core.o")],
            &PathBuf::from("/build/firmware.elf"),
            &LinkExtraArgs::default(),
        );
        assert!(
            args.iter().any(|a| a == "-larm_cortexM4lf_math"),
            "teensy36 link command must include -larm_cortexM4lf_math \
             so Audio.h FFT examples link (see fbuild#300). Args: {:?}",
            args
        );
        // The `-L<core_dir>` flag from LinkerScripts is what lets the linker
        // resolve the `-l` to `libarm_cortexM4lf_math.a` inside teensy3/.
        assert!(
            args.iter().any(|a| a == "-L/teensy3"),
            "expected -L/teensy3 for library search, got {:?}",
            args
        );
    }

    /// Regression test: `build_link_args` always emits `-Wl,-Map=` next to
    /// the elf path so debug builds can inspect link decisions. Reverted
    /// previously by an out-of-scope `output_dir` reference (see #313 hotfix).
    #[test]
    fn test_teensy_link_command_emits_linker_map_next_to_elf() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            LinkerScripts::single(PathBuf::from("/teensy4"), "imxrt1062_t41.ld"),
            crate::teensy::mcu_config::get_teensy_config_for_mcu("imxrt1062").unwrap(),
            BuildProfile::Release,
            Some(8126464),
            Some(524288),
            None,
            false,
        );
        let args = linker.build_link_args(
            &[],
            &[],
            &PathBuf::from("/build/firmware.elf"),
            &LinkExtraArgs::default(),
        );
        assert!(
            args.iter().any(|a| a == "-Wl,-Map=/build/firmware.map"),
            "expected -Wl,-Map=/build/firmware.map next to firmware.elf. Args: {:?}",
            args
        );
    }

    /// Boards that do not declare a CMSIS-DSP lib (e.g. user override clears
    /// it) must not have a spurious `-l` argument appended.
    #[test]
    fn test_teensy_link_command_omits_cmsis_dsp_lib_when_none() {
        let linker = TeensyLinker::new(
            PathBuf::from("/bin/arm-none-eabi-gcc"),
            PathBuf::from("/bin/arm-none-eabi-ar"),
            PathBuf::from("/bin/arm-none-eabi-objcopy"),
            PathBuf::from("/bin/arm-none-eabi-size"),
            LinkerScripts::single(PathBuf::from("/teensy4"), "imxrt1062_t41.ld"),
            crate::teensy::mcu_config::get_teensy_config_for_mcu("imxrt1062").unwrap(),
            BuildProfile::Release,
            Some(8126464),
            Some(524288),
            None,
            false,
        );
        let args = linker.build_link_args(
            &[],
            &[],
            &PathBuf::from("/build/firmware.elf"),
            &LinkExtraArgs::default(),
        );
        assert!(
            !args.iter().any(|a| a.starts_with("-larm_cortex")),
            "no CMSIS-DSP -l flag should be appended when cmsis_dsp_lib is None. \
             Args: {:?}",
            args
        );
    }
}
