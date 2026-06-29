//! Compiler traits and base implementation.
//!
//! Defines the `Compiler` trait and `CompilerBase` shared logic for
//! building compiler flags, invoking gcc/g++, and detecting rebuilds.

use fbuild_core::Result;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

// ── Shared config types (used by all platform MCU configs) ──────────────

/// Compiler flags split by language.
#[derive(Debug, Clone, Deserialize)]
pub struct CompilerFlags {
    pub common: Vec<String>,
    pub c: Vec<String>,
    pub cxx: Vec<String>,
}

/// Profile-specific build flags (release, quick).
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileFlags {
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

/// Objcopy configuration for firmware conversion (AVR and Teensy).
#[derive(Debug, Clone, Deserialize)]
pub struct ObjcopyConfig {
    pub output_format: String,
    pub remove_sections: Vec<String>,
}

/// Common interface for platform MCU configurations.
///
/// Provides the minimal surface needed by shared compiler helpers.
/// Platform-specific details (esptool config, compat_defines, etc.) remain
/// on the concrete types.
pub trait McuConfig {
    /// Get the compiler flags (common, C, C++).
    fn compiler_flags(&self) -> &CompilerFlags;

    /// Get profile-specific flags by name (e.g., "release", "quick").
    fn get_profile(&self, name: &str) -> Option<&ProfileFlags>;
}

/// Result of compiling a single source file.
#[derive(Debug)]
pub struct CompileResult {
    pub success: bool,
    pub object_file: PathBuf,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

static COMPILER_IDENTITY_CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

/// Trait for platform-specific compilers.
///
/// FastLED/fbuild#820 (Phase B of #813): `compile_one` is `async` so
/// per-TU zccache dispatch can `.await` `ZccacheService::compile`
/// directly, with no `Handle::block_on`. The default-method bodies
/// (`compile_c` / `compile_cpp` / `compile`) are `async fn` too so
/// they propagate the `.await` to platform-specific `compile_one`
/// impls.
#[async_trait::async_trait]
pub trait Compiler: Send + Sync {
    /// Platform-specific compilation dispatch.
    ///
    /// Routes to `compile_source()` with platform-specific parameters
    /// (temp dir, response file prefix, compiler cache, extra pre-flags).
    async fn compile_one(
        &self,
        compiler_path: &Path,
        source: &Path,
        output: &Path,
        flags: &[String],
        extra_flags: &[String],
    ) -> Result<CompileResult>;

    /// Tokens to strip from every compile command line. Default: none.
    ///
    /// Platforms that want PlatformIO `build_unflags` applied against
    /// framework/toolchain-contributed flags — not just user flags —
    /// override this to return their stored set, typically
    /// `&self.base.build_unflags`. The default `compile_c` / `compile_cpp`
    /// impls below filter both the platform flags AND `extra_flags`
    /// through this set before invoking `compile_one`. See
    /// FastLED/fbuild#37.
    fn build_unflags(&self) -> &[String] {
        &[]
    }

    /// Compile a C source file to an object file.
    async fn compile_c(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.c_flags();
        let (flags, extra) = apply_compile_unflags(flags, extra_flags, self.build_unflags());
        self.compile_one(self.gcc_path(), source, output, &flags, &extra)
            .await
    }

    /// Compile a C++ source file to an object file.
    async fn compile_cpp(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.cpp_flags();
        let (flags, extra) = apply_compile_unflags(flags, extra_flags, self.build_unflags());
        self.compile_one(self.gxx_path(), source, output, &flags, &extra)
            .await
    }

    /// Compile a source file (auto-detect C vs C++).
    async fn compile(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        match ext.as_str() {
            "c" | "s" => self.compile_c(source, output, extra_flags).await,
            _ => self.compile_cpp(source, output, extra_flags).await,
        }
    }

    /// Path to the C compiler (gcc).
    fn gcc_path(&self) -> &Path;

    /// Path to the C++ compiler (g++).
    fn gxx_path(&self) -> &Path;

    /// C compiler flags (without extra_flags).
    fn c_flags(&self) -> Vec<String>;

    /// C++ compiler flags (without extra_flags).
    fn cpp_flags(&self) -> Vec<String>;

    /// Stable fingerprint of the effective compile configuration for one source file.
    ///
    /// Used for incremental rebuild invalidation when flags or compiler paths change.
    fn rebuild_signature(&self, source: &Path, extra_flags: &[String]) -> String {
        let ext = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let (compiler_path, flags) = match ext.as_str() {
            "c" | "s" => (self.gcc_path(), self.c_flags()),
            _ => (self.gxx_path(), self.cpp_flags()),
        };
        build_rebuild_signature(compiler_path, &flags, &[], extra_flags)
    }
}

/// Shared compiler utilities used by all platform-specific compilers.
pub struct CompilerBase {
    pub mcu: String,
    pub f_cpu: String,
    pub defines: HashMap<String, String>,
    pub include_dirs: Vec<PathBuf>,
    pub verbose: bool,
}

impl CompilerBase {
    /// Build `-D` flags from the defines map.
    ///
    /// Flags are sorted by key to ensure deterministic ordering across builds.
    /// This is critical for zccache: non-deterministic flag order causes different
    /// command hashes → 0% cache hit rate.
    pub fn build_define_flags(&self) -> Vec<String> {
        let mut flags: Vec<String> = self
            .defines
            .iter()
            .map(|(k, v)| {
                if v == "1" {
                    format!("-D{}", k)
                } else {
                    format!("-D{}={}", k, v)
                }
            })
            .collect();
        flags.sort();
        flags
    }

    /// Build `-I` flags from include directories.
    pub fn build_include_flags(&self) -> Vec<String> {
        self.include_dirs
            .iter()
            .map(|d| format!("-I{}", d.display()))
            .collect()
    }

    /// Check if a source file needs rebuilding (source newer than object).
    pub fn needs_rebuild(source: &Path, object: &Path) -> bool {
        Self::needs_rebuild_with_signature(source, object, None)
    }

    /// Check if a source file needs rebuilding, optionally accounting for a
    /// fingerprint of the effective compile command.
    pub fn needs_rebuild_with_signature(
        source: &Path,
        object: &Path,
        signature: Option<&str>,
    ) -> bool {
        if !object.exists() {
            return true;
        }

        let obj_time = object
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if let Some(expected) = signature {
            let stamp = command_hash_path(object);
            let actual = std::fs::read_to_string(&stamp).ok();
            if actual.as_deref() != Some(expected) {
                return true;
            }
        }

        let depfile = depfile_path(object);
        if depfile.exists() {
            if dependency_is_newer_than_object(&depfile, obj_time).unwrap_or(true) {
                return true;
            }
            return false;
        }

        let src_time = source
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        src_time > obj_time
    }

    /// Compute the output .o path for a source file.
    pub fn object_path(source: &Path, build_dir: &Path) -> PathBuf {
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let source_ext = source
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(source.to_string_lossy().as_bytes());
            let result = hasher.finalize();
            format!("{:02x}{:02x}", result[0], result[1])
        };
        // Preserve the source extension before `.o` so linker scripts that
        // match `*.cpp.o` / `*.c.o` still route sections correctly.
        if source_ext.is_empty() {
            build_dir.join(format!("{}_{}.o", stem, hash))
        } else {
            build_dir.join(format!("{}_{}.{}.o", stem, hash, source_ext))
        }
    }
}

/// Filter both `flags` and `extra_flags` through `unflags` using the shared
/// PlatformIO-compatible removal semantics in `pipeline::remove_unflagged_tokens`.
/// Returns the filtered pair ready to pass to `compile_one`. Short-circuits
/// when `unflags` is empty so platforms that don't opt in pay no overhead.
/// See FastLED/fbuild#37.
fn apply_compile_unflags(
    flags: Vec<String>,
    extra_flags: &[String],
    unflags: &[String],
) -> (Vec<String>, Vec<String>) {
    if unflags.is_empty() {
        return (flags, extra_flags.to_vec());
    }
    let mut flags = flags;
    let mut extra = extra_flags.to_vec();
    crate::pipeline::remove_unflagged_tokens(&mut flags, unflags);
    crate::pipeline::remove_unflagged_tokens(&mut extra, unflags);
    (flags, extra)
}

fn depfile_path(object: &Path) -> PathBuf {
    object.with_extension("d")
}

fn command_hash_path(object: &Path) -> PathBuf {
    object.with_extension("cmdhash")
}

/// Resolve `path` to an absolute path by joining it with the current working
/// directory if it's relative. Equivalent in intent to `std::path::absolute`
/// (stable in 1.79), written by hand to stay within the workspace MSRV
/// enforced by `clippy.toml` (1.75). Does not canonicalize symlinks or `..`.
/// Falls back to the original path if `current_dir()` fails (e.g. cwd was
/// deleted) — callers should treat that as the path they originally got.
pub fn absolute_from_cwd(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

pub fn build_rebuild_signature(
    compiler_path: &Path,
    flags: &[String],
    pre_flags: &[String],
    extra_flags: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(compiler_identity(compiler_path).as_bytes());
    hasher.update([0]);
    for group in [flags, pre_flags, extra_flags] {
        hash_signature_group(&mut hasher, group);
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

fn hash_signature_group(hasher: &mut Sha256, group: &[String]) {
    let mut expects_path_value = false;
    for flag in group {
        let normalized = if expects_path_value {
            expects_path_value = false;
            normalize_signature_value(flag)
        } else {
            expects_path_value = is_split_path_flag(flag);
            normalize_signature_flag(flag)
        };
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
    }
}

fn is_split_path_flag(flag: &str) -> bool {
    matches!(
        flag,
        "-I" | "-isystem" | "-iquote" | "-include" | "--sysroot"
    )
}

fn normalize_signature_flag(flag: &str) -> String {
    for prefix in ["-I", "-isystem=", "-iquote=", "-include=", "--sysroot="] {
        if let Some(value) = flag.strip_prefix(prefix) {
            return format!("{prefix}{}", normalize_signature_value(value));
        }
    }
    flag.to_string()
}

fn normalize_signature_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let path = Path::new(value);
    if !looks_like_absolute_path(path, value) {
        return value.to_string();
    }
    normalize_signature_path(path)
}

fn normalize_signature_path(path: &Path) -> String {
    let normalized = normalize_signature_components(path);
    if let Some(index) = normalized
        .iter()
        .position(|component| component.eq_ignore_ascii_case(".fbuild"))
    {
        return normalized[index..].join("/");
    }
    if let Some(index) = normalized
        .iter()
        .position(|component| component.eq_ignore_ascii_case(".build"))
    {
        return normalized[index..].join("/");
    }
    const TAIL_COMPONENTS: usize = 2;
    let start = normalized.len().saturating_sub(TAIL_COMPONENTS);
    normalized[start..].join("/")
}

fn normalize_signature_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().replace('\\', "/"))
            }
            Component::RootDir => None,
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::Normal(value) => Some(value.to_string_lossy().replace('\\', "/")),
        })
        .collect()
}

