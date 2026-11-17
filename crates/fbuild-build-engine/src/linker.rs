//! Linker traits and base implementation.
//!
//! Defines the `Linker` trait and `LinkerBase` shared logic for
//! collecting object files, invoking ar, objcopy, and size reporting.

use fbuild_core::{Result, SizeInfo};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::perf_log::PerfTimer;

/// Working directory to run a link in.
///
/// Returns the output directory when it — and every cwd-sensitive path
/// argument in `path_args` — is absolute, so that `collect2` / `lto-wrapper`
/// scratch files land in the gitignored build tree instead of the daemon's
/// inherited cwd (normally the user's project root). The user-visible symptom
/// of the inherited cwd was a stray 0-byte file literally named `-r` appearing
/// in the repo root after every clean build
/// (FastLED/fbuild#1267, FastLED/FastLED#3867).
///
/// Returns `None` — keeping the historical inherited-cwd behaviour — as soon as
/// any of those paths is relative, because moving the cwd would also relocate a
/// relative `-o`, `-T`, `-L` or object-file path and silently break the link.
///
/// `path_args` must list exactly the paths the caller passes to the linker that
/// are interpreted relative to the process cwd: object files, archives, linker
/// scripts and library search dirs. It deliberately does *not* scan the raw
/// argv, because linker flag lists legitimately contain bare non-path operands
/// (e.g. the symbol name in `-u app_main`) that would fail the check.
///
/// Bare `-T<name>` script *names* resolved through absolute `-L` search dirs are
/// fine: pass the search dirs, not the name.
///
/// # Example
///
/// ```
/// use std::path::{Path, PathBuf};
/// use fbuild_build_engine::linker::link_cwd_for;
///
/// # #[cfg(unix)] {
/// let out = Path::new("/proj/.fbuild/build/release");
/// let objs = vec![PathBuf::from("/proj/.fbuild/build/release/sketch.o")];
/// assert_eq!(link_cwd_for(out, &objs), Some(out));
///
/// let relative = vec![PathBuf::from("build/sketch.o")];
/// assert_eq!(link_cwd_for(out, &relative), None);
/// # }
/// ```
pub fn link_cwd_for<I, P>(output_dir: &Path, path_args: I) -> Option<&Path>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if !output_dir.is_absolute() {
        return None;
    }
    if path_args.into_iter().any(|p| !p.as_ref().is_absolute()) {
        return None;
    }
    Some(output_dir)
}

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

/// Fail a link whose image cannot fit the target (FastLED/fbuild#1409).
///
/// The linker scripts behind several targets (avr5's default text region is
/// 128 KB, for one) accept an image far larger than the board holds, so
/// exiting 0 here handed callers a firmware that cannot be flashed. Flash
/// overflow is always fatal, matching PlatformIO's `checkprogsize` and
/// arduino-cli; RAM overflow is fatal only when `ram_fatal`, otherwise a
/// warning. `size_info` is `None` when the size tool failed, which is not
/// evidence of overflow.
pub fn enforce_size_limits(size_info: Option<&SizeInfo>, ram_fatal: bool) -> Result<()> {
    let Some(size) = size_info else {
        return Ok(());
    };
    let mut errors = Vec::new();
    if let Some(msg) = size.flash_overflow() {
        errors.push(msg);
    }
    if let Some(msg) = size.ram_overflow() {
        if ram_fatal {
            errors.push(msg);
        } else {
            tracing::warn!("{msg}");
        }
    }
    if errors.is_empty() {
        return Ok(());
    }
    Err(fbuild_core::FbuildError::BuildFailed(format!(
        "firmware does not fit the target: {}",
        errors.join("; ")
    )))
}

/// Additional link arguments resolved outside the platform linker config.
#[derive(Debug, Clone, Default)]
pub struct LinkExtraArgs {
    pub flags: Vec<String>,
    pub libs: Vec<String>,
    /// When true, the linker appends `-Wl,--noinhibit-exec` and treats
    /// link failure as success when `firmware.elf` was emitted (so
    /// over-budget builds can be bloat-analyzed). The link exit code
    /// from `gcc`/`ld` is still non-zero, but the pipeline continues
    /// to symbol analysis instead of bailing.
    ///
    /// See FastLED/fbuild#594.
    pub bloat_analysis: bool,
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

/// Sidecar next to an archive naming the objects it was built from.
fn archive_manifest_path(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".inputs");
    PathBuf::from(name)
}

