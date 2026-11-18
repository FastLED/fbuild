//! Actually Portable Executable (APE / cosmocc) launch support.
//!
//! An APE image is simultaneously a Windows PE, a Bourne-shell script, and
//! (via its embedded loader) an ELF/Mach-O program. Windows runs it natively.
//! Unix kernels do not: `execve` returns `ENOEXEC` unless the host registered
//! an APE `binfmt_misc` handler, and `posix_spawn` — what Rust's `Command`
//! uses — never retries through `/bin/sh` the way an interactive shell does.
//! NixOS ships no APE handler, so a bare spawn fails with "Exec format error".
//!
//! fbuild therefore detects the APE magic and launches the image through a
//! loader explicitly: `<loader> <image> <args...>`. fbuild provides the loader
//! itself from the image (see [`prologue`]):
//!
//! * Linux x86_64/aarch64: the gzip'd static ELF loader for the host CPU.
//! * macOS x86_64: the same blob with its embedded Mach-O header moved to
//!   offset 0.
//! * macOS arm64: the image's `ape-m1.c`, compiled once with the host `cc`.
//!
//! Each is validated and installed content-addressed into an owner-only,
//! exec-capable cache directory ([`install`]); on Linux a sealed `memfd` is the
//! last resort. None of this needs `sh`, coreutils, `gzip`, PATH, or a
//! writable temp dir in the child environment.
//!
//! Spawners fbuild does not control (zccache runs the compiler itself) get a
//! host-native path instead, from [`native_executable`].
//!
//! Loader precedence: `FBUILD_APE_LOADER` → loader embedded in the image →
//! `ape` on PATH → well-known `ape` install locations → `/bin/sh` → `sh` on
//! PATH.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;

use crate::path::NormalizedPath;
use std::sync::Mutex;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::host::{self, HostArch, HostPlatform};

pub(crate) mod install;
mod native;
mod prologue;

pub use native::native_executable;

/// Environment variable naming an explicit APE loader (an `ape` binary or a
/// POSIX shell). Read from the child env overlay first, then the process env.
pub const LOADER_ENV: &str = "FBUILD_APE_LOADER";

/// Environment variable naming the preferred directory for loaders extracted
/// from APE images. Consulted before the host defaults; it must be owned by
/// the current user, not group/world-writable, and on an exec-capable mount.
pub const CACHE_DIR_ENV: &str = "FBUILD_APE_CACHE_DIR";

/// Bytes of an image scanned for the shell prologue. Real prologues end
/// within the first ~8 KiB; the cap bounds work on hostile input.
const PROLOGUE_SCAN_BYTES: u64 = 64 * 1024;

/// Upper bound on a compressed or inflated embedded loader (real ones are
/// ~4–5 KiB compressed, ~10 KiB inflated).
const MAX_LOADER_BYTES: u64 = 4 * 1024 * 1024;

/// Leading bytes that identify an APE image. `MZqFpD='` is the standard
/// header (also a valid DOS/PE `MZ` stub), `jartsr='` is the non-Windows
/// variant, and `APEDBG='` is the debug build variant.
const APE_MAGICS: [&[u8; 8]; 3] = [b"MZqFpD='", b"jartsr='", b"APEDBG='"];

/// Well-known install locations for the Cosmopolitan loader, consulted when
/// `ape` is not on PATH (e.g. NixOS service environments with a minimal PATH).
const WELL_KNOWN_LOADERS: [&str; 2] = ["/usr/bin/ape", "/usr/local/bin/ape"];

/// Whether `head` starts with an APE magic.
pub fn is_ape_header(head: &[u8]) -> bool {
    APE_MAGICS.iter().any(|magic| head.starts_with(*magic))
}

/// Whether the file at `path` is an APE image. Unreadable files are not APE.
pub fn is_ape_file(path: &Path) -> bool {
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| is_ape_header(&head))
}

/// How to launch an APE image: run `loader` with `image` as its first argument,
/// followed by the caller's original arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApeLaunch {
    pub loader: NormalizedPath,
    pub image: NormalizedPath,
    /// A private directory holding this loader as `ape`. Spawners prepend it
    /// to the child's `PATH` so APE programs the tool itself spawns (gcc →
    /// `cc1`) find a loader via the prologue's `type ape` — without it those
    /// nested spawns need `sh` + coreutils + a writable temp dir.
    pub ape_path_dir: Option<NormalizedPath>,
}