fn looks_like_absolute_path(path: &Path, raw: &str) -> bool {
    path.is_absolute()
        || path.has_root()
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.as_bytes().get(1) == Some(&b':')
}

fn compiler_identity(path: &Path) -> String {
    let cache = COMPILER_IDENTITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(identity) = cache.lock().unwrap().get(path).cloned() {
        return identity;
    }

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    let version = compiler_version(path);
    let identity = format!("{stem}\0{version}");
    cache
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), identity.clone());
    identity
}

fn compiler_version(path: &Path) -> String {
    // FastLED/fbuild#820 (Phase B of #813): `fbuild_core::subprocess::
    // run_command` is now `async`. `compiler_version` is called from
    // the sync `rebuild_signature` trait method (which is in turn
    // called from sync rebuild-check code paths), so we bridge to the
    // ambient tokio runtime via `block_in_place` + `block_on`. This is
    // safe because the daemon runs on a multi-thread tokio runtime and
    // `block_in_place` permits this exact pattern.
    let program = path.to_string_lossy().to_string();
    let result = match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(async {
                let args = [program.as_str(), "-dumpversion"];
                fbuild_core::subprocess::run_command(&args, None, None, None).await
            })
        }),
        Err(_) => {
            // No ambient runtime — happens in unit-test contexts that
            // don't spin up a tokio runtime. Returning an empty version
            // is a graceful degradation: rebuild-signature loses the
            // compiler-version contribution but still encodes path +
            // flags, which is enough for the tests that don't touch a
            // real toolchain.
            return String::new();
        }
    };
    match result {
        Ok(output) if output.success() => output.stdout.trim().to_string(),
        _ => String::new(),
    }
}

