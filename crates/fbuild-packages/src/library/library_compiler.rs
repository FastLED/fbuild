//! Library compilation and archiving.
//!
//! Compiles C/C++ source files from downloaded libraries using the ESP32
//! toolchain, then archives the object files into static libraries (.a).

use std::ffi::OsString;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fbuild_core::subprocess::run_command;
use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};

/// C++-only flags that must not be passed to gcc for .c files.
const CXX_ONLY_PREFIXES: &[&str] = &["-std=gnu++", "-std=c++", "-fno-rtti", "-fuse-cxa-atexit"];

/// Optionally prepend zccache to a compiler command line.
fn wrap_compiler_args(args: &[&str], cache_path: Option<&Path>) -> Vec<String> {
    match cache_path {
        Some(zcc) => {
            let mut wrapped = Vec::with_capacity(args.len() + 2);
            wrapped.push(zcc.to_string_lossy().to_string());
            wrapped.push("wrap".to_string());
            wrapped.extend(args.iter().map(|s| s.to_string()));
            wrapped
        }
        None => args.iter().map(|s| s.to_string()).collect(),
    }
}

fn invocation_response_file_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Check if a compiler flag is C++ only.
fn is_cxx_only_flag(flag: &str) -> bool {
    CXX_ONLY_PREFIXES.iter().any(|p| flag.starts_with(p))
}