impl ApeLaunch {
    /// The child's `PATH` with [`Self::ape_path_dir`] prepended, given the
    /// `PATH` it would otherwise get. `None` when there is nothing to add.
    pub fn child_path(&self, inherited: Option<&OsStr>) -> Option<OsString> {
        let dir = self.ape_path_dir.as_ref()?;
        let rest = inherited.filter(|p| !p.is_empty());
        std::env::join_paths(
            std::iter::once(dir.as_path().to_path_buf())
                .chain(rest.into_iter().flat_map(std::env::split_paths)),
        )
        .ok()
    }
}

/// Plan an APE launch for `program` on the current host.
///
/// `cwd` is the child's working directory (used to resolve a relative
/// program path) and `env_overlay` the child's env overlay (consulted for
/// `PATH`, [`LOADER_ENV`] and [`CACHE_DIR_ENV`] before the process
/// environment). Returns `None` when `program` is not an APE image or the
/// host runs APE natively.
pub fn plan_launch(
    program: &OsStr,
    cwd: Option<&Path>,
    env_overlay: Option<&[(&str, &str)]>,
) -> Option<ApeLaunch> {
    let host = host::current();
    if host.is_windows() {
        return None;
    }
    let overlay_var = |key: &str| {
        env_overlay
            .and_then(|vars| vars.iter().rev().find(|(k, _)| *k == key))
            .map(|(_, v)| OsString::from(*v))
            .or_else(|| std::env::var_os(key))
            .filter(|v| !v.is_empty())
    };
    let path_var = overlay_var("PATH");
    let loader_override = overlay_var(LOADER_ENV);
    let cache_dirs = cache_dirs(overlay_var(CACHE_DIR_ENV));
    plan_launch_for(
        host,
        program,
        cwd,
        path_var.as_deref(),
        loader_override.as_deref(),
        &cache_dirs,
    )
}

static DEFAULT_CACHE_ROOT: std::sync::OnceLock<NormalizedPath> = std::sync::OnceLock::new();

/// Register fbuild's own cache directory for APE-derived executables
/// (loaders, `ape` PATH entries, shims). Binaries call this at startup with a
/// directory under `fbuild_paths::get_cache_root()`, which `fbuild-core` can't
/// depend on. The first registration wins.
pub fn set_default_cache_root(dir: impl AsRef<Path>) {
    let _ = DEFAULT_CACHE_ROOT.set(NormalizedPath::new(dir));
}

/// Candidate cache directories, most preferred first: the explicit
/// [`CACHE_DIR_ENV`] value, the registered fbuild cache root, then host
/// defaults (XDG/`~/.cache`, runtime dir; `~/Library/Caches` on macOS).
fn cache_dirs(explicit: Option<OsString>) -> Vec<NormalizedPath> {
    explicit
        .map(NormalizedPath::new)
        .into_iter()
        .chain(DEFAULT_CACHE_ROOT.get().cloned())
        .chain(super::selected::ape::default_loader_dirs())
        .collect()
}

/// [`plan_launch`] with every host input explicit, for deterministic tests.
/// `cache_dirs` are the candidate directories for an extracted loader.
pub fn plan_launch_for(
    host: HostPlatform,
    program: &OsStr,
    cwd: Option<&Path>,
    path_var: Option<&OsStr>,
    loader_override: Option<&OsStr>,
    cache_dirs: &[NormalizedPath],
) -> Option<ApeLaunch> {
    if host.is_windows() {
        return None;
    }
    let image = resolve_program(program, cwd, path_var)?;
    if !is_ape_file(&image) {
        return None;
    }
    let loader = match loader_override.filter(|v| !v.is_empty()) {
        Some(explicit) => resolve_explicit_loader(explicit, path_var)?,
        None => {
            embedded_loader(&image, host, cache_dirs).or_else(|| resolve_host_loader(path_var))?
        }
    };
    let ape_path_dir = ape_path_dir(&loader, cache_dirs);
    Some(ApeLaunch {
        loader,
        image,
        ape_path_dir,
    })
}

/// Resolve `program` to the file the kernel would execute: a path with a
/// separator is taken as-is (relative to `cwd` when given), a bare name is
/// searched on `path_var`.
fn resolve_program(
    program: &OsStr,
    cwd: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Option<NormalizedPath> {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        let resolved = match cwd {
            Some(dir) if path.is_relative() => dir.join(path),
            _ => path.to_path_buf(),
        };
        return std::path::absolute(resolved).ok().map(NormalizedPath::new);
    }
    find_executable_on_path(program, path_var)
}