fn dependency_is_newer_than_object(
    depfile: &Path,
    object_time: SystemTime,
) -> std::io::Result<bool> {
    let depfile_time = depfile.metadata()?.modified()?;
    if depfile_time > object_time {
        return Ok(true);
    }

    for dependency in parse_depfile_paths(depfile)? {
        let dep_time = std::fs::metadata(&dependency)?.modified()?;
        if dep_time > object_time {
            return Ok(true);
        }
    }

    Ok(false)
}

fn parse_depfile_paths(depfile: &Path) -> std::io::Result<Vec<PathBuf>> {
    let text = std::fs::read_to_string(depfile)?;
    let normalized = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let deps = depfile_dependencies_section(&normalized);

    let mut paths = Vec::new();
    for token in deps.split_whitespace() {
        let unescaped = token.replace("\\ ", " ");
        if !unescaped.is_empty() {
            paths.push(PathBuf::from(OsString::from(unescaped)));
        }
    }
    Ok(paths)
}

fn depfile_dependencies_section(contents: &str) -> &str {
    let bytes = contents.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b':' && bytes[i + 1].is_ascii_whitespace() {
            return &contents[i + 1..];
        }
    }
    contents
}

/// Get the platform-appropriate temp directory for response files.
///
/// Delegates to [`fbuild_core::response_file::windows_temp_dir`].
pub fn windows_temp_dir() -> PathBuf {
    fbuild_core::response_file::windows_temp_dir()
}

