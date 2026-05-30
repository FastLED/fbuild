//! Linker traits and base implementation.
//!
//! Defines the `Linker` trait and `LinkerBase` shared logic for
//! collecting object files, invoking ar, objcopy, and size reporting.

use fbuild_core::{Result, SizeInfo};
use std::path::{Path, PathBuf};

/// Result of a link operation.
#[derive(Debug)]
pub struct LinkResult {
    pub success: bool,
    pub elf_path: Option<PathBuf>,
    pub hex_path: Option<PathBuf>,
    pub bin_path: Option<PathBuf>,
    pub size_info: Option<SizeInfo>,
    pub symbol_map: Option<fbuild_core::SymbolMap>,
    pub stdout: String,
    pub stderr: String,
}

/// Additional link arguments resolved outside the platform linker config.
#[derive(Debug, Clone, Default)]
pub struct LinkExtraArgs {
    pub flags: Vec<String>,
    pub libs: Vec<String>,
}

/// Check whether `elf_path` is newer than every file in `inputs`.
///
/// Returns `true` when the ELF can be reused (no input was modified since
/// the last link). Any I/O error conservatively returns `false` (relink).
fn elf_is_up_to_date<'a>(elf_path: &Path, inputs: impl Iterator<Item = &'a PathBuf>) -> bool {
    let elf_mtime = match elf_path.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };
    for input in inputs {
        let input_mtime = match input.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return false,
        };
        if input_mtime >= elf_mtime {
            return false;
        }
    }
    true
}

/// Trait for platform-specific linkers.
pub trait Linker: Send + Sync {
    /// Create a static archive (.a) from object files.
    fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()>;

    /// Link objects and archives into an ELF binary.
    fn link(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        output: &Path,
        extra: &LinkExtraArgs,
    ) -> Result<PathBuf>;

    /// Convert ELF to firmware format (.hex, .bin, etc.).
    fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf>;

    /// Report firmware size.
    fn report_size(&self, elf_path: &Path) -> Result<SizeInfo>;

    /// Path to the platform's size tool (e.g. `arm-none-eabi-size`).
    ///
    /// Used to derive the `nm` tool path for symbol analysis.
    fn size_tool_path(&self) -> &Path;

    /// Path to the platform's `ar` archiver, when the linker exposes one.
    ///
    /// Default: `None`. Per-platform linkers override to expose the binary
    /// they store internally. Consumed by [`crate::build_info::emit_build_info`]
    /// so downstream tools (FastLED `ci/compiled_size.py`, etc.) can locate
    /// the toolchain that produced firmware.elf. See FastLED/fbuild#297.
    fn ar_tool_path(&self) -> Option<&Path> {
        None
    }

    /// Path to the platform's `objcopy`, when the link toolchain has one.
    ///
    /// Default: `None`. Platforms that produce an ELF directly (e.g. ESP8266
    /// for the .elf output, before esptool processing) legitimately return
    /// `None`. See [`Linker::ar_tool_path`].
    fn objcopy_tool_path(&self) -> Option<&Path> {
        None
    }

    /// Path to the link driver — typically the `gcc`/`g++` binary the linker
    /// invokes (not `ld` directly). Used as the `cc_path`/`cxx_path` fallback
    /// when downstream tools need a compiler. Default: `None`.
    fn link_driver_path(&self) -> Option<&Path> {
        None
    }