fn resolve_explicit_loader(explicit: &OsStr, path_var: Option<&OsStr>) -> Option<NormalizedPath> {
    let explicit = Path::new(explicit);
    if explicit.components().count() > 1 || explicit.is_absolute() {
        return Some(NormalizedPath::new(explicit));
    }
    find_executable_on_path(explicit.as_os_str(), path_var)
}

fn resolve_host_loader(path_var: Option<&OsStr>) -> Option<NormalizedPath> {
    find_executable_on_path(OsStr::new("ape"), path_var)
        .or_else(|| first_executable(WELL_KNOWN_LOADERS.iter().map(NormalizedPath::new)))
        .or_else(|| first_executable([NormalizedPath::from("/bin/sh")]))
        .or_else(|| find_executable_on_path(OsStr::new("sh"), path_var))
}

fn find_executable_on_path(name: &OsStr, path_var: Option<&OsStr>) -> Option<NormalizedPath> {
    let dirs = std::env::split_paths(path_var?).filter(|dir| !dir.as_os_str().is_empty());
    first_executable(dirs.map(|dir| NormalizedPath::new(dir.join(name))))
}

fn first_executable(
    candidates: impl IntoIterator<Item = NormalizedPath>,
) -> Option<NormalizedPath> {
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && super::fs::is_executable(&meta))
}

/// Identity of an image (and where its derived files may live) for the
/// memos: a rebuilt image at the same path changes length or mtime and is
/// re-derived.
type ImageKey = (
    NormalizedPath,
    u64,
    Option<SystemTime>,
    HostPlatform,
    Vec<NormalizedPath>,
);

fn image_key(image: &Path, host: HostPlatform, dirs: &[NormalizedPath]) -> Option<ImageKey> {
    let meta = std::fs::metadata(image).ok()?;
    Some((
        NormalizedPath::new(image),
        meta.len(),
        meta.modified().ok(),
        host,
        dirs.to_vec(),
    ))
}

/// Memo of derived executables (loaders, native copies) per image.
struct Memo(Mutex<Option<HashMap<ImageKey, NormalizedPath>>>);

impl Memo {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn get(&self, key: &ImageKey) -> Option<NormalizedPath> {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let hit = guard.as_ref()?.get(key)?.clone();
        // A cache cleaner may have removed it since; re-derive then.
        hit.exists().then_some(hit)
    }

    fn put(&self, key: ImageKey, path: NormalizedPath) {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        guard.get_or_insert_with(HashMap::new).insert(key, path);
    }
}

static LOADER_MEMO: Memo = Memo::new();

/// The host-runnable loader embedded in `image`, installed into the first
/// usable `cache_dirs` entry (or, on Linux, a sealed memfd). `None` when the
/// image carries no valid loader for the host or nothing could be
/// materialized.
fn embedded_loader(
    image: &Path,
    host: HostPlatform,
    cache_dirs: &[NormalizedPath],
) -> Option<NormalizedPath> {
    let key = image_key(image, host, cache_dirs)?;
    if let Some(hit) = LOADER_MEMO.get(&key) {
        return Some(hit);
    }
    let arch = arch_name(host.arch())?;
    let loader = if host.is_linux() {
        let bytes = extract_loader(image, host.arch())?;
        materialize(
            &bytes,
            &format!("ape-loader-linux-{arch}-{}", short_hash(&bytes)),
            cache_dirs,
        )?
    } else if host.is_macos() && host.arch() == HostArch::X86_64 {
        let bytes = extract_macos_x86_64_loader(image)?;
        materialize(
            &bytes,
            &format!("ape-loader-macos-{arch}-{}", short_hash(&bytes)),
            cache_dirs,
        )?
    } else if host.is_macos() && host.arch() == HostArch::Aarch64 {
        compile_macos_aarch64_loader(image, cache_dirs)?
    } else {
        return None;
    };
    LOADER_MEMO.put(key, loader.clone());
    Some(loader)
}