/// Write flags to a temporary GCC response file (`@file` syntax).
///
/// Delegates to [`fbuild_core::response_file::write_response_file`].
pub async fn write_response_file(
    flags: &[String],
    temp_dir: &Path,
    prefix: &str,
) -> Result<PathBuf> {
    fbuild_core::response_file::write_response_file(flags, temp_dir, prefix).await
}

/// Response-file directory for a given output object. Used by the
/// per-platform compilers when they construct response files for
/// out-of-band invocation; the embedded `compile_source` path no
/// longer routes through here. Retained because the compiler-tests
/// module exercises it directly and `fbuild-packages` has its own
/// copy for library compiles.
#[allow(dead_code)]
fn response_file_dir(output: &Path, fallback_temp_dir: &Path) -> PathBuf {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("tmp"))
        .unwrap_or_else(|| fallback_temp_dir.to_path_buf())
}

#[allow(dead_code)]
fn invocation_response_file_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Prepare compiler flags for direct execution (no response file).
///
/// Delegates to [`fbuild_core::compiler_flags::prepare_flags_for_exec`].
/// See that module for full documentation.
pub fn prepare_flags_for_exec(flags: Vec<String>) -> Vec<String> {
    fbuild_core::compiler_flags::prepare_flags_for_exec(flags)
}

/// Replace backslashes with forward slashes for GCC response files,
/// but preserve `\"` sequences which are intentional escapes in define values.
///
/// Delegates to [`fbuild_core::response_file::replace_path_backslashes`].
pub fn replace_path_backslashes(s: &str) -> String {
    fbuild_core::response_file::replace_path_backslashes(s)
}

/// Build C flags: common_flags + language-specific C flags from MCU config.
pub fn build_c_flags(common_flags: Vec<String>, config: &dyn McuConfig) -> Vec<String> {
    let mut flags = common_flags;
    flags.extend(config.compiler_flags().c.iter().cloned());
    flags
}

/// Build C++ flags: common_flags + language-specific C++ flags from MCU config.
pub fn build_cpp_flags(common_flags: Vec<String>, config: &dyn McuConfig) -> Vec<String> {
    let mut flags = common_flags;
    flags.extend(config.compiler_flags().cxx.iter().cloned());
    flags
}