fn archive_manifest(objects: &[PathBuf]) -> String {
    objects
        .iter()
        .map(|o| o.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Trait for platform-specific linkers.
///
/// FastLED/fbuild#820 (Phase B of #813): every method that invokes a
/// subprocess (`archive`, `link`, `convert_firmware`, `report_size`)
/// is `async fn` so the per-platform linker impls can `.await`
/// `fbuild_core::subprocess::run_command` directly.
#[async_trait::async_trait]
pub trait Linker: Send + Sync {
    /// Create a static archive (.a) from object files.
    async fn archive(&self, objects: &[PathBuf], output: &Path) -> Result<()>;

    /// Link objects and archives into an ELF binary.
    async fn link(
        &self,
        objects: &[PathBuf],
        archives: &[PathBuf],
        output: &Path,
        extra: &LinkExtraArgs,
    ) -> Result<PathBuf>;

    /// Convert ELF to firmware format (.hex, .bin, etc.).
    async fn convert_firmware(&self, elf_path: &Path, output_dir: &Path) -> Result<PathBuf>;

    /// Report firmware size.
    async fn report_size(&self, elf_path: &Path) -> Result<SizeInfo>;

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

    /// Platform-owned link inputs the generic digest can't see, such as a
    /// generated linker script's recipe. A change relinks a reused ELF
    /// (FastLED/fbuild#1736).
    fn link_fingerprint(&self) -> String {
        String::new()
    }

    /// Directory for cross-project link-output reuse, when the platform
    /// supports it (FastLED/fbuild#1894). `None` disables the cache.
    ///
    /// A hit restores `firmware.elf` when the link inputs are byte-identical
    /// to a previous link, which is the case for fresh project directories
    /// after the prefix-map work in #1893. The cache never changes the linked
    /// bytes: a miss always relinks.
    fn link_output_cache_dir(&self) -> Option<&Path> {
        None
    }

    /// Whether static RAM over the board's maximum fails the build.
    ///
    /// Default `false`: like PlatformIO, RAM overflow is a warning, because one
    /// static-RAM figure cannot describe MCUs with several RAM regions (e.g.
    /// Teensy 4 RAM1/`DMAMEM`/`EXTMEM`). Linkers whose figure is the whole of the
    /// MCU's RAM (AVR) override this to fail, as arduino-cli does
    /// (FastLED/fbuild#1409).
    fn ram_overflow_is_fatal(&self) -> bool {
        false
    }

    /// Full link pipeline: archive core → link → convert → size → optional symbol analysis.
    ///
    /// Skips relinking when the existing firmware.elf is newer than all input
    /// objects and archives, saving ~10-14s on incremental builds where only
    /// the compilation step ran (or nothing changed at all).
    ///
    /// `perf` receives `link-elf` (or `link-restore` on a cross-project cache
    /// hit), `link-convert`, `link-size` and `link-symbols` so the caller's
    /// outer link phase is attributable instead of guessed at
    /// (FastLED/fbuild#1894). The middle two run concurrently, so their sum may
    /// exceed the outer phase; the caller's own timing is the truth. When the
    /// platform opts in, a fresh directory restores a content-identical ELF.
    async fn link_all(
        &self,
        sketch_objects: &[PathBuf],
        core_objects: &[PathBuf],
        output_dir: &Path,
        extra: &LinkExtraArgs,
        symbol_analysis: bool,
        perf: &mut PerfTimer,
    ) -> Result<LinkResult> {
        let candidate_elf = output_dir.join("firmware.elf");
        let all_objects: Vec<&PathBuf> = sketch_objects.iter().chain(core_objects).collect();
        let digest = crate::link_digest::link_digest(&all_objects, extra, &self.link_fingerprint());
        if candidate_elf.exists() {
            let can_skip = crate::link_digest::digest_matches(&candidate_elf, &digest)
                && elf_is_up_to_date(&candidate_elf, all_objects.iter().copied());
            if can_skip {
                tracing::info!("link: firmware.elf is up-to-date, skipping relink");
                return finish_from_elf(
                    self,
                    candidate_elf,
                    output_dir,
                    extra,
                    symbol_analysis,
                    perf,
                )
                .await;
            }
        }

        // A fresh directory has no ELF, but its inputs are byte-identical to a
        // previous project's (FastLED/fbuild#1894). Reuse the cached ELF and
        // let the normal convert/size legs below run from it.
        let cache_root = self.link_output_cache_dir();
        let content_key = cache_root
            .filter(|_| !candidate_elf.exists())
            .and_then(|_| {
                let inputs: Vec<&Path> =
                    all_objects.iter().map(|object| object.as_path()).collect();
                crate::link_output_cache::content_key(
                    &inputs,
                    extra,
                    &self.link_fingerprint(),
                    output_dir,
                )
            });
        let restored = cache_root
            .zip(content_key.as_deref())
            .is_some_and(|(root, key)| {
                let started = Instant::now();
                let hit = crate::link_output_cache::restore(root, key, &candidate_elf);
                if hit {
                    perf.record("link-restore", started.elapsed());
                }
                hit
            });

        // Pass core objects directly to linker (not archived) for LTO compatibility.
        // With LTO + archives, the linker can't see symbols across TUs properly.
        let elf_path = if restored {
            crate::link_digest::record_digest(&candidate_elf, &digest);
            candidate_elf.clone()
        } else {
            let map_path = output_dir.join("firmware.map");
            let fresh_map = crate::link_overflow::prepare_map(&map_path);
            let link_started = Instant::now();
            let elf_path = self
                .link(sketch_objects, core_objects, output_dir, extra)
                .await
                .map_err(|error| {
                    crate::link_overflow::enrich(error, fresh_map.then_some(map_path.as_path()))
                })?;
            perf.record("link-elf", link_started.elapsed());
            crate::link_digest::record_digest(&elf_path, &digest);
            if let Some((root, key)) = cache_root.zip(content_key.as_deref()) {
                crate::link_output_cache::store(root, key, &elf_path);
            }
            elf_path
        };

        finish_from_elf(self, elf_path, output_dir, extra, symbol_analysis, perf).await
    }
}

/// Convert, size and optionally analyze an ELF that already exists, then
/// assemble the [`LinkResult`].
///
/// Shared by all three `link_all` outcomes — an up-to-date ELF, a
/// cross-project cache hit, and a fresh link (FastLED/fbuild#1894) — so the
/// post-link legs cannot drift between them.
async fn finish_from_elf<L: Linker + ?Sized>(
    linker: &L,
    elf_path: PathBuf,
    output_dir: &Path,
    extra: &LinkExtraArgs,
    symbol_analysis: bool,
    perf: &mut PerfTimer,
) -> Result<LinkResult> {
    // Convert and size. Both only read the ELF, so they run together instead of
    // back to back on the critical path (FastLED/fbuild#1468).
    let ((firmware_path, convert_elapsed), (size_info, size_elapsed)) = tokio::join!(
        timed(linker.convert_firmware(&elf_path, output_dir)),
        timed(linker.report_size(&elf_path))
    );
    perf.record("link-convert", convert_elapsed);
    perf.record("link-size", size_elapsed);
    let firmware_path = firmware_path?;
    let size_info = size_info.ok();
    if !extra.bloat_analysis {
        enforce_size_limits(size_info.as_ref(), linker.ram_overflow_is_fatal())?;
    }
    let symbol_map = if symbol_analysis {
        let started = Instant::now();
        let map = LinkerBase::analyze_symbols(linker.size_tool_path(), &elf_path)
            .await
            .ok();
        perf.record("link-symbols", started.elapsed());
        map
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

/// Await `future`, returning its output and its own wall-clock duration.
///
/// `link_all` joins the conversion and the size report; timing each leg through
/// this keeps their costs separable (FastLED/fbuild#1894).
async fn timed<F: std::future::Future>(future: F) -> (F::Output, Duration) {
    let started = Instant::now();
    let output = future.await;
    (output, started.elapsed())
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
    pub async fn archive(
        ar_path: &Path,
        objects: &[PathBuf],
        output: &Path,
        tool_label: &str,
    ) -> Result<()> {
        // Preserve the core archiver's 2-minute watchdog (#809).
        fbuild_core::archive::create(
            ar_path,
            objects,
            output,
            tool_label,
            std::time::Duration::from_secs(120),
        )
        .await
    }

    /// Whether `archive` already holds exactly `objects` and no object is newer.
    ///
    /// The object list is compared through a sidecar manifest: an mtime check
    /// alone would keep members of objects that no longer exist, and a stale
    /// member can still satisfy a symbol at link time.
    pub fn archive_is_current(archive: &Path, objects: &[PathBuf]) -> bool {
        match std::fs::read_to_string(archive_manifest_path(archive)) {
            Ok(manifest) => {
                manifest == archive_manifest(objects) && elf_is_up_to_date(archive, objects.iter())
            }
            Err(_) => false,
        }
    }

    /// Rebuild `archive` from `objects` unless [`Self::archive_is_current`]
    /// holds, so an unchanged archive keeps its mtime and the link can skip.
    pub async fn archive_if_stale(
        ar_path: &Path,
        objects: &[PathBuf],
        archive: &Path,
        tool_label: &str,
    ) -> Result<()> {
        if Self::archive_is_current(archive, objects) {
            return Ok(());
        }
        Self::archive(ar_path, objects, archive, tool_label).await?;
        std::fs::write(archive_manifest_path(archive), archive_manifest(objects))?;
        Ok(())
    }

    /// Report firmware size from the ELF section headers.
    ///
    /// Reads the headers directly instead of running the toolchain's `size`
    /// tool: GNU `size`'s Berkeley format counts read-only `SHT_NOBITS`
    /// reservations (Arduino-Pico's `.heap`) as `text`, so its flash figure
    /// was off by the whole heap (FastLED/fbuild#2012). RAM is PlatformIO's
    /// default `.data`/`.bss`/`.noinit` set (FastLED/fbuild#2027); platforms
    /// with their own set use [`crate::elf_size::report_size_with_ram`].
    pub fn report_size(
        elf_path: &Path,
        max_flash: Option<u64>,
        max_ram: Option<u64>,
    ) -> Result<SizeInfo> {
        let ram = fbuild_core::PLATFORMIO_RAM_SECTIONS;
        crate::elf_size::report_size_with_ram(elf_path, ram, max_flash, max_ram)
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
    pub async fn analyze_symbols(
        size_path: &Path,
        elf_path: &Path,
    ) -> Result<fbuild_core::SymbolMap> {
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
        // FastLED/fbuild#809: bound the symbol-map analysis nm invocation.
        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(30)),
        )
        .await?;

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
    pub async fn objcopy_firmware(
        objcopy_path: &Path,
        elf_path: &Path,
        output_dir: &Path,
        config: &crate::compiler::ObjcopyConfig,
        tool_label: &str,
    ) -> Result<PathBuf> {
        Self::objcopy_firmware_with_env(
            objcopy_path,
            elf_path,
            output_dir,
            config,
            tool_label,
            None,
        )
        .await
    }

    /// Output path objcopy writes for `output_format` (`firmware.hex` for
    /// `ihex`, `firmware.bin` otherwise).
    pub fn objcopy_output_path(output_dir: &Path, output_format: &str) -> PathBuf {
        let ext = match output_format {
            "ihex" => "hex",
            _ => "bin",
        };
        output_dir.join(format!("firmware.{ext}"))
    }

    /// The objcopy argv (tool first) that converts `elf_path` to `out_path`.
    pub fn objcopy_args(
        objcopy_path: &Path,
        elf_path: &Path,
        out_path: &Path,
        config: &crate::compiler::ObjcopyConfig,
    ) -> Vec<String> {
        let mut args = vec![
            objcopy_path.to_string_lossy().to_string(),
            "-O".to_string(),
            config.output_format.clone(),
        ];
        for section in &config.only_sections {
            args.push("-j".to_string());
            args.push(section.clone());
        }
        for section in &config.remove_sections {
            args.push("-R".to_string());
            args.push(section.clone());
        }
        args.push(elf_path.to_string_lossy().to_string());
        args.push(out_path.to_string_lossy().to_string());
        args
    }

    /// As [`Self::objcopy_firmware`] but with explicit child env vars — used
    /// to run a bare-name objcopy under the CLI caller's PATH instead of the
    /// daemon's spawn-time PATH (FastLED/fbuild#1219). `None` = daemon env.
    pub async fn objcopy_firmware_with_env(
        objcopy_path: &Path,
        elf_path: &Path,
        output_dir: &Path,
        config: &crate::compiler::ObjcopyConfig,
        tool_label: &str,
        env: Option<&[(&str, &str)]>,
    ) -> Result<PathBuf> {
        use fbuild_core::subprocess::run_command;

        let hex_path = Self::objcopy_output_path(output_dir, &config.output_format);
        let args = Self::objcopy_args(objcopy_path, elf_path, &hex_path, config);

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        // FastLED/fbuild#809: objcopy ELF→HEX/BIN bounded at 1 min.
        let result = run_command(
            &args_ref,
            None,
            env,
            Some(std::time::Duration::from_secs(60)),
        )
        .await?;

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
#[path = "linker_tests.rs"]
mod linker_tests;

#[cfg(test)]
#[path = "objcopy_tests.rs"]
mod objcopy_tests;

#[cfg(test)]
#[path = "linker_unit_tests.rs"]
mod tests;