/// A private directory containing `loader` under the name `ape`, for
/// [`ApeLaunch::ape_path_dir`]. Shells are never exposed as `ape`.
fn ape_path_dir(loader: &Path, cache_dirs: &[NormalizedPath]) -> Option<NormalizedPath> {
    if loader.file_name().is_some_and(|n| n == "sh") || super::selected::ape::is_anonymous(loader) {
        return None;
    }
    if loader.file_name().is_some_and(|n| n == "ape") && !super::selected::ape::is_anonymous(loader)
    {
        return loader.parent().map(NormalizedPath::new);
    }
    let bytes = std::fs::read(loader).ok()?;
    if bytes.len() as u64 > MAX_LOADER_BYTES {
        return None;
    }
    let sub = format!("bin-{}", short_hash(&bytes));
    install::install(cache_dirs, Some(&sub), "ape", &bytes)?
        .parent()
        .map(NormalizedPath::new)
}

/// Install `bytes` as an executable named `name` in the first usable cache
/// dir, else as a host anonymous executable (Linux memfd).
pub(crate) fn materialize(
    bytes: &[u8],
    name: &str,
    dirs: &[NormalizedPath],
) -> Option<NormalizedPath> {
    install::install(dirs, None, name, bytes)
        .or_else(|| super::selected::ape::anonymous_executable(bytes, name))
}

fn short_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn arch_name(arch: HostArch) -> Option<&'static str> {
    match arch {
        HostArch::X86_64 => Some("x86_64"),
        HostArch::Aarch64 => Some("aarch64"),
        _ => None,
    }
}

/// ELF `e_machine` for `arch`.
fn elf_machine(arch: HostArch) -> Option<u16> {
    match arch {
        HostArch::X86_64 => Some(62),   // EM_X86_64
        HostArch::Aarch64 => Some(183), // EM_AARCH64
        _ => None,
    }
}

/// An opened image with its parsed prologue.
struct Image {
    file: std::fs::File,
    len: u64,
    prologue: prologue::Prologue,
}

fn open_image(image: &Path) -> Option<Image> {
    let mut file = std::fs::File::open(image).ok()?;
    let len = file.metadata().ok()?.len();
    let mut head = Vec::new();
    (&mut file)
        .take(PROLOGUE_SCAN_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    if !is_ape_header(&head) {
        return None;
    }
    Some(Image {
        file,
        len,
        prologue: prologue::parse(&head),
    })
}

impl Image {
    /// Read `range`, bounded by the file length and [`MAX_LOADER_BYTES`].
    fn read(&mut self, (skip, count): prologue::Range) -> Option<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let end = skip.checked_add(count)?;
        if count == 0 || count > MAX_LOADER_BYTES || end > self.len {
            return None;
        }
        self.file.seek(SeekFrom::Start(skip)).ok()?;
        let mut out = Vec::with_capacity(count as usize);
        (&mut self.file).take(count).read_to_end(&mut out).ok()?;
        (out.len() as u64 == count).then_some(out)
    }

    fn inflate(&mut self, range: prologue::Range) -> Option<Vec<u8>> {
        let gz = self.read(range)?;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(gz.as_slice())
            .take(MAX_LOADER_BYTES + 1)
            .read_to_end(&mut out)
            .ok()?;
        (out.len() as u64 <= MAX_LOADER_BYTES).then_some(out)
    }
}

/// Inflate and validate the Linux ELF loader `image` embeds for `arch`.
pub fn extract_loader(image: &Path, arch: HostArch) -> Option<Vec<u8>> {
    let machine = elf_machine(arch)?;
    let mut img = open_image(image)?;
    let range = img.prologue.linux_loader(arch)?;
    let elf = img.inflate(range)?;
    is_static_elf_for(&elf, machine).then_some(elf)
}

/// The macOS x86_64 loader: the same gzip'd blob as Linux, with the Mach-O
/// header it carries at offset 320 (8 × 64 bytes) moved to offset 0 — what the
/// prologue's `dd if="$t.$$" of="$t.$$" skip=5 count=8 bs=64` does.
pub fn extract_macos_x86_64_loader(image: &Path) -> Option<Vec<u8>> {
    let mut img = open_image(image)?;
    let range = img.prologue.macos_loader_x86_64?;
    let mut bin = img.inflate(range)?;
    if bin.len() < 320 + 512 {
        return None;
    }
    bin.copy_within(320..320 + 512, 0);
    is_macho_x86_64(&bin).then_some(bin)
}

