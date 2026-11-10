//! Whole-profile install of the pinned ESP-IDF release (FastLED/fbuild#1970).
//!
//! [`esp_idf_release`](super::esp_idf_release) pins one fbuild package per tool
//! component. This module installs a whole [`IdfProfile`] at once and turns the
//! result into the environment a delegated `idf.py` build (#1971) has to hand
//! the subprocess: the `PATH` entries, `ESP_ROM_ELF_DIR`, and
//! `OPENOCD_SCRIPTS`.
//!
//! The point of the profile is that it is *measurable*: [`EspIdfRelease::installed_bytes`]
//! walks the installed tree, so #1970's "`<= 1.3 GB`, asserted against `du`, not
//! against a manifest estimate" acceptance criterion is checked against bytes on
//! disk. The pinned constants in the parent module are what the release *should*
//! cost; this is what it *did* cost.

use std::collections::HashSet;
use std::path::Path;

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::fs::FileIdentity;

use super::esp_idf_release::{EspIdfTool, IdfComponent, IdfProfile};

/// A fully installed profile: one tool root per component, in install order.
#[derive(Debug, Clone)]
pub struct IdfEnvironment {
    roots: Vec<(IdfComponent, NormalizedPath)>,
}

impl IdfEnvironment {
    /// Build an environment from installed tool roots.
    ///
    /// Public because the delegated-build code (#1971) may resolve roots itself
    /// (for example from a caller-supplied cache root); everything below is
    /// derived from these paths and never from a hard-coded layout.
    pub fn from_roots(roots: Vec<(IdfComponent, NormalizedPath)>) -> Self {
        Self { roots }
    }

    /// Every installed root, in install order.
    pub fn tool_roots(&self) -> &[(IdfComponent, NormalizedPath)] {
        &self.roots
    }

    /// The root of one component, if the profile installed it.
    pub fn tool_root(&self, component: IdfComponent) -> Option<&NormalizedPath> {
        self.roots
            .iter()
            .find(|(c, _)| *c == component)
            .map(|(_, root)| root)
    }

    /// Directories a delegated build must see on `PATH`.
    ///
    /// Decided by the archive, not by a table: a component that exports a `bin/`
    /// directory contributes it, and one that does not (`esp-rom-elfs`) is data
    /// the linker reads by absolute path instead. That keeps a future component
    /// pinned in the parent module from silently never reaching `PATH`.
    pub fn path_dirs(&self) -> Vec<NormalizedPath> {
        self.roots
            .iter()
            .map(|(_, root)| root.join("bin"))
            .filter(|bin| bin.as_path().is_dir())
            .collect()
    }

    /// Variables ESP-IDF's CMake layer reads *instead of* `IDF_TOOLS_PATH`.
    ///
    /// Only the entries whose target actually exists are returned: a profile
    /// that installed no OpenOCD must not export a `OPENOCD_SCRIPTS` path that
    /// points at nothing (CMake then fails with an error that blames the board
    /// script, not the missing package).
    pub fn env_vars(&self) -> Vec<(&'static str, NormalizedPath)> {
        let mut vars = Vec::new();
        if let Some(rom_elfs) = self.tool_root(IdfComponent::EspRomElfs) {
            vars.push(("ESP_ROM_ELF_DIR", rom_elfs.clone()));
        }
        if let Some(openocd) = self.tool_root(IdfComponent::OpenocdEsp32) {
            let scripts = openocd.join("share").join("openocd").join("scripts");
            if scripts.as_path().is_dir() {
                vars.push(("OPENOCD_SCRIPTS", scripts));
            }
        }
        vars
    }
}

/// One pinned ESP-IDF release, installed one profile at a time.
pub struct EspIdfRelease {
    profile: IdfProfile,
    tools: Vec<EspIdfTool>,
}

impl EspIdfRelease {
    /// Resolve every component of `profile` against the default cache.
    pub fn new(project_dir: &Path, profile: IdfProfile) -> Result<Self> {
        let tools = profile
            .components()
            .iter()
            .map(|component| EspIdfTool::new(project_dir, *component))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { profile, tools })
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    ///
    /// `#[cfg(test)]` on purpose: an explicit root is how the proof keeps 192 MB
    /// of archives out of the user's real cache. Production resolves the cache
    /// the same way every other package does.
    #[cfg(test)]
    pub(in crate::toolchain) fn with_cache_root(
        project_dir: &Path,
        cache_root: &Path,
        profile: IdfProfile,
    ) -> Result<Self> {
        let host = fbuild_core::platform::host::current();
        let tools = profile
            .components()
            .iter()
            .map(|component| EspIdfTool::with_cache_root(project_dir, cache_root, *component, host))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { profile, tools })
    }

    /// The profile this release installs.
    pub fn profile(&self) -> IdfProfile {
        self.profile
    }

    /// True only when **every** component of the profile is installed: a
    /// profile with three of four tools is not a usable ESP-IDF.
    pub fn is_installed(&self) -> bool {
        self.tools.iter().all(EspIdfTool::is_installed)
    }

    /// Install every component and describe the resulting environment.
    pub async fn install(&self) -> Result<IdfEnvironment> {
        let mut roots = Vec::with_capacity(self.tools.len());
        for tool in &self.tools {
            let root = tool.ensure_normalized().await?;
            roots.push((tool.component(), root));
        }
        Ok(IdfEnvironment::from_roots(roots))
    }

    /// Real bytes of the installed profile, summed over the installed roots.
    ///
    /// Zero for a component that is not installed, so this reports what is on
    /// disk rather than what the pins claim; call [`Self::is_installed`] first
    /// when the distinction matters.
    pub fn installed_bytes(&self) -> u64 {
        // One `seen` set for the whole profile, so a file hardlinked across two
        // components is also counted once.
        let mut seen = HashSet::new();
        self.tools
            .iter()
            .map(|tool| dir_bytes(tool.install_path().as_path(), &mut seen))
            .sum()
    }
}