/// Outcome of a single [`LibCompileBackend::compile`] dispatch.
pub struct LibCompileOutcome {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Pluggable compile dispatch for library TUs (FastLED/fbuild#986).
///
/// The #800 embedded-service migration routed sketch/core compiles through the
/// in-process `ZccacheService`, but library compilation here still shells out
/// via `run_command` (uncached — `compiler_cache` is `None`). `fbuild-packages`
/// cannot depend on `fbuild-build` (which owns the embedded service), so an
/// orchestrator injects the service as a trait object: when a backend is
/// supplied, `compile_one_source` dispatches through it (cached, hits
/// cross-project via #985's project-independent keys) instead of spawning gcc
/// directly. `None` preserves the historical subprocess behavior exactly.
#[async_trait::async_trait]
pub trait LibCompileBackend: Send + Sync {
    /// Compile one TU. `args` are the sanitized flags that follow the compiler
    /// (source/`-o` included); `env` carries the scratch TMP/TEMP + any
    /// `ZCCACHE_WORKTREE_ROOT`.
    async fn compile(
        &self,
        compiler: &Path,
        args: Vec<String>,
        cwd: PathBuf,
        env: Vec<(String, String)>,
    ) -> Result<LibCompileOutcome>;
}

/// Compile all source files in a library and produce a static archive.
///
/// - C files compiled with gcc + C-safe flags (no C++ flags)
/// - C++ files compiled with g++ + full flags
/// - Objects archived into `lib{name}.a`
///
/// Returns the archive path, or None if the library is header-only.
#[allow(clippy::too_many_arguments)]
pub async fn compile_library(
    name: &str,
    source_files: &[PathBuf],
    include_dirs: &[PathBuf],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    output_dir: &Path,
    verbose: bool,
    compiler_cache: Option<&Path>,
) -> Result<Option<PathBuf>> {
    compile_library_with_jobs(
        name,
        source_files,
        include_dirs,
        gcc_path,
        gxx_path,
        ar_path,
        c_flags,
        cpp_flags,
        output_dir,
        verbose,
        1,
        compiler_cache,
        None,
        None,
    )
    .await
}

/// Compile all source files in a library with parallel jobs.
#[allow(clippy::too_many_arguments)]
pub async fn compile_library_with_jobs(
    name: &str,
    source_files: &[PathBuf],
    include_dirs: &[PathBuf],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    output_dir: &Path,
    verbose: bool,
    jobs: usize,
    compiler_cache: Option<&Path>,
    // When `Some(workspace_root)`, compiles run workspace-relative (source
    // / `-o` / project `-I` relativized, cwd = workspace) so their zccache
    // keys are stable across project directories and hit the warm cache
    // (FastLED/fbuild#952). `None` keeps the historical absolute-path,
    // ambient-cwd invocation for every caller that hasn't opted in.
    compile_cwd: Option<PathBuf>,
    // When `Some`, dispatch each TU through the embedded zccache service so lib
    // compiles are cached and hit cross-project (FastLED/fbuild#986). `None`
    // keeps the direct-subprocess path for every caller that hasn't opted in.
    backend: Option<std::sync::Arc<dyn LibCompileBackend>>,
) -> Result<Option<PathBuf>> {
    if source_files.is_empty() {
        tracing::debug!("library {} is header-only, skipping compile", name);
        return Ok(None);
    }

    let obj_dir = output_dir.join("obj");
    std::fs::create_dir_all(&obj_dir)?;

    // Pre-create all output directories (must be done before parallel compilation)
    for source in source_files {
        let obj = object_path(source, &obj_dir);
        if let Some(parent) = obj.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Build include flags once (shared across all compilations)
    let include_flags = build_include_flags(include_dirs, output_dir).await?;

    // Pre-compute C-safe flags once
    let c_safe_flags: Vec<String> = c_flags
        .iter()
        .filter(|f| !is_cxx_only_flag(f))
        .cloned()
        .collect();

    let cpp_flags = cpp_flags.to_vec();

    // When opted in, relativize project-rooted path flags (`-I`, `--sysroot`,
    // ...) to the workspace once. Paths outside the workspace (toolchain,
    // SDK) are already project-independent and pass through unchanged. This
    // keeps the per-TU rebuild signature and the zccache key project-stable
    // (FastLED/fbuild#952).
    let (include_flags, c_safe_flags, cpp_flags) = match compile_cwd.as_deref() {
        Some(cwd) => (
            fbuild_core::path::normalize_flags_for_compile_cwd(&include_flags, cwd),
            fbuild_core::path::normalize_flags_for_compile_cwd(&c_safe_flags, cwd),
            fbuild_core::path::normalize_flags_for_compile_cwd(&cpp_flags, cwd),
        ),
        None => (include_flags, c_safe_flags, cpp_flags),
    };
    let all_objects: Vec<PathBuf> = source_files
        .iter()
        .map(|source| object_path(source, &obj_dir))
        .collect();
    let stale_sources: Vec<PathBuf> = source_files
        .iter()
        .zip(all_objects.iter())
        .filter_map(|(source, obj)| {
            let signature = compile_signature(
                source,
                gcc_path,
                gxx_path,
                &c_safe_flags,
                &cpp_flags,
                &include_flags,
            );
            if object_needs_rebuild(source, obj, &signature).unwrap_or(true) {
                Some(source.clone())
            } else {
                None
            }
        })
        .collect();
    let archive_path = output_dir.join(format!("lib{}.a", name));

    if stale_sources.is_empty()
        && archive_is_up_to_date(&archive_path, &all_objects).unwrap_or(false)
    {
        tracing::info!(
            "library {} is up to date: {} files -> {}",
            name,
            all_objects.len(),
            archive_path.display()
        );
        return Ok(Some(archive_path));
    }

    let jobs = jobs.max(1);

    if jobs <= 1 || stale_sources.len() <= 1 {
        // Sequential path
        for source in &stale_sources {
            compile_one_source(
                source,
                &obj_dir,
                gcc_path,
                gxx_path,
                &c_safe_flags,
                &cpp_flags,
                &include_flags,
                name,
                verbose,
                compiler_cache,
                compile_cwd.as_deref(),
                backend.as_ref(),
            )
            .await?;
        }

        tracing::info!(
            "archiving library {}: {} objects -> {}",
            name,
            all_objects.len(),
            archive_path.display()
        );
        archive_objects(ar_path, &all_objects, &archive_path).await?;
        tracing::info!(
            "compiled library {}: {} changed / {} total files -> {}",
            name,
            stale_sources.len(),
            all_objects.len(),
            archive_path.display()
        );
        return Ok(Some(archive_path));
    }

    // Parallel path — use a tokio Semaphore to bound concurrency.
    let total = stale_sources.len();
    let thread_count = jobs.min(total);
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(thread_count));

    let mut tasks = tokio::task::JoinSet::new();
    let compiled_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let obj_dir_owned = obj_dir.clone();
    let gcc_path_owned = gcc_path.to_path_buf();
    let gxx_path_owned = gxx_path.to_path_buf();
    let c_safe_flags_arc = std::sync::Arc::new(c_safe_flags);
    let cpp_flags_arc = std::sync::Arc::new(cpp_flags.clone());
    let include_flags_arc = std::sync::Arc::new(include_flags.clone());
    let name_owned = name.to_string();
    let compiler_cache_owned = compiler_cache.map(|p| p.to_path_buf());
    let compile_cwd_owned = compile_cwd.clone();
    let backend_owned = backend.clone();

    for source in stale_sources.clone() {
        let sem = sem.clone();
        let obj_dir_t = obj_dir_owned.clone();
        let gcc_t = gcc_path_owned.clone();
        let gxx_t = gxx_path_owned.clone();
        let c_safe_t = c_safe_flags_arc.clone();
        let cpp_t = cpp_flags_arc.clone();
        let inc_t = include_flags_arc.clone();
        let lib_name_t = name_owned.clone();
        let cache_t = compiler_cache_owned.clone();
        let cwd_t = compile_cwd_owned.clone();
        let backend_t = backend_owned.clone();
        let counter = compiled_count.clone();
        tasks.spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|e| FbuildError::BuildFailed(format!("semaphore closed: {e}")))?;
            let cache_ref = cache_t.as_deref();
            compile_one_source(
                &source,
                &obj_dir_t,
                &gcc_t,
                &gxx_t,
                &c_safe_t,
                &cpp_t,
                &inc_t,
                &lib_name_t,
                verbose,
                cache_ref,
                cwd_t.as_deref(),
                backend_t.as_ref(),
            )
            .await?;
            let count = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if count % 20 == 0 || count == total {
                tracing::info!("[{}/{}] compiled [{}]", count, total, lib_name_t);
            }
            Ok::<(), FbuildError>(())
        });
    }

    let mut first_error: Option<String> = None;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
            Err(join_err) => {
                if first_error.is_none() {
                    first_error = Some(format!("compile task panicked: {join_err}"));
                }
            }
        }
    }

    if let Some(error) = first_error {
        return Err(FbuildError::BuildFailed(error));
    }

    let mut all_objects = all_objects;
    all_objects.sort(); // deterministic archive

    // Archive
    tracing::info!(
        "archiving library {}: {} objects -> {}",
        name,
        all_objects.len(),
        archive_path.display()
    );
    archive_objects(ar_path, &all_objects, &archive_path).await?;

    tracing::info!(
        "compiled library {}: {} changed / {} total files ({} threads) -> {}",
        name,
        total,
        all_objects.len(),
        thread_count,
        archive_path.display()
    );

    Ok(Some(archive_path))
}