/// The Apple Silicon loader source (`ape-m1.c`) embedded in `image`.
pub fn extract_macos_aarch64_loader_source(image: &Path) -> Option<Vec<u8>> {
    let mut img = open_image(image)?;
    let range = img.prologue.macos_loader_source_aarch64?;
    let src = img.inflate(range)?;
    (std::str::from_utf8(&src).is_ok() && src.windows(4).any(|w| w == b"main")).then_some(src)
}

/// Compile the image's `ape-m1.c` with the host C compiler (Xcode Command
/// Line Tools) into a content-addressed cache entry, as the prologue would,
/// but without depending on `sh`, `dd`, `gzip` or `$TMPDIR`.
fn compile_macos_aarch64_loader(image: &Path, dirs: &[NormalizedPath]) -> Option<NormalizedPath> {
    let src = extract_macos_aarch64_loader_source(image)?;
    let name = format!("ape-loader-macos-aarch64-{}", short_hash(&src));
    let dir = dirs
        .iter()
        .find(|dir| super::fs::ensure_private_dir(dir) && super::fs::mount_allows_exec(dir))?;
    let target = dir.join(&name);
    if is_executable_file(&target) {
        return Some(target);
    }
    // One compile at a time in this process: concurrent first spawns would
    // otherwise all run `cc` (seconds each). Re-check once we hold the lock.
    static COMPILE: Mutex<()> = Mutex::new(());
    let _compiling = COMPILE.lock().unwrap_or_else(|e| e.into_inner());
    if is_executable_file(&target) {
        return Some(target);
    }
    // Unique per attempt, so racing processes never share temp files.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tag = format!(
        "{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let src_path = dir.join(format!(".{name}.{tag}.c"));
    let out_path = dir.join(format!(".{name}.{tag}"));
    std::fs::write(&src_path, &src).ok()?;
    let cc = first_executable([NormalizedPath::from("/usr/bin/cc")])
        .unwrap_or_else(|| NormalizedPath::from("cc"));
    let compiled = crate::subprocess::run_command_blocking(
        &[
            cc.to_str()?,
            "-w",
            "-O",
            "-o",
            out_path.to_str()?,
            src_path.to_str()?,
        ],
        None,
        None,
        Some(std::time::Duration::from_secs(300)),
    );
    let _ = std::fs::remove_file(&src_path);
    let ok = compiled.as_ref().is_ok_and(|out| out.success())
        && std::fs::rename(&out_path, &target).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&out_path);
        if let Ok(out) = compiled {
            tracing::warn!(
                "could not build the Apple Silicon APE loader with {}: {}",
                cc.display(),
                out.stderr.trim()
            );
        }
        return None;
    }
    Some(target)
}

/// A 64-bit little-endian ELF executable for `machine`.
fn is_static_elf_for(elf: &[u8], machine: u16) -> bool {
    elf.len() >= 64
        && elf.len() as u64 <= MAX_LOADER_BYTES
        && elf.starts_with(b"\x7fELF")
        && elf[4] == 2 // ELFCLASS64
        && elf[5] == 1 // ELFDATA2LSB
        // ET_EXEC (x86_64 loader) or ET_DYN (position-independent aarch64 loader)
        && matches!(u16::from_le_bytes([elf[16], elf[17]]), 2 | 3)
        && u16::from_le_bytes([elf[18], elf[19]]) == machine
}

