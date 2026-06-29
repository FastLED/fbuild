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
    gcc_ar_path: PathBuf,
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
        gcc_ar_path: PathBuf,
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
            gcc_ar_path,
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

/// Partition a mixed list of link inputs into (existing archives, raw objects).
///
/// Inputs ending in `.a` (case-sensitive — matches the toolchain convention) are
/// real static archives and should be passed straight to the linker. Everything
/// else (typically `.o` files coming from the framework compile step) is treated
/// as a raw object that needs to be wrapped in an archive so the linker pulls in
/// only members whose strong symbols are referenced.
///
/// Lives at module scope so unit tests can exercise the partition logic without
/// invoking `avr-gcc-ar`.
pub(crate) fn partition_link_inputs(inputs: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (existing_archives, raw_objects): (Vec<PathBuf>, Vec<PathBuf>) = inputs
        .iter()
        .cloned()
        .partition(|p| p.extension().and_then(|e| e.to_str()) == Some("a"));
    (existing_archives, raw_objects)
}

impl AvrLinker {
    /// Build the argv that will be passed to `avr-gcc` for the link step.
    ///
    /// Factored out so it can be unit-tested without invoking the toolchain
    /// (see #305 — assert that `-Wl,-Map=` is present). `archives` is the
    /// already-partitioned archive list (real `.a` files + any framework
    /// archive synthesised by `link()` for #304); this function does not
    /// touch the filesystem.
    fn build_link_args(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        output_dir: &Path,
        elf_path: &Path,
        extra: &LinkExtraArgs,
    ) -> Vec<String> {
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

        // Always emit a linker map next to firmware.elf for debugging (#305).
        let map_path = output_dir.join("firmware.map");
        args.push(format!("-Wl,-Map={}", map_path.to_string_lossy()));

        // Sketch objects first
        for obj in objects {
            args.push(obj.to_string_lossy().to_string());
        }

        // Archives (already partitioned by `link()`): the framework archive
        // synthesised from raw `.o` framework objects + any real `.a` files
        // passed in. Archive-member selection drops unreferenced members so
        // unused-but-not-eliminable ISRs (e.g. Tone.cpp __vector_1/3 on AVR)
        // no longer get pulled in. See FastLED/fbuild#304.
        for archive in archives {
            args.push(archive.to_string_lossy().to_string());
        }

        // Group for circular deps + libraries from config
        args.push("-Wl,--start-group".to_string());
        args.extend(self.mcu_config.linker_libs.iter().cloned());
        args.extend(extra.libs.iter().cloned());
        args.push("-Wl,--end-group".to_string());

        args
    }
}

