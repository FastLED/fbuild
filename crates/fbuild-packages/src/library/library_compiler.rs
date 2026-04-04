//! Library compilation and archiving.
//!
//! Compiles C/C++ source files from downloaded libraries using the ESP32
//! toolchain, then archives the object files into static libraries (.a).

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{FbuildError, Result};

/// C++-only flags that must not be passed to gcc for .c files.
const CXX_ONLY_PREFIXES: &[&str] = &["-std=gnu++", "-std=c++", "-fno-rtti", "-fuse-cxa-atexit"];

/// Optionally prepend zccache to a compiler command line.
fn wrap_compiler_args(args: &[&str], cache_path: Option<&Path>) -> Vec<String> {
    match cache_path {
        Some(zcc) => {
            let mut wrapped = Vec::with_capacity(args.len() + 1);
            wrapped.push(zcc.to_string_lossy().to_string());
            wrapped.extend(args.iter().map(|s| s.to_string()));
            wrapped
        }
        None => args.iter().map(|s| s.to_string()).collect(),
    }
}

/// Check if a compiler flag is C++ only.
fn is_cxx_only_flag(flag: &str) -> bool {
    CXX_ONLY_PREFIXES.iter().any(|p| flag.starts_with(p))
}

/// Compile all source files in a library and produce a static archive.
///
/// - C files compiled with gcc + C-safe flags (no C++ flags)
/// - C++ files compiled with g++ + full flags
/// - Objects archived into `lib{name}.a`
///
/// Returns the archive path, or None if the library is header-only.
#[allow(clippy::too_many_arguments)]
pub fn compile_library(
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
    )
}