/// A 64-bit Mach-O for x86_64 (`MH_MAGIC_64`, `CPU_TYPE_X86_64`).
fn is_macho_x86_64(bin: &[u8]) -> bool {
    bin.len() >= 32 && bin.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) && bin[4..8] == [7, 0, 0, 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::host::{HostArch, HostOs};

    const LINUX: HostPlatform = HostPlatform::new(HostOs::Linux, HostArch::X86_64);
    const WINDOWS: HostPlatform = HostPlatform::new(HostOs::Windows, HostArch::X86_64);

    /// Minimal APE-shaped script: the `MZqFpD='...'` header is a shell
    /// assignment, exactly like a real cosmocc image's prologue.
    const FAKE_APE: &str = "MZqFpD='\n'\necho fake-ape \"$@\"\n";

    fn write_exe(dir: &Path, name: &str, body: &str) -> NormalizedPath {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        crate::platform::fs::set_executable(&path).unwrap();
        NormalizedPath::new(path)
    }

    #[test]
    fn recognizes_every_ape_magic_and_rejects_other_formats() {
        assert!(is_ape_header(b"MZqFpD='\n\n\0"));
        assert!(is_ape_header(b"jartsr='\n"));
        assert!(is_ape_header(b"APEDBG='\n"));
        assert!(!is_ape_header(b"MZ\x90\0\x03\0\0\0")); // plain PE
        assert!(!is_ape_header(b"\x7fELF\x02\x01\x01\0")); // ELF
        assert!(!is_ape_header(b"#!/bin/sh\n"));
        assert!(!is_ape_header(b"MZqF"));
    }

    #[test]
    fn windows_runs_ape_natively() {
        let dir = tempfile::tempdir().unwrap();
        let ape = write_exe(dir.path(), "tool", FAKE_APE);
        let plan = plan_launch_for(
            WINDOWS,
            ape.as_os_str(),
            None,
            None,
            Some(OsStr::new("/bin/sh")),
            &[],
        );
        assert_eq!(plan, None);
    }

    #[test]
    fn non_ape_program_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_exe(dir.path(), "tool", "#!/bin/sh\necho hi\n");
        let plan = plan_launch_for(
            LINUX,
            script.as_os_str(),
            None,
            None,
            Some(OsStr::new("/bin/sh")),
            &[],
        );
        assert_eq!(plan, None);
    }

    #[test]
    fn explicit_loader_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let ape = write_exe(dir.path(), "tool", FAKE_APE);
        let plan = plan_launch_for(
            LINUX,
            ape.as_os_str(),
            None,
            None,
            Some(OsStr::new("/opt/cosmo/ape")),
            &[],
        )
        .expect("APE must be planned");
        assert_eq!(plan.loader, NormalizedPath::from("/opt/cosmo/ape"));
        assert_eq!(plan.image, ape);
    }

    #[test]
    fn bare_name_resolves_on_path_and_prefers_ape_loader_on_path() {
        let bin = tempfile::tempdir().unwrap();
        let ape = write_exe(bin.path(), "tool", FAKE_APE);
        let loader = write_exe(bin.path(), "ape", "#!/bin/sh\n");
        let plan = plan_launch_for(
            LINUX,
            OsStr::new("tool"),
            None,
            Some(bin.path().as_os_str()),
            None,
            &[],
        )
        .expect("APE on PATH must be planned");
        assert_eq!(plan.image, ape);
        assert_eq!(plan.loader, loader);
    }

    #[test]
    fn relative_program_resolves_against_child_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let ape = write_exe(&dir.path().join("bin"), "tool", FAKE_APE);
        let plan = plan_launch_for(
            LINUX,
            OsStr::new("bin/tool"),
            Some(dir.path()),
            None,
            Some(OsStr::new("/bin/sh")),
            &[],
        )
        .expect("relative APE must be planned");
        assert_eq!(plan.image, ape);
    }

    /// End-to-end through the real spawn path: without loader routing a Unix
    /// `posix_spawn` of an APE fails with ENOEXEC ("Exec format error").
    /// Windows runs an APE image directly (it is a valid PE) under every
    /// spelling fbuild accepts for a package tool.
    #[test]
    fn windows_spawns_real_ape_under_exe_and_com_names() {
        if !host::current().is_windows() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for name in ["tool.exe", "tool.com"] {
            let image = copy_fixture(dir.path(), name);
            assert_eq!(plan_launch(image.as_os_str(), None, None), None);
            let out = crate::subprocess::run_command_blocking(
                &[image.to_str().unwrap(), "on", "windows"],
                None,
                None,
                None,
            )
            .expect("APE must spawn natively on Windows");
            assert!(out.success(), "{name} stderr: {}", out.stderr);
            assert_eq!(out.stdout, "hello world on windows\n", "{name}");
        }
    }

    #[test]
    fn run_command_launches_ape_through_loader() {
        if host::current().is_windows() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ape = write_exe(dir.path(), "tool", FAKE_APE);
        let program = ape.to_str().unwrap();
        let out = crate::subprocess::run_command_blocking(
            &[program, "a", "b c"],
            None,
            Some(&[(LOADER_ENV, "/bin/sh")]),
            None,
        )
        .expect("APE spawn must succeed through the loader");
        assert!(out.success(), "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "fake-ape a b c\n");
    }

    /// The checked-in cosmocc hello-world (`data/ape-hello`).
    fn hello_fixture() -> NormalizedPath {
        NormalizedPath::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("data/ape-hello/hello.com"))
    }

    #[test]
    fn hello_fixture_is_detected_as_ape() {
        assert!(is_ape_file(&hello_fixture()));
    }

    /// A real cosmocc image through the default loader chain (no override).
    /// Every Unix host: Linux uses the embedded ELF loader, macOS x86_64 the
    /// embedded Mach-O loader, Apple Silicon the image's `ape-m1.c` built with `cc`.
    #[test]
    fn real_ape_hello_runs_via_run_command() {
        if host::current().is_windows() {
            return;
        }
        let ape = hello_fixture();
        let out = crate::subprocess::run_command_blocking(
            &[ape.to_str().unwrap(), "from", "fbuild"],
            None,
            None,
            None,
        )
        .expect("real APE must spawn");
        assert!(out.success(), "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "hello world from fbuild\n");
    }

    #[test]
    fn real_ape_hello_runs_via_process_command() {
        if host::current().is_windows() {
            return;
        }
        let mut command = crate::platform::process::command(hello_fixture());
        command.arg("std");
        let mut child = crate::platform::process::spawn_contained(
            &mut command,
            crate::platform::process::ContainedStdio {
                stdout: crate::platform::process::StdioSource::Pipe,
                ..Default::default()
            },
        )
        .expect("real APE must spawn");
        let mut stdout = String::new();
        child
            .take_stdout()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(child.wait().unwrap(), 0);
        assert_eq!(stdout, "hello world std\n");
    }

    fn copy_fixture(dir: &Path, name: &str) -> NormalizedPath {
        let path = dir.join(name);
        std::fs::copy(hello_fixture(), &path).unwrap();
        NormalizedPath::new(path)
    }

    /// Hostile/corrupt images never panic and never yield a loader.
    #[test]
    fn corrupt_images_yield_no_embedded_loader() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::read(hello_fixture()).unwrap();
        let arch = HostArch::X86_64;

        let truncated = dir.path().join("truncated");
        std::fs::write(&truncated, &real[..20_000]).unwrap();
        assert_eq!(extract_loader(&truncated, arch), None);

        let mut garbage = real.clone();
        garbage[275392..275392 + 4180].fill(0xA5);
        let garbled = dir.path().join("garbled");
        std::fs::write(&garbled, &garbage).unwrap();
        assert_eq!(extract_loader(&garbled, arch), None);

        let huge = dir.path().join("huge-range");
        std::fs::write(
            &huge,
            "MZqFpD='\n'\nif [ \"$m\" = x86_64 ]; then\ndd if=\"$o\" skip=18446744073709551615 count=99 bs=1 2>/dev/null | gzip -dc >\"$t.$$\" ||exit\nfi\n",
        )
        .unwrap();
        assert_eq!(extract_loader(&huge, arch), None);

        // A valid gzip of a non-ELF payload is rejected too.
        let mut fake = b"MZqFpD='\n'\nif [ \"$m\" = x86_64 ]; then\ndd if=\"$o\" skip=200 count=COUNT bs=1 2>/dev/null | gzip -dc >\"$t.$$\" ||exit\nfi\n".to_vec();
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(b"#!/bin/sh\necho pwned\n").unwrap();
            enc.finish().unwrap();
        }
        let text = String::from_utf8(fake)
            .unwrap()
            .replace("COUNT", &gz.len().to_string());
        fake = text.into_bytes();
        fake.resize(200, b'\n');
        fake.extend_from_slice(&gz);
        let not_elf = dir.path().join("not-elf");
        std::fs::write(&not_elf, &fake).unwrap();
        assert_eq!(extract_loader(&not_elf, arch), None);
    }

    /// Spawn the fixture in a child environment with no PATH, TMPDIR or HOME,
    /// so neither `ape`, `sh` utilities, nor the prologue's `$TMPDIR` cache can
    /// help: only fbuild's own extracted loader can make this work.
    fn run_hostile(program: &str, cache: &Path, args: &[&str]) -> crate::subprocess::ToolOutput {
        let mut argv = vec![program];
        argv.extend_from_slice(args);
        crate::subprocess::run_command_blocking(
            &argv,
            None,
            Some(&[
                ("PATH", "/nonexistent"),
                ("TMPDIR", "/nonexistent"),
                ("HOME", "/nonexistent"),
                (CACHE_DIR_ENV, cache.to_str().unwrap()),
            ]),
            None,
        )
        .expect("APE must spawn")
    }

    #[test]
    fn real_ape_runs_with_hostile_child_env_using_own_loader() {
        if host::current().is_windows() {
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        let out = run_hostile(
            hello_fixture().to_str().unwrap(),
            cache.path(),
            &["hostile"],
        );
        assert!(out.success(), "stderr: {}", out.stderr);
        assert_eq!(out.stdout, "hello world hostile\n");
        let mut installed: Vec<_> = std::fs::read_dir(cache.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        installed.sort();
        // The loader, plus the same loader exposed as `bin-<hash>/ape` for
        // the child's PATH (nested APE spawns).
        assert_eq!(installed.len(), 2, "{installed:?}");
        assert!(installed[0].starts_with("ape-loader-"), "{installed:?}");
        assert!(installed[1].starts_with("bin-"), "{installed:?}");
        let exposed = cache.path().join(&installed[1]).join("ape");
        assert_eq!(
            std::fs::read(exposed).unwrap(),
            std::fs::read(cache.path().join(&installed[0])).unwrap()
        );
    }

    #[test]
    fn broken_ape_on_path_does_not_hijack_launch() {
        if host::current().is_windows() {
            return;
        }
        let bin = tempfile::tempdir().unwrap();
        write_exe(bin.path(), "ape", "#!/bin/sh\necho hijacked; exit 99\n");
        let image = copy_fixture(bin.path(), "hello");
        let cache = tempfile::tempdir().unwrap();
        let out = crate::subprocess::run_command_blocking(
            &["hello", "ok"],
            None,
            Some(&[
                ("PATH", bin.path().to_str().unwrap()),
                (CACHE_DIR_ENV, cache.path().to_str().unwrap()),
            ]),
            None,
        )
        .expect("bare-name APE must spawn");
        assert_eq!(out.stdout, "hello world ok\n", "stderr: {}", out.stderr);
        assert!(image.exists());
    }

    #[test]
    fn awkward_image_paths_and_args_survive() {
        if host::current().is_windows() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("dir with spaces – ünïcode");
        std::fs::create_dir(&sub).unwrap();
        let image = copy_fixture(&sub, "hello world.com");
        let link = dir.path().join("link-to-hello");
        crate::platform::fs::symlink_file(&image, &link).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let link = NormalizedPath::new(link);
        for program in [&image, &link] {
            let out = run_hostile(
                program.to_str().unwrap(),
                cache.path(),
                &["a b", "", "$HOME", "--assimilate"],
            );
            assert!(out.success(), "stderr: {}", out.stderr);
            assert_eq!(out.stdout, "hello world a b  $HOME --assimilate\n");
        }
        // `--assimilate` is just an argument: the image must be untouched.
        assert_eq!(
            std::fs::read(&image).unwrap(),
            std::fs::read(hello_fixture()).unwrap()
        );
    }

    #[test]
    fn rebuilt_image_at_same_path_is_reextracted() {
        if host::current().is_windows() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let image = copy_fixture(dir.path(), "tool");
        let cache = tempfile::tempdir().unwrap();
        let program = image.to_str().unwrap();
        assert_eq!(
            run_hostile(program, cache.path(), &["1"]).stdout,
            "hello world 1\n"
        );
        // Replace with a non-APE script: must now run natively, not via loader.
        std::fs::write(&image, "#!/bin/sh\necho native \"$@\"\n").unwrap();
        crate::platform::fs::set_executable(&image).unwrap();
        // Just-written script: retry ETXTBSY from sibling tests forking (#1366).
        let out = crate::subprocess::run_command_blocking_retrying_exec_busy(
            &[program, "2"],
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.stdout, "native 2\n");
        // Deleted cache between spawns is re-materialized.
        std::fs::copy(hello_fixture(), &image).unwrap();
        for entry in std::fs::read_dir(cache.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                std::fs::remove_dir_all(path).unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
        assert_eq!(
            run_hostile(program, cache.path(), &["3"]).stdout,
            "hello world 3\n"
        );
    }

    #[test]
    fn stress_many_concurrent_first_spawns() {
        if host::current().is_windows() {
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        let cache_dir = cache.path().join("cold");
        let image = hello_fixture();
        let program = image.to_str().unwrap();
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..48)
                .map(|i| {
                    let cache_dir = &cache_dir;
                    s.spawn(move || {
                        let arg = i.to_string();
                        let out = run_hostile(program, cache_dir, &[&arg]);
                        assert!(out.success(), "spawn {i} stderr: {}", out.stderr);
                        assert_eq!(out.stdout, format!("hello world {i}\n"));
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    }
}