    /// Full link pipeline: archive core → link → convert → size → optional symbol analysis.
    ///
    /// Skips relinking when the existing firmware.elf is newer than all input
    /// objects and archives, saving ~10-14s on incremental builds where only
    /// the compilation step ran (or nothing changed at all).
    fn link_all(
        &self,
        sketch_objects: &[PathBuf],
        core_objects: &[PathBuf],
        output_dir: &Path,
        extra: &LinkExtraArgs,
        symbol_analysis: bool,
    ) -> Result<LinkResult> {
        let candidate_elf = output_dir.join("firmware.elf");
        if candidate_elf.exists() {
            let can_skip = elf_is_up_to_date(
                &candidate_elf,
                sketch_objects.iter().chain(core_objects.iter()),
            );
            if can_skip {
                tracing::info!("link: firmware.elf is up-to-date, skipping relink");
                let firmware_path = self.convert_firmware(&candidate_elf, output_dir)?;
                let size_info = self.report_size(&candidate_elf).ok();
                let symbol_map = if symbol_analysis {
                    LinkerBase::analyze_symbols(self.size_tool_path(), &candidate_elf).ok()
                } else {
                    None
                };
                let is_hex = firmware_path.extension().is_some_and(|e| e == "hex");
                return Ok(LinkResult {
                    success: true,
                    elf_path: Some(candidate_elf),
                    hex_path: if is_hex {
                        Some(firmware_path.clone())
                    } else {
                        None
                    },
                    bin_path: if !is_hex { Some(firmware_path) } else { None },
                    size_info,
                    symbol_map,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
        }

        // Pass core objects directly to linker (not archived) for LTO compatibility.
        // With LTO + archives, the linker can't see symbols across TUs properly.
        let elf_path = self.link(sketch_objects, core_objects, output_dir, extra)?;

        // Convert
        let firmware_path = self.convert_firmware(&elf_path, output_dir)?;

        // Size
        let size_info = self.report_size(&elf_path).ok();

        // Symbol analysis
        let symbol_map = if symbol_analysis {
            LinkerBase::analyze_symbols(self.size_tool_path(), &elf_path).ok()
        } else {
            None
        };

        let is_hex = firmware_path.extension().is_some_and(|e| e == "hex");
        Ok(LinkResult {
            success: true,
            elf_path: Some(elf_path),
            hex_path: if is_hex {
                Some(firmware_path.clone())
            } else {
                None
            },
            bin_path: if !is_hex { Some(firmware_path) } else { None },
            size_info,
            symbol_map,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Linker script configuration — search directories and script names.
///
/// Platforms resolve linker scripts differently, but the final link command
/// always needs `-L <dir>` and `-T <script>` flags.  This struct captures
/// both patterns:
///
/// - **Simple** (Teensy, ESP8266): one search dir + one board script name,
///   constructed via [`LinkerScripts::single`].
/// - **SDK-provided** (ESP32): pre-built `-L`/`-T` flags from an SDK flags
///   file, constructed via [`LinkerScripts::from_raw_flags`].
///
/// The canonical script name comes from `BoardConfig.ldscript` (populated
/// from PlatformIO board JSON `build.arduino.ldscript`).
#[derive(Debug, Clone, Default)]
pub struct LinkerScripts {
    /// Directories to search for linker scripts (`-L` flags).
    pub search_dirs: Vec<PathBuf>,
    /// Script files to include (`-T` flags).
    pub scripts: Vec<String>,
}

impl LinkerScripts {
    /// Create an empty configuration (no linker scripts).
    pub fn new() -> Self {
        Self::default()
    }

    /// One search directory and one script name.
    ///
    /// Common case for Teensy and ESP8266 where the board JSON names a single
    /// linker script and the framework provides the directory.
    pub fn single(search_dir: PathBuf, script: &str) -> Self {
        Self {
            search_dirs: vec![search_dir],
            scripts: vec![script.to_string()],
        }
    }

    /// Parse pre-built `-L` / `-T` flags into a `LinkerScripts`.
    ///
    /// Used by ESP32 where the SDK provides a flags file containing the full
    /// set of linker script arguments.
    pub fn from_raw_flags(flags: &[String]) -> Self {
        let mut search_dirs = Vec::new();
        let mut scripts = Vec::new();
        let mut i = 0;
        while i < flags.len() {
            let flag = &flags[i];
            if let Some(dir) = flag.strip_prefix("-L") {
                search_dirs.push(PathBuf::from(dir));
            } else if let Some(script) = flag.strip_prefix("-T") {
                if script.is_empty() {
                    // `-T script` (separate args)
                    if i + 1 < flags.len() {
                        scripts.push(flags[i + 1].clone());
                        i += 1;
                    }
                } else {
                    // `-Tscript` (concatenated)
                    scripts.push(script.to_string());
                }
            }
            i += 1;
        }
        Self {
            search_dirs,
            scripts,
        }
    }

    /// Convert to linker command-line arguments.
    ///
    /// Produces `-L{dir}` for each search directory, then `-T{script}` for
    /// each script file.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::with_capacity(self.search_dirs.len() + self.scripts.len());
        for dir in &self.search_dirs {
            args.push(format!("-L{}", dir.display()));
        }
        for script in &self.scripts {
            args.push(format!("-T{script}"));
        }
        args
    }

    /// Returns `true` if no scripts are configured.
    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }
}

/// Shared linker utilities used by all platform-specific linkers.
pub struct LinkerBase {
    pub verbose: bool,
}

impl LinkerBase {
    /// Collect all .o files from a directory.
    pub fn collect_objects(dir: &Path) -> Vec<PathBuf> {
        let mut objects = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "o") {
                    objects.push(path);
                }
            }
        }
        objects.sort();
        objects
    }

    /// Create a static archive (.a) from object files using `ar rcs`.
    pub fn archive(
        ar_path: &Path,
        objects: &[PathBuf],
        output: &Path,
        tool_label: &str,
    ) -> Result<()> {
        use fbuild_core::subprocess::run_command;

        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove existing archive to avoid stale objects
        if output.exists() {
            std::fs::remove_file(output)?;
        }

        let mut args: Vec<String> = vec![
            ar_path.to_string_lossy().to_string(),
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
                "{} failed: {}",
                tool_label, result.stderr
            )));
        }