/// Compile all source files in a library with parallel jobs.
#[allow(clippy::too_many_arguments)]
pub fn compile_library_with_jobs(
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
    let include_flags = build_include_flags(include_dirs, output_dir)?;

    // Pre-compute C-safe flags once
    let c_safe_flags: Vec<String> = c_flags
        .iter()
        .filter(|f| !is_cxx_only_flag(f))
        .cloned()
        .collect();

    let cpp_flags = cpp_flags.to_vec();

    let jobs = jobs.max(1);

    if jobs <= 1 || source_files.len() <= 1 {
        // Sequential path
        let mut objects = Vec::new();
        for source in source_files {
            let obj = compile_one_source(
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
            )?;
            objects.push(obj);
        }

        let archive_path = output_dir.join(format!("lib{}.a", name));
        archive_objects(ar_path, &objects, &archive_path)?;
        tracing::info!(
            "compiled library {}: {} files -> {}",
            name,
            objects.len(),
            archive_path.display()
        );
        return Ok(Some(archive_path));
    }

    // Parallel path
    let total = source_files.len();
    let thread_count = jobs.min(total);

    let work_iter = std::sync::Mutex::new(source_files.iter());
    let first_error: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let compiled_count = std::sync::atomic::AtomicUsize::new(0);
    let objects: std::sync::Mutex<Vec<(usize, PathBuf)>> = std::sync::Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        if first_error.lock().unwrap().is_some() {
                            return;
                        }

                        // Get next work item with its index
                        let item = {
                            let mut iter = work_iter.lock().unwrap();
                            iter.next().cloned()
                        };

                        let source = match item {
                            Some(s) => s,
                            None => return,
                        };

                        match compile_one_source(
                            &source,
                            &obj_dir,
                            gcc_path,
                            gxx_path,
                            &c_safe_flags,
                            &cpp_flags,
                            &include_flags,
                            name,
                            verbose,
                            compiler_cache,
                        ) {
                            Ok(obj) => {
                                let count = compiled_count
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                    + 1;
                                objects.lock().unwrap().push((count, obj));
                                if count % 20 == 0 || count == total {
                                    tracing::info!("[{}/{}] compiled [{}]", count, total, name);
                                }
                            }
                            Err(e) => {
                                let mut err = first_error.lock().unwrap();
                                if err.is_none() {
                                    *err = Some(e.to_string());
                                }
                            }
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    });

    if let Some(error) = first_error.into_inner().unwrap() {
        return Err(FbuildError::BuildFailed(error));
    }

    // Collect objects (order doesn't matter for archiving)
    let mut all_objects: Vec<PathBuf> = objects
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|(_, obj)| obj)
        .collect();
    all_objects.sort(); // deterministic archive

    // Archive
    let archive_path = output_dir.join(format!("lib{}.a", name));
    archive_objects(ar_path, &all_objects, &archive_path)?;

    tracing::info!(
        "compiled library {}: {} files ({} threads) -> {}",
        name,
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
fn compile_one_source(
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
) -> Result<PathBuf> {
    let obj = object_path(source, obj_dir);

    let is_c = source.extension().map(|e| e == "c").unwrap_or(false);

    let (compiler, flags): (&Path, &[String]) = if is_c {
        (gcc_path, c_safe_flags)
    } else {
        (gxx_path, cpp_flags)
    };

    if verbose {
        tracing::info!(
            "compile [{}]: {}",
            lib_name,
            source.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    // Collect all flags that follow the compiler executable
    let mut all_flags: Vec<String> = Vec::new();
    all_flags.extend_from_slice(flags);
    all_flags.extend_from_slice(include_flags);
    all_flags.extend([
        "-c".to_string(),
        source.to_string_lossy().to_string(),
        "-o".to_string(),
        obj.to_string_lossy().to_string(),
    ]);

    // On Windows, put ALL flags in a response file to avoid command-line
    // length limits (OS error 206). The command becomes:
    //   [zccache] <compiler> @response.rsp
    // zccache >=1.1.7 passes @file references through to the compiler
    // without expanding them, so this is safe.
    let args = if cfg!(windows) {
        let rsp_path = fbuild_core::response_file::write_response_file(
            &all_flags,
            &fbuild_core::response_file::windows_temp_dir(),
            "lib_compile",
        )?;
        let mut a = Vec::new();
        if let Some(zcc) = compiler_cache {
            a.push(zcc.to_string_lossy().to_string());
        }
        a.push(compiler.to_string_lossy().to_string());
        a.push(format!("@{}", rsp_path.display()));
        a
    } else {
        let sanitized = fbuild_core::compiler_flags::prepare_flags_for_exec(all_flags);
        let mut raw_args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        raw_args.extend(sanitized);
        let raw_refs: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();
        wrap_compiler_args(&raw_refs, compiler_cache)
    };

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = run_command(&args_ref, None, None, None)?;

    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "failed to compile {} in library {}:\n{}",
            source.display(),
            lib_name,
            result.stderr
        )));
    }

    Ok(obj)
}

/// Build include flags, using a response file on Windows if needed.
///
/// When there are many include paths (>100), writes a response file.
/// Uses `-iprefix` + `-iwithprefixbefore` for paths sharing a common prefix
/// to keep the total command line under GCC 8.4.0's 32KB CreateProcess limit.
fn build_include_flags(include_dirs: &[PathBuf], _temp_dir: &Path) -> Result<Vec<String>> {
    let flags: Vec<String> = include_dirs
        .iter()
        .map(|d| format!("-I{}", d.display()))
        .collect();

    if cfg!(windows) && flags.len() > 100 {
        let rsp_path = fbuild_core::response_file::write_response_file(
            &flags,
            &fbuild_core::response_file::windows_temp_dir(),
            "lib_includes",
        )?;
        Ok(vec![format!("@{}", rsp_path.display())])
    } else {
        Ok(flags)
    }
}

/// Create a static archive from object files.
fn archive_objects(ar_path: &Path, objects: &[PathBuf], output: &Path) -> Result<()> {
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
    // Use a hash of the full source path to avoid collisions
    let hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(source.to_string_lossy().as_bytes());
        let result = hasher.finalize();
        format!("{:02x}{:02x}", result[0], result[1])
    };
    obj_dir.join(format!("{}_{}.o", stem, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_build_include_flags_small() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let flags = build_include_flags(&dirs, tmp.path()).unwrap();
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
    }
}