/// Compile a single source file, returning its object path.
///
/// On Windows, ALL compiler flags are written to a GCC response file (`@file`)
/// to avoid exceeding the 32KB command-line limit (OS error 206).
#[allow(clippy::too_many_arguments)]
async fn compile_one_source(
    source: &Path,
    obj_dir: &Path,
    gcc_path: &Path,
    gxx_path: &Path,
    c_safe_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
    lib_name: &str,
    verbose: bool,
    compiler_cache: Option<&Path>,
    // When `Some`, run the compiler from this workspace root and pass
    // source / `-o` as workspace-relative args, so the zccache key is
    // stable across project directories (FastLED/fbuild#952). When `None`
    // (every non-opted-in caller) the invocation is byte-identical to
    // before: absolute paths, ambient cwd.
    compile_cwd: Option<&Path>,
    // When `Some`, dispatch through the embedded zccache service instead of
    // spawning gcc directly, so lib TUs are cached (FastLED/fbuild#986).
    backend: Option<&std::sync::Arc<dyn LibCompileBackend>>,
) -> Result<PathBuf> {
    let obj = object_path(source, obj_dir);
    let rsp_dir = obj_dir.parent().unwrap_or(obj_dir).join("tmp");

    let is_c = source.extension().map(|e| e == "c").unwrap_or(false);

    let (compiler, flags): (&Path, &[String]) = if is_c {
        (gcc_path, c_safe_flags)
    } else {
        (gxx_path, cpp_flags)
    };
    let rebuild_signature = build_rebuild_signature(compiler, flags, include_flags);

    if verbose {
        tracing::info!(
            "compile [{}]: {}",
            lib_name,
            source.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    // Source and object args: workspace-relative when opted in, else the
    // historical absolute strings.
    let (source_arg, obj_arg) = match compile_cwd {
        Some(cwd) => (
            fbuild_core::path::path_arg_for_compile_cwd(source, cwd),
            fbuild_core::path::path_arg_for_compile_cwd(&obj, cwd),
        ),
        None => (
            source.to_string_lossy().to_string(),
            obj.to_string_lossy().to_string(),
        ),
    };

    // Collect all flags that follow the compiler executable
    let mut all_flags: Vec<String> = Vec::new();
    all_flags.extend_from_slice(flags);
    all_flags.extend_from_slice(include_flags);
    all_flags.extend(["-c".to_string(), source_arg, "-o".to_string(), obj_arg]);

    // Dispatch the compile. With a backend (FastLED/fbuild#986), route through
    // the in-process zccache service — cached, and project-directory-independent
    // via #985 — mirroring fbuild-build's compile_source. Without one, keep the
    // historical direct-subprocess path byte-identical.
    let (success, stderr) = if let Some(backend) = backend {
        let sanitized = fbuild_core::compiler_flags::prepare_flags_for_exec(all_flags);
        // Scratch dir for the compiler's TMP/TEMP (fbuild-owned, off the
        // system temp — mirrors compile_env_for_build usage on the sketch path).
        let scratch = compile_cwd
            .map(Path::to_path_buf)
            .or_else(|| obj.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let mut env = fbuild_core::subprocess::compile_env_for_build(&scratch).unwrap_or_default();
        if let Some(root) = compile_cwd {
            if root.is_dir() {
                env.push((
                    "ZCCACHE_WORKTREE_ROOT".to_string(),
                    root.to_string_lossy().to_string(),
                ));
            }
        }
        let cwd = compile_cwd
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        // No @response-file: the embedded service manages long arg lists itself.
        let outcome = backend.compile(compiler, sanitized, cwd, env).await?;
        (
            outcome.exit_code == 0,
            String::from_utf8_lossy(&outcome.stderr).into_owned(),
        )
    } else {
        // On Windows, put ALL flags in a response file to avoid command-line
        // length limits (OS error 206): `[zccache] <compiler> @response.rsp`.
        let args = if cfg!(windows) {
            let rsp_path = fbuild_core::response_file::write_response_file(
                &all_flags,
                &rsp_dir,
                "lib_compile",
            )
            .await?;
            let rsp_path = invocation_response_file_path(&rsp_path)?;
            let raw_args = [
                compiler.to_string_lossy().to_string(),
                format!("@{}", rsp_path.display()),
            ];
            let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
            wrap_compiler_args(&raw_refs, compiler_cache)
        } else {
            let sanitized = fbuild_core::compiler_flags::prepare_flags_for_exec(all_flags);
            let mut raw_args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
            raw_args.extend(sanitized);
            let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
            wrap_compiler_args(&raw_refs, compiler_cache)
        };
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        // FastLED/fbuild#966: pin worktree_root on the subprocess path too. The
        // env vec must outlive the borrow passed to run_command.
        let worktree_root = compile_cwd.map(|cwd| cwd.to_string_lossy().to_string());
        let env_pairs: Option<Vec<(&str, &str)>> = worktree_root
            .as_deref()
            .map(|root| vec![("ZCCACHE_WORKTREE_ROOT", root)]);
        // FastLED/fbuild#844: explicit 15 min cap — a single cold-cache
        // templated TU is the worst case that still finishes inside it.
        let result = run_command(
            &args_ref,
            compile_cwd,
            env_pairs.as_deref(),
            Some(fbuild_core::time::REAL_BUILD_TIMEOUT),
        )
        .await?;
        (result.success(), result.stderr)
    };

    if !success {
        return Err(FbuildError::BuildFailed(format!(
            "failed to compile {} in library {}:\n{}",
            source.display(),
            lib_name,
            stderr
        )));
    }

    std::fs::write(command_hash_path(&obj), rebuild_signature)?;

    Ok(obj)
}

fn object_needs_rebuild(source: &Path, object: &Path, signature: &str) -> Result<bool> {
    if !object.exists() {
        return Ok(true);
    }

    let object_meta = std::fs::metadata(object)?;
    let object_time = modified_time(&object_meta)?;
    let actual_signature = std::fs::read_to_string(command_hash_path(object)).unwrap_or_default();
    if actual_signature != signature {
        return Ok(true);
    }
    let depfile = object.with_extension("d");
    if depfile.exists() {
        return dependency_is_newer_than_object(&depfile, object_time);
    }

    let source_meta = std::fs::metadata(source)?;
    Ok(object_time < modified_time(&source_meta)?)
}

fn command_hash_path(object: &Path) -> PathBuf {
    object.with_extension("cmdhash")
}

fn compile_signature(
    source: &Path,
    gcc_path: &Path,
    gxx_path: &Path,
    c_safe_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
) -> String {
    let is_c = source.extension().map(|e| e == "c").unwrap_or(false);
    let (compiler, flags): (&Path, &[String]) = if is_c {
        (gcc_path, c_safe_flags)
    } else {
        (gxx_path, cpp_flags)
    };
    build_rebuild_signature(compiler, flags, include_flags)
}

fn build_rebuild_signature(compiler: &Path, flags: &[String], include_flags: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(compiler.to_string_lossy().as_bytes());
    hasher.update([0]);
    for flag in flags {
        hasher.update(flag.as_bytes());
        hasher.update([0]);
    }
    hasher.update([0xff]);
    for flag in include_flags {
        hasher.update(flag.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn dependency_is_newer_than_object(depfile: &Path, object_time: SystemTime) -> Result<bool> {
    let depfile_time = modified_time(&std::fs::metadata(depfile)?)?;
    if depfile_time > object_time {
        return Ok(true);
    }

    for dependency in parse_depfile_paths(depfile)? {
        let dep_time = modified_time(&std::fs::metadata(&dependency)?)?;
        if dep_time > object_time {
            return Ok(true);
        }
    }

    Ok(false)
}

fn parse_depfile_paths(depfile: &Path) -> Result<Vec<PathBuf>> {
    let text = std::fs::read_to_string(depfile).map_err(|e| {
        FbuildError::BuildFailed(format!("failed to read depfile {}: {e}", depfile.display()))
    })?;
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

fn archive_is_up_to_date(archive: &Path, objects: &[PathBuf]) -> Result<bool> {
    if !archive.exists() || objects.is_empty() {
        return Ok(false);
    }

    let archive_time = modified_time(&std::fs::metadata(archive)?)?;
    for object in objects {
        if !object.exists() {
            return Ok(false);
        }
        if archive_time < modified_time(&std::fs::metadata(object)?)? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn modified_time(metadata: &Metadata) -> Result<SystemTime> {
    metadata.modified().map_err(|e| {
        FbuildError::BuildFailed(format!("failed to read file modification time: {e}"))
    })
}

/// Build include flags, using a response file on Windows if needed.
///
/// When there are many include paths (>100), writes a response file.
/// Uses `-iprefix` + `-iwithprefixbefore` for paths sharing a common prefix
/// to keep the total command line under GCC 8.4.0's 32KB CreateProcess limit.
async fn build_include_flags(include_dirs: &[PathBuf], _temp_dir: &Path) -> Result<Vec<String>> {
    let flags: Vec<String> = include_dirs
        .iter()
        .map(|d| format!("-I{}", d.display()))
        .collect();

    if cfg!(windows) && flags.len() > 100 {
        let rsp_dir = _temp_dir.join("tmp");
        let rsp_path =
            fbuild_core::response_file::write_response_file(&flags, &rsp_dir, "lib_includes")
                .await?;
        Ok(vec![format!("@{}", rsp_path.display())])
    } else {
        Ok(flags)
    }
}

/// Create a static archive from object files.
async fn archive_objects(ar_path: &Path, objects: &[PathBuf], output: &Path) -> Result<()> {
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
    // FastLED/fbuild#844: explicit 60 s cap. `ar rcs` over a large
    // FastLED object set (hundreds of .o files) finishes in <10 s on
    // every host fbuild supports; anything past a minute is a disk /
    // filesystem fault rather than legitimately slow work, so we cap
    // tightly to surface those failures fast instead of waiting out
    // the 15 min default.
    let result = run_command(
        &args_ref,
        None,
        None,
        Some(std::time::Duration::from_secs(60)),
    )
    .await?;

    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "ar failed: {}",
            result.stderr
        )));
    }

    Ok(())
}

/// Compute the object file path for a source file.
fn object_path(source: &Path, obj_dir: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    // FastLED/fbuild#966: the disambiguating hash must be
    // project-directory-independent — the object filename becomes the `-o`
    // arg that zccache folds into the per-TU context key, so hashing the
    // absolute source path defeats cross-project cache hits. Hash the source
    // relative to the project workspace (parent of `.fbuild/` in obj_dir);
    // sources outside it (global framework/library cache) keep their absolute
    // path, which is already project-independent.
    let hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(object_hash_key(source, obj_dir).as_bytes());
        let result = hasher.finalize();
        format!("{:02x}{:02x}", result[0], result[1])
    };
    obj_dir.join(format!("{}_{}.o", stem, hash))
}

/// Project-workspace-relative key for the object hash (FastLED/fbuild#966).
fn object_hash_key(source: &Path, obj_dir: &Path) -> String {
    for ancestor in obj_dir.ancestors() {
        if ancestor
            .file_name()
            .map(|n| n == ".fbuild")
            .unwrap_or(false)
        {
            if let Some(workspace) = ancestor.parent() {
                if let Ok(rel) = source.strip_prefix(workspace) {
                    return rel.to_string_lossy().replace('\\', "/");
                }
            }
            break;
        }
    }
    source.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_signature() -> &'static str {
        "test-signature"
    }

    #[test]
    fn test_is_cxx_only_flag() {
        assert!(is_cxx_only_flag("-std=gnu++2b"));
        assert!(is_cxx_only_flag("-std=c++17"));
        assert!(is_cxx_only_flag("-fno-rtti"));
        assert!(is_cxx_only_flag("-fuse-cxa-atexit"));
        assert!(!is_cxx_only_flag("-std=gnu17"));
        assert!(!is_cxx_only_flag("-Os"));
        assert!(!is_cxx_only_flag("-DFOO"));
    }

    #[test]
    fn test_object_path_unique() {
        let obj_dir = Path::new("/tmp/obj");
        let p1 = object_path(Path::new("/src/a/main.cpp"), obj_dir);
        let p2 = object_path(Path::new("/src/b/main.cpp"), obj_dir);
        assert_ne!(
            p1, p2,
            "different source paths should produce different object paths"
        );
    }

    #[test]
    fn test_object_path_extension() {
        let obj_dir = Path::new("/tmp/obj");
        let p = object_path(Path::new("/src/main.cpp"), obj_dir);
        assert_eq!(p.extension().unwrap(), "o");
    }

    #[tokio::test]
    async fn test_build_include_flags_small() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let flags = build_include_flags(&dirs, tmp.path()).await.unwrap();
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
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
    fn test_object_needs_rebuild_when_object_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src.cpp");
        std::fs::write(&source, "int x;").unwrap();
        let object = tmp.path().join("src.o");

        assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
    }

    #[test]
    fn test_object_needs_rebuild_when_source_newer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src.cpp");
        let object = tmp.path().join("src.o");
        std::fs::write(&source, "int x;").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&object, "obj").unwrap();
        std::fs::write(command_hash_path(&object), test_signature()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, "int y;").unwrap();

        assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
    }

    #[test]
    fn test_object_needs_rebuild_when_object_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src.cpp");
        let object = tmp.path().join("src.o");
        std::fs::write(&source, "int x;").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&object, "obj").unwrap();
        std::fs::write(command_hash_path(&object), test_signature()).unwrap();

        assert!(!object_needs_rebuild(&source, &object, test_signature()).unwrap());
    }

    #[test]
    fn test_object_needs_rebuild_when_header_dep_is_newer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src.cpp");
        let header = tmp.path().join("config.h");
        let object = tmp.path().join("src.o");
        let depfile = tmp.path().join("src.d");

        std::fs::write(&source, "#include \"config.h\"\n").unwrap();
        std::fs::write(&header, "#define X 1\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&object, "obj").unwrap();
        std::fs::write(
            &depfile,
            format!(
                "{}: {} {}\n",
                object.display(),
                source.display(),
                header.display()
            ),
        )
        .unwrap();
        std::fs::write(command_hash_path(&object), test_signature()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&header, "#define X 2\n").unwrap();

        assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
    }

    #[test]
    fn test_object_needs_rebuild_when_command_hash_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src.cpp");
        let object = tmp.path().join("src.o");

        std::fs::write(&source, "int x;").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&object, "obj").unwrap();
        std::fs::write(command_hash_path(&object), "old-signature").unwrap();

        assert!(object_needs_rebuild(&source, &object, test_signature()).unwrap());
    }

    #[test]
    fn test_archive_is_up_to_date_when_archive_newer_than_all_objects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let object_a = tmp.path().join("a.o");
        let object_b = tmp.path().join("b.o");
        let archive = tmp.path().join("libx.a");
        std::fs::write(&object_a, "a").unwrap();
        std::fs::write(&object_b, "b").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&archive, "archive").unwrap();

        assert!(archive_is_up_to_date(&archive, &[object_a, object_b]).unwrap());
    }

    #[test]
    fn test_archive_is_not_up_to_date_when_object_newer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let object = tmp.path().join("a.o");
        let archive = tmp.path().join("libx.a");
        std::fs::write(&object, "a").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&archive, "archive").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&object, "newer").unwrap();

        assert!(!archive_is_up_to_date(&archive, &[object]).unwrap());
    }
}