        Ok(())
    }

    /// Report firmware size by running the size tool and parsing its output.
    pub fn report_size(
        size_path: &Path,
        elf_path: &Path,
        max_flash: Option<u64>,
        max_ram: Option<u64>,
        tool_label: &str,
    ) -> Result<SizeInfo> {
        use fbuild_core::subprocess::run_command;

        let args = [
            size_path.to_string_lossy().to_string(),
            elf_path.to_string_lossy().to_string(),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "{} failed: {}",
                tool_label, result.stderr
            )));
        }

        SizeInfo::parse(&result.stdout, max_flash, max_ram).ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "failed to parse {} output:\n{}",
                tool_label, result.stdout
            ))
        })
    }

    /// Derive the `nm` tool path from the `size` tool path.
    ///
    /// All GCC cross-toolchains use the same prefix convention:
    /// `arm-none-eabi-size` → `arm-none-eabi-nm`, `avr-size` → `avr-nm`.
    pub fn nm_path_from_size_path(size_path: &Path) -> PathBuf {
        let stem = size_path.file_stem().unwrap_or_default().to_string_lossy();
        let nm_stem = if let Some(prefix) = stem.strip_suffix("size") {
            format!("{prefix}nm")
        } else {
            "nm".to_string()
        };
        let parent = size_path.parent().unwrap_or(Path::new("."));
        let ext = size_path.extension().unwrap_or_default();
        if ext.is_empty() {
            parent.join(&nm_stem)
        } else {
            parent.join(format!("{nm_stem}.{}", ext.to_string_lossy()))
        }
    }

    /// Run `nm --print-size --size-sort --reverse-sort` on an ELF and parse the output.
    pub fn analyze_symbols(size_path: &Path, elf_path: &Path) -> Result<fbuild_core::SymbolMap> {
        use fbuild_core::subprocess::run_command;

        let nm_path = Self::nm_path_from_size_path(size_path);
        let args = [
            nm_path.to_string_lossy().to_string(),
            "--print-size".to_string(),
            "--size-sort".to_string(),
            "--reverse-sort".to_string(),
            elf_path.to_string_lossy().to_string(),
        ];
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "nm failed: {}",
                result.stderr
            )));
        }

        fbuild_core::SymbolMap::parse_nm_output(&result.stdout).ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed("nm produced no parseable symbols".into())
        })
    }

    /// Convert ELF to firmware using objcopy (shared by AVR and Teensy).
    pub fn objcopy_firmware(
        objcopy_path: &Path,
        elf_path: &Path,
        output_dir: &Path,
        output_format: &str,
        remove_sections: &[String],
        tool_label: &str,
    ) -> Result<PathBuf> {
        use fbuild_core::subprocess::run_command;

        let ext = match output_format {
            "ihex" => "hex",
            _ => "bin",
        };
        let hex_path = output_dir.join(format!("firmware.{ext}"));

        let mut args = vec![
            objcopy_path.to_string_lossy().to_string(),
            "-O".to_string(),
            output_format.to_string(),
        ];

        for section in remove_sections {
            args.push("-R".to_string());
            args.push(section.clone());
        }

        args.push(elf_path.to_string_lossy().to_string());
        args.push(hex_path.to_string_lossy().to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "{} failed: {}",
                tool_label, result.stderr
            )));
        }

        Ok(hex_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_objects_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let objects = LinkerBase::collect_objects(tmp.path());
        assert!(objects.is_empty());
    }

    #[test]
    fn test_collect_objects() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.o"), "").unwrap();
        std::fs::write(tmp.path().join("helper.o"), "").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "").unwrap();

        let objects = LinkerBase::collect_objects(tmp.path());
        assert_eq!(objects.len(), 2);
        assert!(objects.iter().all(|p| p.extension().unwrap() == "o"));
    }

    #[test]
    fn test_linker_scripts_default_is_empty() {
        let ls = LinkerScripts::new();
        assert!(ls.is_empty());
        assert!(ls.to_args().is_empty());
    }

    #[test]
    fn test_linker_scripts_single() {
        let ls = LinkerScripts::single(PathBuf::from("/sdk/ld"), "eagle.flash.4m1m.ld");
        assert!(!ls.is_empty());
        assert_eq!(ls.search_dirs, vec![PathBuf::from("/sdk/ld")]);
        assert_eq!(ls.scripts, vec!["eagle.flash.4m1m.ld"]);
        assert_eq!(ls.to_args(), vec!["-L/sdk/ld", "-Teagle.flash.4m1m.ld"]);
    }

    #[test]
    fn test_linker_scripts_from_raw_flags_concatenated() {
        let flags = vec![
            "-L/sdk/ld".to_string(),
            "-Tmemory.ld".to_string(),
            "-Tsections.ld".to_string(),
        ];
        let ls = LinkerScripts::from_raw_flags(&flags);
        assert_eq!(ls.search_dirs, vec![PathBuf::from("/sdk/ld")]);
        assert_eq!(ls.scripts, vec!["memory.ld", "sections.ld"]);
        assert_eq!(
            ls.to_args(),
            vec!["-L/sdk/ld", "-Tmemory.ld", "-Tsections.ld"]
        );
    }

    #[test]
    fn test_linker_scripts_from_raw_flags_separated() {
        let flags = vec![
            "-L/sdk/ld".to_string(),
            "-T".to_string(),
            "memory.ld".to_string(),
        ];
        let ls = LinkerScripts::from_raw_flags(&flags);
        assert_eq!(ls.scripts, vec!["memory.ld"]);
    }

    #[test]
    fn test_linker_scripts_from_raw_flags_empty() {
        let ls = LinkerScripts::from_raw_flags(&[]);
        assert!(ls.is_empty());
    }

    #[test]
    fn test_linker_scripts_to_args_order() {
        let ls = LinkerScripts {
            search_dirs: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            scripts: vec!["x.ld".to_string(), "y.ld".to_string()],
        };
        assert_eq!(ls.to_args(), vec!["-L/a", "-L/b", "-Tx.ld", "-Ty.ld"]);
    }

    #[test]
    fn test_nm_path_from_size_path_arm() {
        let size = PathBuf::from("/toolchain/bin/arm-none-eabi-size");
        let nm = LinkerBase::nm_path_from_size_path(&size);
        assert_eq!(nm, PathBuf::from("/toolchain/bin/arm-none-eabi-nm"));
    }

    #[test]
    fn test_nm_path_from_size_path_avr() {
        let size = PathBuf::from("/toolchain/bin/avr-size");
        let nm = LinkerBase::nm_path_from_size_path(&size);
        assert_eq!(nm, PathBuf::from("/toolchain/bin/avr-nm"));
    }

    #[test]
    fn test_nm_path_from_size_path_xtensa() {
        let size = PathBuf::from("/toolchain/bin/xtensa-esp32-elf-size");
        let nm = LinkerBase::nm_path_from_size_path(&size);
        assert_eq!(nm, PathBuf::from("/toolchain/bin/xtensa-esp32-elf-nm"));
    }

    #[test]
    fn test_nm_path_from_size_path_with_exe() {
        let size = PathBuf::from("C:/toolchain/bin/arm-none-eabi-size.exe");
        let nm = LinkerBase::nm_path_from_size_path(&size);
        assert_eq!(nm, PathBuf::from("C:/toolchain/bin/arm-none-eabi-nm.exe"));
    }
}
