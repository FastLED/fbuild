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
    pub stdout: String,
    pub stderr: String,
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
    fn link(&self, objects: &[PathBuf], archives: &[PathBuf], output: &Path) -> Result<PathBuf>;

    /// Convert ELF to firmware format (.hex, .bin, etc.).
    fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf>;

    /// Report firmware size.
    fn report_size(&self, elf_path: &Path) -> Result<SizeInfo>;

    /// Full link pipeline: archive core → link → convert → size.
    ///
    /// Skips relinking when the existing firmware.elf is newer than all input
    /// objects and archives, saving ~10-14s on incremental builds where only
    /// the compilation step ran (or nothing changed at all).
    fn link_all(
        &self,
        sketch_objects: &[PathBuf],
        core_objects: &[PathBuf],
        output_dir: &Path,
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
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
        }

        // Pass core objects directly to linker (not archived) for LTO compatibility.
        // With LTO + archives, the linker can't see symbols across TUs properly.
        let elf_path = self.link(sketch_objects, core_objects, output_dir)?;

        // Convert
        let firmware_path = self.convert_firmware(&elf_path, output_dir)?;

        // Size
        let size_info = self.report_size(&elf_path).ok();

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
            stdout: String::new(),
            stderr: String::new(),
        })
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
}