#[async_trait::async_trait]
impl Linker for AvrLinker {
    async fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()> {
        crate::linker::LinkerBase::archive(&self.ar_path, objects, output, "avr-ar").await
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

        // Partition: real `.a` archives are pass-through; raw `.o` files get
        // archived into `libframework.a` so the linker only pulls in members
        // whose strong symbols are referenced. Without this, unused-but-not-
        // eliminable ISRs (e.g. Tone.cpp __vector_1/__vector_3 in the AVR
        // framework) are dragged in via weak-symbol override of
        // __vector_default, inflating the binary by ~10% on attiny85.
        // See FastLED/fbuild#304.
        //
        // Use gcc-ar (not plain ar) so the LTO bytecode plugin index is
        // written — preserves -fuse-linker-plugin compatibility, which the
        // default avr.json profile enables via `-flto -fuse-linker-plugin`.
        let (existing_archives, raw_objects) = partition_link_inputs(archives);
        let mut linker_archives: Vec<PathBuf> =
            Vec::with_capacity(existing_archives.len() + usize::from(!raw_objects.is_empty()));
        if !raw_objects.is_empty() {
            let framework_archive = output_dir.join("libframework.a");
            crate::linker::LinkerBase::archive(
                &self.gcc_ar_path,
                &raw_objects,
                &framework_archive,
                "avr-gcc-ar",
            )
            .await?;
            linker_archives.push(framework_archive);
        }
        linker_archives.extend(existing_archives);

        let args = self.build_link_args(objects, &linker_archives, output_dir, &elf_path, extra);

        if self.verbose {
            tracing::debug!(target: "fbuild_build::linker::avr", "link: {}", args.join(" "));
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        // FastLED/fbuild#809: bound the link step at 3 min — generous
        // upper bound for the largest AVR sketches.
        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(180)),
        )
        .await?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "avr-gcc link failed:\n{}",
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
            "avr-objcopy",
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
            "avr-size",
        )
        .await
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
            PathBuf::from("/bin/avr-gcc-ar"),
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
        assert_eq!(linker.gcc_ar_path, PathBuf::from("/bin/avr-gcc-ar"));
    }

    #[test]
    fn link_partitions_archives_and_objects() {
        // Regression test for FastLED/fbuild#304: link() must split mixed
        // .a/.o inputs so .o files are archived (and thus subject to
        // unreferenced-member elimination by the linker) while real .a
        // files pass through unchanged.
        let inputs = vec![
            PathBuf::from("/tmp/Tone.cpp.o"),
            PathBuf::from("/tmp/libfoo.a"),
            PathBuf::from("/tmp/main.cpp.o"),
        ];
        let (archives, objects) = partition_link_inputs(&inputs);
        assert_eq!(archives, vec![PathBuf::from("/tmp/libfoo.a")]);
        assert_eq!(
            objects,
            vec![
                PathBuf::from("/tmp/Tone.cpp.o"),
                PathBuf::from("/tmp/main.cpp.o"),
            ]
        );
    }

    #[test]
    fn partition_handles_all_archives() {
        let inputs = vec![PathBuf::from("/tmp/a.a"), PathBuf::from("/tmp/b.a")];
        let (archives, objects) = partition_link_inputs(&inputs);
        assert_eq!(archives.len(), 2);
        assert!(objects.is_empty());
    }

    #[test]
    fn partition_handles_all_objects() {
        let inputs = vec![PathBuf::from("/tmp/x.o"), PathBuf::from("/tmp/y.o")];
        let (archives, objects) = partition_link_inputs(&inputs);
        assert!(archives.is_empty());
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn partition_handles_empty() {
        let (archives, objects) = partition_link_inputs(&[]);
        assert!(archives.is_empty());
        assert!(objects.is_empty());
    }

    #[test]
    fn partition_treats_unknown_extensions_as_objects() {
        // Defensive: anything that isn't .a (e.g. .lo, .obj, no extension)
        // is treated as an object and archived. This is safer than letting
        // an unknown extension fall through to a raw avr-gcc positional arg.
        let inputs = vec![PathBuf::from("/tmp/weird.lo"), PathBuf::from("/tmp/no_ext")];
        let (archives, objects) = partition_link_inputs(&inputs);
        assert!(archives.is_empty());
        assert_eq!(objects.len(), 2);
    }

    /// #305: every per-platform linker must emit a `firmware.map` next to
    /// `firmware.elf`. Assert the generated argv contains a `-Wl,-Map=` token.
    #[test]
    fn test_avr_link_args_contain_map_flag() {
        let linker = AvrLinker::new(
            PathBuf::from("/bin/avr-gcc"),
            PathBuf::from("/bin/avr-ar"),
            PathBuf::from("/bin/avr-gcc-ar"),
            PathBuf::from("/bin/avr-objcopy"),
            PathBuf::from("/bin/avr-size"),
            "atmega328p",
            get_avr_config().unwrap(),
            BuildProfile::Release,
            Some(32256),
            Some(2048),
            false,
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let output_dir = tmp.path();
        let elf_path = output_dir.join("firmware.elf");
        let extra = LinkExtraArgs::default();
        let args = linker.build_link_args(&[], &[], output_dir, &elf_path, &extra);

        let map_flag = args
            .iter()
            .find(|a| a.starts_with("-Wl,-Map="))
            .expect("link args must contain -Wl,-Map= for firmware.map emission");
        let expected_map = output_dir.join("firmware.map");
        assert!(
            map_flag.contains(&*expected_map.to_string_lossy()),
            "expected map flag to reference {}, got {}",
            expected_map.display(),
            map_flag
        );
    }
}