/// Compile a single source file through the embedded zccache service.
///
/// Phase 4 stage 2 of FastLED/fbuild#789 (#800): the wrapper-binary
/// `zccache wrap …` path is gone. Every per-TU compile dispatches
/// through `ZccacheService::compile` on the daemon's tokio runtime.
/// The `_compiler_cache: Option<&Path>` parameter is retained for
/// API stability across the per-platform compilers; the value is
/// ignored.
///
/// Platform-specific differences expressed through parameters:
/// - `response_file_prefix`: "avr", "teensy", "esp32" (unused under
///   embedded — the in-process API takes args as `Vec<String>`, no
///   command line).
/// - `extra_pre_flags`: additional flags inserted between base flags
///   and `extra_flags` (ESP32 uses this for include flags deferred
///   from `common_flags`).
#[allow(clippy::too_many_arguments)]
pub async fn compile_source(
    compiler: &Path,
    source: &Path,
    output: &Path,
    flags: &[String],
    extra_flags: &[String],
    _temp_dir: &Path,
    _response_file_prefix: &str,
    verbose: bool,
    _compiler_cache: Option<&Path>,
    extra_pre_flags: &[String],
) -> Result<CompileResult> {
    // #282: a relative `output` (from `fbuild build <relative project_dir>` —
    // what CI does) collides with the asymmetry between
    // `compile_cwd_from_output` (canonicalizes the workspace to absolute) and
    // `path_arg_for_compile_cwd` (short-circuits on relative paths). The
    // resulting gcc invocation would get `cwd = absolute project_dir` plus a
    // relative `-o`, resolving to a doubled path whose parent dir was never
    // created — and `-MMD -MF` fails. Promote both paths to absolute up front
    // so the downstream cwd / `-o` pair stays consistent.
    let source_buf = absolute_from_cwd(source);
    let output_buf = absolute_from_cwd(output);
    let source = source_buf.as_path();
    let output = output_buf.as_path();

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let compile_cwd = crate::zccache::compile_cwd_from_output(output);
    let (source_arg, output_arg) = if let Some(cwd) = compile_cwd.as_deref() {
        (
            crate::zccache::path_arg_for_compile_cwd(source, cwd),
            crate::zccache::path_arg_for_compile_cwd(output, cwd),
        )
    } else {
        (
            source.to_string_lossy().to_string(),
            output.to_string_lossy().to_string(),
        )
    };

    let mut all_flags: Vec<String> = Vec::new();
    if let Some(cwd) = compile_cwd.as_deref() {
        all_flags.extend(crate::zccache::normalize_flags_for_compile_cwd(flags, cwd));
        all_flags.extend(crate::zccache::normalize_flags_for_compile_cwd(
            extra_pre_flags,
            cwd,
        ));
        all_flags.extend(crate::zccache::normalize_flags_for_compile_cwd(
            extra_flags,
            cwd,
        ));
    } else {
        all_flags.extend(flags.iter().cloned());
        all_flags.extend(extra_pre_flags.iter().cloned());
        all_flags.extend(extra_flags.iter().cloned());
    }
    let rebuild_signature = build_rebuild_signature(compiler, flags, extra_pre_flags, extra_flags);
    all_flags.extend(["-c".to_string(), source_arg, "-o".to_string(), output_arg]);

    let global = crate::compile_backend::get_global().ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(
            "compile_backend not installed — fbuild-daemon must call \
             compile_backend::install_global at startup before any compile \
             fires (FastLED/fbuild#800)"
                .to_string(),
        )
    })?;
    let svc = global.service();
    let cwd = compile_cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // Sanitize for direct exec — same pass the wrapper-mode arms used
    // before #800. `prepare_flags_for_exec` un-quotes shell-escaped
    // include args (`-I"path with spaces"` → `-Ipath with spaces`),
    // collapses adjacent `-I path` pairs, and rewrites Windows
    // backslashes in path-bearing flags. Skipping it produced
    // malformed `#include` directives in the ARM/RISC-V CI builds.
    let sanitized = prepare_flags_for_exec(all_flags);

    if verbose {
        tracing::info!(
            "compile (embedded): {} {}",
            compiler.display(),
            sanitized.join(" ")
        );
    }

    // FastLED/fbuild#820 (Phase B of #813): direct `.await` on the
    // async zccache service. The legacy `compile_blocking` path is gone
    // — every per-TU compile is reached through the async build trait
    // chain, so the daemon's tokio runtime drives `ZccacheService::
    // compile` natively.
    let outcome = svc
        .compile(compiler, sanitized, cwd, Vec::new())
        .await
        .map_err(|err| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "embedded zccache compile failed for {}: {err}",
                source.display(),
            ))
        })?;

    if outcome.exit_code == 0 {
        std::fs::write(command_hash_path(output), rebuild_signature)?;
    }

    Ok(CompileResult {
        success: outcome.exit_code == 0,
        object_file: output.to_path_buf(),
        stdout: String::from_utf8_lossy(&outcome.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr).into_owned(),
        exit_code: outcome.exit_code,
    })
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
