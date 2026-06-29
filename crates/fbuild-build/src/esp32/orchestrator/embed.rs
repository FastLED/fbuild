//! Convert `board_build.embed_files` / `embed_txtfiles` into linkable ELF objects.

use std::path::Path;

use fbuild_core::Result;

/// Process `board_build.embed_files` and `board_build.embed_txtfiles`.
///
/// Converts data files into linkable ELF objects using `objcopy --input-target binary`.
/// This generates `_binary_<name>_start`, `_binary_<name>_end`, and `_binary_<name>_size`
/// symbols that the firmware can reference.
///
/// - `embed_files`: embedded as-is (binary)
/// - `embed_txtfiles`: a null-terminated copy is created first, then embedded
#[allow(clippy::too_many_arguments)]
pub(super) async fn process_embed_files(
    embed_files: &[String],
    embed_txtfiles: &[String],
    project_dir: &Path,
    embed_dir: &Path,
    objcopy_path: &Path,
    output_target: &str,
    binary_arch: &str,
    verbose: bool,
) -> Result<Vec<std::path::PathBuf>> {
    use fbuild_core::subprocess::run_command;

    let mut objects = Vec::new();

    // Helper: convert a relative file path to the object file name.
    // e.g. "config/timezones.json" → "config_timezones_json.o"
    let to_obj_name = |path: &str| -> String {
        let sanitized = path.replace(['/', '\\', '.', '-'], "_");
        format!("{}.o", sanitized)
    };

    // Process binary embed files (embed as-is, cwd=project_dir)
    for file in embed_files {
        let src_path = project_dir.join(file);
        if !src_path.exists() {
            tracing::warn!("embed_files: {} not found, skipping", src_path.display());
            continue;
        }

        let obj_name = to_obj_name(file);
        let obj_path = embed_dir.join(&obj_name);

        if obj_path.exists() {
            objects.push(obj_path);
            continue;
        }

        let args = [
            objcopy_path.to_string_lossy().to_string(),
            "--input-target".to_string(),
            "binary".to_string(),
            "--output-target".to_string(),
            output_target.to_string(),
            "--binary-architecture".to_string(),
            binary_arch.to_string(),
            "--rename-section".to_string(),
            ".data=.rodata.embedded".to_string(),
            file.replace('\\', "/"),
            obj_path.to_string_lossy().to_string(),
        ];

        if verbose {
            tracing::info!("embed: {}", args.join(" "));
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, Some(project_dir), None, None).await?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "objcopy failed for embed file {}:\n{}",
                file, result.stderr
            )));
        }

        tracing::info!("embedded binary file: {}", file);
        objects.push(obj_path);
    }

    // Process text embed files (null-terminated copy, then objcopy from embed_dir)
    for file in embed_txtfiles {
        let src_path = project_dir.join(file);
        if !src_path.exists() {
            tracing::warn!("embed_txtfiles: {} not found, skipping", src_path.display());
            continue;
        }

        let obj_name = to_obj_name(file);
        let obj_path = embed_dir.join(&obj_name);

        if obj_path.exists() {
            objects.push(obj_path);
            continue;
        }

        // Create null-terminated copy in embed_dir preserving relative path
        let rel_dest = embed_dir.join(file);
        if let Some(parent) = rel_dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = std::fs::read(&src_path)?;
        if content.last() != Some(&0) {
            content.push(0);
        }
        std::fs::write(&rel_dest, &content)?;

        let args = [
            objcopy_path.to_string_lossy().to_string(),
            "--input-target".to_string(),
            "binary".to_string(),
            "--output-target".to_string(),
            output_target.to_string(),
            "--binary-architecture".to_string(),
            binary_arch.to_string(),
            "--rename-section".to_string(),
            ".data=.rodata.embedded".to_string(),
            file.replace('\\', "/"),
            obj_path.to_string_lossy().to_string(),
        ];

        if verbose {
            tracing::info!("embed txt: {}", args.join(" "));
        }

        // Run from embed_dir so objcopy generates symbols from the relative path
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, Some(embed_dir), None, None).await?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "objcopy failed for embed txtfile {}:\n{}",
                file, result.stderr
            )));
        }

        tracing::info!("embedded text file: {}", file);
        objects.push(obj_path);
    }

    if !objects.is_empty() {
        tracing::info!("processed {} embedded files", objects.len());
    }

    Ok(objects)
}
