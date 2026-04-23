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
pub trait Compiler: Send + Sync {
    /// Platform-specific compilation dispatch.
    ///
    /// Routes to `compile_source()` with platform-specific parameters
    /// (temp dir, response file prefix, compiler cache, extra pre-flags).
    fn compile_one(
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
    fn compile_c(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.c_flags();
        let (flags, extra) = apply_compile_unflags(flags, extra_flags, self.build_unflags());
        self.compile_one(self.gcc_path(), source, output, &flags, &extra)
    }

    /// Compile a C++ source file to an object file.
    fn compile_cpp(
        &self,
        source: &Path,
        output: &Path,
        extra_flags: &[String],
    ) -> Result<CompileResult> {
        let flags = self.cpp_flags();
        let (flags, extra) = apply_compile_unflags(flags, extra_flags, self.build_unflags());
        self.compile_one(self.gxx_path(), source, output, &flags, &extra)
    }

    /// Compile a source file (auto-detect C vs C++).
    fn compile(
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
            "c" | "s" => self.compile_c(source, output, extra_flags),
            _ => self.compile_cpp(source, output, extra_flags),
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
    match std::process::Command::new(path)
        .arg("-dumpversion")
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
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
pub fn write_response_file(flags: &[String], temp_dir: &Path, prefix: &str) -> Result<PathBuf> {
    fbuild_core::response_file::write_response_file(flags, temp_dir, prefix)
}

fn response_file_dir(output: &Path, fallback_temp_dir: &Path) -> PathBuf {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("tmp"))
        .unwrap_or_else(|| fallback_temp_dir.to_path_buf())
}

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

/// Compile a single source file: assemble flags, handle response files, execute.
///
/// This is the shared core of all platform compilers. Platform-specific
/// differences are expressed through parameters:
/// - `response_file_prefix`: "avr", "teensy", "esp32"
/// - `extra_pre_flags`: additional flags inserted between base flags and extra_flags
///   (ESP32 uses this for include flags deferred from common_flags)
/// - `compiler_cache`: optional zccache path (ESP32 only, None for others)
///
/// On Windows, response files are written into a stable `tmp` directory next to
/// the output object so repeated builds can reuse the same path and avoid
/// timestamp churn from ephemeral temp files.
#[allow(clippy::too_many_arguments)]
pub fn compile_source(
    compiler: &Path,
    source: &Path,
    output: &Path,
    flags: &[String],
    extra_flags: &[String],
    temp_dir: &Path,
    response_file_prefix: &str,
    verbose: bool,
    compiler_cache: Option<&Path>,
    extra_pre_flags: &[String],
) -> Result<CompileResult> {
    use fbuild_core::subprocess::run_command;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let compile_cwd = compiler_cache.and_then(|_| crate::zccache::compile_cwd_from_output(output));
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

    // On Windows, write all flags to a response file to avoid command-line
    // length limits and backslash-quote escaping issues with CreateProcessW.
    let args = if cfg!(windows) {
        let response_dir = response_file_dir(output, temp_dir);
        let response_file = write_response_file(&all_flags, &response_dir, response_file_prefix)?;
        let response_file = invocation_response_file_path(&response_file)?;
        let raw_args = [
            compiler.to_string_lossy().to_string(),
            format!("@{}", response_file.display()),
        ];
        let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
        crate::zccache::wrap_args(&raw_refs, compiler_cache)
    } else {
        let sanitized = prepare_flags_for_exec(all_flags);
        let mut raw_args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        raw_args.extend(sanitized);
        let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
        crate::zccache::wrap_args(&raw_refs, compiler_cache)
    };

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    if verbose {
        tracing::info!("compile: {}", args.join(" "));
    }

    let result = run_command(&args_ref, compile_cwd.as_deref(), None, None)?;

    if result.success() {
        std::fs::write(command_hash_path(output), rebuild_signature)?;
    }

    Ok(CompileResult {
        success: result.success(),
        object_file: output.to_path_buf(),
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_define_flags() {
        let base = CompilerBase {
            mcu: "atmega328p".to_string(),
            f_cpu: "16000000L".to_string(),
            defines: {
                let mut d = HashMap::new();
                d.insert("PLATFORMIO".to_string(), "1".to_string());
                d.insert("F_CPU".to_string(), "16000000L".to_string());
                d
            },
            include_dirs: Vec::new(),
            verbose: false,
        };
        let flags = base.build_define_flags();
        assert!(flags.contains(&"-DPLATFORMIO".to_string()));
        assert!(flags.contains(&"-DF_CPU=16000000L".to_string()));
    }

    #[test]
    fn test_build_include_flags() {
        let base = CompilerBase {
            mcu: String::new(),
            f_cpu: String::new(),
            defines: HashMap::new(),
            include_dirs: vec![
                PathBuf::from("/usr/include"),
                PathBuf::from("/opt/avr/include"),
            ],
            verbose: false,
        };
        let flags = base.build_include_flags();
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
    }

    #[test]
    fn test_needs_rebuild_missing_object() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("test.c");
        std::fs::write(&src, "int main() {}").unwrap();
        let obj = tmp.path().join("test.o");
        assert!(CompilerBase::needs_rebuild(&src, &obj));
    }

    #[test]
    fn test_object_path() {
        let path = CompilerBase::object_path(Path::new("main.cpp"), Path::new("/build"));
        assert!(path.starts_with("/build"));
        assert!(path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with("main_"));
        assert_eq!(path.extension().unwrap(), "o");
    }

    #[test]
    fn test_object_path_is_unique_per_source_path() {
        let p1 = CompilerBase::object_path(Path::new("/src/a/main.cpp"), Path::new("/build"));
        let p2 = CompilerBase::object_path(Path::new("/src/b/main.cpp"), Path::new("/build"));
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_object_path_preserves_source_extension_before_o() {
        let cpp = CompilerBase::object_path(Path::new("/src/main.cpp"), Path::new("/build"));
        let c = CompilerBase::object_path(Path::new("/src/startup.c"), Path::new("/build"));
        let asm = CompilerBase::object_path(Path::new("/src/vector.S"), Path::new("/build"));

        assert!(cpp.to_string_lossy().ends_with(".cpp.o"));
        assert!(c.to_string_lossy().ends_with(".c.o"));
        assert!(asm.to_string_lossy().ends_with(".S.o"));
    }

    #[test]
    fn test_prepare_flags_for_exec_strips_escaped_quotes() {
        let flags = vec![
            r#"-DARDUINO_BOARD=\"ESP32_DEV\""#.to_string(),
            r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#.to_string(),
            r#"-DIDF_VER=\"v5.3.2\""#.to_string(),
        ];
        let result = prepare_flags_for_exec(flags);
        assert_eq!(result[0], r#"-DARDUINO_BOARD="ESP32_DEV""#);
        assert_eq!(result[1], r#"-DMBEDTLS_CONFIG_FILE="mbedtls/esp_config.h""#);
        assert_eq!(result[2], r#"-DIDF_VER="v5.3.2""#);
    }

    #[test]
    fn test_prepare_flags_for_exec_preserves_normal_flags() {
        let flags = vec![
            "-DPLATFORMIO".to_string(),
            "-DF_CPU=16000000L".to_string(),
            "-I/usr/include".to_string(),
            "-c".to_string(),
            "-Wall".to_string(),
        ];
        let result = prepare_flags_for_exec(flags.clone());
        assert_eq!(result, flags);
    }

    #[test]
    fn test_prepare_flags_for_exec_empty() {
        let result = prepare_flags_for_exec(Vec::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_prepare_flags_and_response_file_produce_same_define_value() {
        // Both paths must produce the same define value for GCC.
        // Given input: -DFOO=\"bar\"
        // - prepare_flags_for_exec → -DFOO="bar" (argv: GCC sees FOO = "bar")
        // - write_response_file → '-DFOO="bar"' (response file: GCC sees FOO = "bar")
        let input = r#"-DFOO=\"bar\""#.to_string();

        // Direct exec path
        let exec_result = prepare_flags_for_exec(vec![input.clone()]);
        assert_eq!(exec_result[0], r#"-DFOO="bar""#);

        // Response file path
        let tmp = tempfile::TempDir::new().unwrap();
        let rsp = write_response_file(&[input], tmp.path(), "test").unwrap();
        let content = std::fs::read_to_string(rsp).unwrap();
        // Response file wraps in single quotes with unescaped "
        assert_eq!(content, r#"'-DFOO="bar"'"#);
    }

    #[test]
    fn test_response_file_dir_prefers_output_sibling_tmp() {
        let output = Path::new("C:/work/build/src/main.cpp.o");
        let fallback = Path::new("C:/temp");
        assert_eq!(
            response_file_dir(output, fallback),
            PathBuf::from("C:/work/build/src/tmp")
        );
    }

    #[test]
    fn test_response_file_dir_falls_back_without_output_parent() {
        let output = Path::new("main.o");
        let fallback = Path::new("C:/temp");
        assert_eq!(
            response_file_dir(output, fallback),
            PathBuf::from("C:/temp")
        );
    }

    #[test]
    fn test_invocation_response_file_path_makes_relative_path_absolute() {
        let relative = Path::new("build/tmp/test.rsp");
        let absolute = invocation_response_file_path(relative).unwrap();
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(relative));
    }

    #[test]
    fn test_invocation_response_file_path_preserves_absolute_path() {
        let absolute_input = std::env::current_dir().unwrap().join("build/tmp/test.rsp");
        let absolute = invocation_response_file_path(&absolute_input).unwrap();
        assert_eq!(absolute, absolute_input);
    }

    #[test]
    fn test_needs_rebuild_when_depfile_dependency_is_newer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("main.cpp");
        let header = tmp.path().join("config.h");
        let obj = tmp.path().join("main.cpp.o");
        let dep = tmp.path().join("main.cpp.d");

        std::fs::write(&src, "#include \"config.h\"\n").unwrap();
        std::fs::write(&header, "#define X 1\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&obj, "obj").unwrap();
        std::fs::write(
            &dep,
            format!(
                "{}: {} {}\n",
                obj.display(),
                src.display(),
                header.display()
            ),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&header, "#define X 2\n").unwrap();

        assert!(CompilerBase::needs_rebuild(&src, &obj));
    }

    #[test]
    fn test_needs_rebuild_uses_depfile_when_dependencies_are_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("main.cpp");
        let header = tmp.path().join("config.h");
        let obj = tmp.path().join("main.cpp.o");
        let dep = tmp.path().join("main.cpp.d");

        std::fs::write(&src, "#include \"config.h\"\n").unwrap();
        std::fs::write(&header, "#define X 1\n").unwrap();
        std::fs::write(
            &dep,
            format!(
                "{}: {} {}\n",
                obj.display(),
                src.display(),
                header.display()
            ),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&obj, "obj").unwrap();

        assert!(!CompilerBase::needs_rebuild(&src, &obj));
    }

    #[test]
    fn test_needs_rebuild_when_command_hash_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("main.cpp");
        let obj = tmp.path().join("main.cpp.o");
        let stamp = tmp.path().join("main.cpp.cmdhash");

        std::fs::write(&src, "int main() { return 0; }\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&obj, "obj").unwrap();
        std::fs::write(&stamp, "old-signature").unwrap();

        assert!(CompilerBase::needs_rebuild_with_signature(
            &src,
            &obj,
            Some("new-signature")
        ));
    }

    #[test]
    fn test_build_rebuild_signature_ignores_absolute_compiler_path() {
        let flags = vec!["-Os".to_string(), "-mmcu=atmega328p".to_string()];
        let sig_a =
            build_rebuild_signature(Path::new("/opt/toolchains/a/bin/avr-gcc"), &flags, &[], &[]);
        let sig_b = build_rebuild_signature(
            Path::new("/home/runner/.fbuild/packages/toolchain-atmelavr/bin/avr-gcc"),
            &flags,
            &[],
            &[],
        );

        assert_eq!(sig_a, sig_b);
    }

    #[test]
    fn test_build_rebuild_signature_changes_when_compiler_name_changes() {
        let flags = vec!["-Os".to_string()];
        let sig_a =
            build_rebuild_signature(Path::new("/tmp/toolchains/a/bin/avr-gcc"), &flags, &[], &[]);
        let sig_b = build_rebuild_signature(
            Path::new("/tmp/toolchains/a/bin/xtensa-gcc"),
            &flags,
            &[],
            &[],
        );

        assert_ne!(sig_a, sig_b);
    }

    #[test]
    fn test_build_rebuild_signature_ignores_attached_include_root() {
        let flags_a = vec![
            "-I/tmp/ws-a/project/include".to_string(),
            "-I/home/runner/.fbuild/packages/framework-arduinoavr/cores/arduino".to_string(),
        ];
        let flags_b = vec![
            "-I/tmp/ws-b/project/include".to_string(),
            "-I/Users/runner/.fbuild/packages/framework-arduinoavr/cores/arduino".to_string(),
        ];

        let sig_a =
            build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
        let sig_b =
            build_rebuild_signature(Path::new("/tmp/ws-b/tool/bin/avr-gcc"), &flags_b, &[], &[]);

        assert_eq!(sig_a, sig_b);
    }

    #[test]
    fn test_build_rebuild_signature_ignores_split_path_flag_values() {
        let flags_a = vec![
            "-isystem".to_string(),
            "/tmp/ws-a/project/sdk/include".to_string(),
        ];
        let flags_b = vec![
            "-isystem".to_string(),
            "/tmp/ws-b/project/sdk/include".to_string(),
        ];

        let sig_a = build_rebuild_signature(
            Path::new("/tmp/ws-a/tool/bin/xtensa-gcc"),
            &flags_a,
            &[],
            &[],
        );
        let sig_b = build_rebuild_signature(
            Path::new("/tmp/ws-b/tool/bin/xtensa-gcc"),
            &flags_b,
            &[],
            &[],
        );

        assert_eq!(sig_a, sig_b);
    }

    #[test]
    fn test_build_rebuild_signature_changes_when_include_suffix_changes() {
        let flags_a = vec!["-I/tmp/ws-a/project/include".to_string()];
        let flags_b = vec!["-I/tmp/ws-a/project/generated".to_string()];

        let sig_a =
            build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
        let sig_b =
            build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_b, &[], &[]);

        assert_ne!(sig_a, sig_b);
    }

    #[test]
    fn test_build_rebuild_signature_changes_when_non_path_flag_changes() {
        let flags_a = vec!["-Os".to_string()];
        let flags_b = vec!["-O2".to_string()];

        let sig_a =
            build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_a, &[], &[]);
        let sig_b =
            build_rebuild_signature(Path::new("/tmp/ws-a/tool/bin/avr-gcc"), &flags_b, &[], &[]);

        assert_ne!(sig_a, sig_b);
    }
}
