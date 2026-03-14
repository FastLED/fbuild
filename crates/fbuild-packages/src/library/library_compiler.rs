//! Library compilation and archiving.
//!
//! Compiles C/C++ source files from downloaded libraries using the ESP32
//! toolchain, then archives the object files into static libraries (.a).

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::{FbuildError, Result};

/// C++-only flags that must not be passed to gcc for .c files.
const CXX_ONLY_PREFIXES: &[&str] = &["-std=gnu++", "-std=c++", "-fno-rtti", "-fuse-cxa-atexit"];

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
) -> Result<Option<PathBuf>> {
    if source_files.is_empty() {
        tracing::debug!("library {} is header-only, skipping compile", name);
        return Ok(None);
    }

    let obj_dir = output_dir.join("obj");
    std::fs::create_dir_all(&obj_dir)?;

    // Build include flags
    let include_flags = build_include_flags(include_dirs)?;

    let mut objects = Vec::new();

    for source in source_files {
        let obj = object_path(source, &obj_dir);
        if let Some(parent) = obj.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let is_c = source.extension().map(|e| e == "c").unwrap_or(false);

        let (compiler, flags) = if is_c {
            // Filter out C++-only flags for C files
            let c_safe: Vec<String> = c_flags
                .iter()
                .filter(|f| !is_cxx_only_flag(f))
                .cloned()
                .collect();
            (gcc_path, c_safe)
        } else {
            (gxx_path, cpp_flags.to_vec())
        };

        let mut args: Vec<String> = vec![compiler.to_string_lossy().to_string()];
        args.extend(flags);
        args.extend(include_flags.clone());
        args.extend([
            "-c".to_string(),
            source.to_string_lossy().to_string(),
            "-o".to_string(),
            obj.to_string_lossy().to_string(),
        ]);

        if verbose {
            tracing::info!(
                "compile [{}]: {}",
                name,
                source.file_name().unwrap_or_default().to_string_lossy()
            );
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, None, None, None)?;

        if !result.success() {
            return Err(FbuildError::BuildFailed(format!(
                "failed to compile {} in library {}:\n{}",
                source.display(),
                name,
                result.stderr
            )));
        }

        objects.push(obj);
    }

    // Archive
    let archive_path = output_dir.join(format!("lib{}.a", name));
    archive_objects(ar_path, &objects, &archive_path)?;

    tracing::info!(
        "compiled library {}: {} files -> {}",
        name,
        objects.len(),
        archive_path.display()
    );

    Ok(Some(archive_path))
}

/// Build include flags, using a response file on Windows if needed.
fn build_include_flags(include_dirs: &[PathBuf]) -> Result<Vec<String>> {
    let flags: Vec<String> = include_dirs
        .iter()
        .map(|d| format!("-I{}", d.display()))
        .collect();

    if cfg!(windows) && flags.len() > 100 {
        let temp_dir = if cfg!(windows) {
            std::env::var("LOCALAPPDATA")
                .map(|la| std::path::PathBuf::from(la).join("Temp"))
                .unwrap_or_else(|_| std::env::temp_dir())
        } else {
            std::env::temp_dir()
        };
        let rsp_path = temp_dir.join(format!("fbuild_lib_includes_{}.rsp", std::process::id()));
        let content = flags.join("\n");
        std::fs::write(&rsp_path, content).map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to write response file {}: {}",
                rsp_path.display(),
                e
            ))
        })?;
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
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let flags = build_include_flags(&dirs).unwrap();
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
    }
}