/// Recursive byte total of `root`, in `du`'s terms: symlinks are not followed
/// and a hardlinked file is counted once.
///
/// The hardlink rule is not a detail. Espressif's cross-toolchains ship their
/// versioned binaries as hardlinks to the unversioned names (60 of them in
/// `xtensa-esp-elf` alone), and counting each directory entry instead reports
/// 1,262 MB for a tree `du -sb` measures at 1,103 MB — enough to make a 1.15 GB
/// profile look like it blew #1970's 1.3 GB budget. Identity comes from the
/// platform facade, so the same rule holds on Windows.
///
/// A dangling entry contributes 0 rather than an error: this is a budget
/// measurement, and a tree that cannot be fully read must not read as "the
/// install failed".
pub(super) fn dir_bytes(root: &Path, seen: &mut HashSet<FileIdentity>) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let Ok(kind) = entry.file_type() else {
                return 0;
            };
            let path = entry.path();
            if kind.is_dir() {
                dir_bytes(&path, seen)
            } else if kind.is_file() {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                // An identity that cannot be taken (permissions, a lock) counts
                // the file: over-reporting a measurement is safe, dropping it
                // would let a budget pass on files it never saw.
                match fbuild_core::platform::fs::file_identity(&path) {
                    Ok(id) => {
                        if seen.insert(id) {
                            len
                        } else {
                            0
                        }
                    }
                    Err(_) => len,
                }
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fabricated_root(base: &Path, component: IdfComponent, with_bin: bool) -> NormalizedPath {
        let root = NormalizedPath::new(base).join(component.id());
        std::fs::create_dir_all(root.as_path()).unwrap();
        if with_bin {
            let bin = root.join("bin");
            std::fs::create_dir_all(bin.as_path()).unwrap();
            std::fs::write(bin.join("tool").as_path(), b"x").unwrap();
        }
        root
    }

    #[test]
    fn every_s3_component_contributes_to_path_except_the_elf_data() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = IdfEnvironment::from_roots(vec![
            (
                IdfComponent::XtensaEspElf,
                fabricated_root(tmp.path(), IdfComponent::XtensaEspElf, true),
            ),
            (
                IdfComponent::Esp32UlpElf,
                fabricated_root(tmp.path(), IdfComponent::Esp32UlpElf, true),
            ),
            (
                IdfComponent::EspRomElfs,
                fabricated_root(tmp.path(), IdfComponent::EspRomElfs, false),
            ),
            (
                IdfComponent::OpenocdEsp32,
                fabricated_root(tmp.path(), IdfComponent::OpenocdEsp32, true),
            ),
        ]);
        let dirs = env.path_dirs();
        assert_eq!(dirs.len(), 3, "rom ELFs export data, not a bin/: {dirs:?}");
        assert!(
            dirs.iter()
                .all(|d| d.as_path().file_name().and_then(|n| n.to_str()) == Some("bin"))
        );
        assert!(env.tool_root(IdfComponent::EspRomElfs).is_some());
        assert!(
            env.tool_root(IdfComponent::EspRomElfs)
                .unwrap()
                .as_path()
                .is_dir()
        );
    }

    #[test]
    fn env_vars_point_at_real_directories_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let openocd = fabricated_root(tmp.path(), IdfComponent::OpenocdEsp32, true);
        let rom_elfs = fabricated_root(tmp.path(), IdfComponent::EspRomElfs, false);

        // Without the scripts tree, no half-valid OPENOCD_SCRIPTS is exported.
        let bare = IdfEnvironment::from_roots(vec![
            (IdfComponent::EspRomElfs, rom_elfs.clone()),
            (IdfComponent::OpenocdEsp32, openocd.clone()),
        ]);
        assert_eq!(
            bare.env_vars().iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["ESP_ROM_ELF_DIR"]
        );

        let scripts = openocd.join("share").join("openocd").join("scripts");
        std::fs::create_dir_all(scripts.as_path()).unwrap();
        let full = IdfEnvironment::from_roots(vec![
            (IdfComponent::EspRomElfs, rom_elfs.clone()),
            (IdfComponent::OpenocdEsp32, openocd.clone()),
        ]);
        let vars = full.env_vars();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].0, "ESP_ROM_ELF_DIR");
        assert_eq!(vars[0].1, rom_elfs);
        assert_eq!(vars[1].0, "OPENOCD_SCRIPTS");
        assert_eq!(vars[1].1, scripts);
    }

    #[test]
    fn a_profile_without_a_component_exports_nothing_for_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = IdfEnvironment::from_roots(vec![(
            IdfComponent::XtensaEspElf,
            fabricated_root(tmp.path(), IdfComponent::XtensaEspElf, true),
        )]);
        assert!(
            env.env_vars().is_empty(),
            "no ROM ELFs -> no ESP_ROM_ELF_DIR"
        );
        assert_eq!(env.path_dirs().len(), 1);
    }

    #[test]
    fn installed_bytes_counts_the_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = NormalizedPath::new(tmp.path()).join("tree");
        std::fs::create_dir_all(root.join("bin").as_path()).unwrap();
        std::fs::create_dir_all(root.join("nested").join("deep").as_path()).unwrap();
        std::fs::write(root.join("bin").join("a").as_path(), vec![0u8; 1000]).unwrap();
        std::fs::write(
            root.join("nested").join("deep").join("b").as_path(),
            vec![0u8; 24],
        )
        .unwrap();
        // A hardlink is a second directory entry for the same bytes; `du`
        // counts it once and so must a budget measurement.
        std::fs::hard_link(
            root.join("bin").join("a").as_path(),
            root.join("nested").join("a-link").as_path(),
        )
        .unwrap();

        let mut seen = HashSet::new();
        assert_eq!(dir_bytes(root.as_path(), &mut seen), 1024);
        let mut empty = HashSet::new();
        assert_eq!(
            dir_bytes(tmp.path().join("missing").as_path(), &mut empty),
            0
        );
    }

    /// Install the whole ESP32-S3 profile from the pinned archives, then check
    /// the three things only a real install can show: the compilers report the
    /// pinned versions, the profile fits #1970's 1.3 GB budget on disk, and a
    /// second install is a no-op (a sentinel file written into the tree
    /// survives, so no component was re-downloaded or re-extracted).
    ///
    /// ~192 MB of downloads; run with
    /// `soldr cargo test -p fbuild-toolchain --lib -- --ignored esp_idf_s3_profile`.
    #[tokio::test]
    #[ignore = "downloads every pinned ESP32-S3 ESP-IDF archive (~192 MB)"]
    async fn esp_idf_s3_profile_installs_every_component_for_real() {
        let dir = tempfile::TempDir::new().unwrap();
        let release =
            EspIdfRelease::with_cache_root(dir.path(), dir.path(), IdfProfile::Esp32S3Only)
                .unwrap();
        assert!(!release.is_installed());

        let env = release.install().await.unwrap();
        assert!(release.is_installed());
        assert_eq!(env.tool_roots().len(), 4);
        assert_eq!(env.path_dirs().len(), 3);

        // The compilers are real binaries at the pinned versions.
        let gcc = env
            .tool_root(IdfComponent::XtensaEspElf)
            .unwrap()
            .join("bin")
            .join("xtensa-esp32-elf-gcc");
        assert!(
            gcc.as_path().is_file(),
            "missing {}",
            gcc.as_path().display()
        );
        // Through the subprocess facade: a raw `Command::output()` in a
        // `crates/*/src/` file is what `ban_raw_subprocess` denies, tests
        // included.
        let out = fbuild_core::subprocess::run_command(
            &[gcc.as_path().to_string_lossy().as_ref(), "--version"],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let text = out.stdout;
        assert!(
            text.contains("esp-14.2.0_20241119"),
            "xtensa GCC is not the pinned release: {text}"
        );

        let rom_elf = env
            .tool_root(IdfComponent::EspRomElfs)
            .unwrap()
            .join("esp32s3_rev0_rom.elf");
        assert!(rom_elf.as_path().is_file(), "missing {rom_elf}");

        // AC2: measured on disk, not estimated from the manifest.
        let bytes = release.installed_bytes();
        eprintln!(
            "esp-idf S3 profile installed {bytes} B ({:.1} MB)",
            bytes as f64 / 1e6
        );
        assert!(
            bytes > 1_000_000_000,
            "measurement looks truncated: {bytes} B"
        );
        assert!(bytes <= 1_300_000_000, "S3 profile measured {bytes} B");

        // AC4: second install is a no-op -- nothing is fetched or re-extracted.
        let sentinel = env
            .tool_root(IdfComponent::EspRomElfs)
            .unwrap()
            .join("agent-sentinel");
        std::fs::write(sentinel.as_path(), b"keep me").unwrap();
        let before = release.installed_bytes();
        let again = release.install().await.unwrap();
        assert!(
            sentinel.as_path().is_file(),
            "a second install re-extracted"
        );
        assert_eq!(again.path_dirs(), env.path_dirs());
        assert_eq!(
            release.installed_bytes(),
            before,
            "a second install changed the installed tree"
        );
    }
}
