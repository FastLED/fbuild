//! Clang-family analysis subcommands: `clang-tidy`, `clang-query`, and
//! `iwyu` (include-what-you-use), plus a couple of cache + filter helpers
//! used by them.

use crate::output;

use super::build::{normalize_path, run_build};

/// Run IWYU (include-what-you-use) analysis with ESP32 cross-compilation support.
///
/// Unlike the generic `run_clang_tool()`, this:
/// 1. Downloads IWYU via ClangComponent
/// 2. Generates compile_commands.json via the fbuild daemon
/// 3. Preprocesses the database: adds GCC builtin includes, converts framework
///    `-I` to `-isystem`, removes `--target=` flags, deduplicates `-D` defines
/// 4. Writes a modified compile_commands.json to a temp dir
/// 5. Runs IWYU per-source-file with `-p <temp_dir>`
/// 6. Filters output to only show suggestions for files under `src/`
pub async fn run_iwyu(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir).await?;

    // Step 1: Ensure IWYU is installed
    let component = fbuild_packages::toolchain::ClangComponent::new(
        fbuild_packages::toolchain::ClangComponentKind::Iwyu,
    );
    let tool_path = component.get_binary("include-what-you-use").await?;
    output::progress(format!(
        "Using include-what-you-use: {}",
        tool_path.display()
    ));

    // Step 2: Generate compile_commands.json via fbuild daemon (skip if it already exists)
    let project_path = std::path::Path::new(&project_dir);
    let db_path = project_path.join("compile_commands.json");
    if db_path.exists() {
        output::progress("Using existing compile_commands.json");
    } else {
        output::progress("Generating compile_commands.json...");
        run_build(
            project_dir.clone(),
            environment.clone(),
            false,
            verbose,
            None,
            false,
            false,
            false,
            Some("compiledb".to_string()),
            None,
            true, // no_timestamp: compiledb generation doesn't need timestamps
            None,
            false, // bloat_analysis
        )
        .await?;
        if !db_path.exists() {
            return Err(fbuild_core::FbuildError::Other(
                "compile_commands.json was not generated".into(),
            ));
        }
    }
    let db_content = std::fs::read_to_string(&db_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to read compile_commands.json: {}", e))
    })?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&db_content).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse compile_commands.json: {}", e))
    })?;

    // Filter to project source files only.
    // Source files can be under <project>/src/ (original) or
    // <project>/.fbuild/build/<env>/*/src/ (build copies with Arduino preprocessing).
    // Exclude framework/SDK files (paths containing cache/platforms/ or cache/toolchains/).
    let src_dir = project_path.join("src");
    let build_src_suffix = format!("{}src", std::path::MAIN_SEPARATOR_STR);
    let project_prefix = project_path
        .to_string_lossy()
        .replace('/', std::path::MAIN_SEPARATOR_STR);
    let source_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| {
            e.get("file")
                .and_then(|f| f.as_str())
                .map(|f| {
                    let p = std::path::Path::new(f);
                    // Direct match: file is under <project>/src/
                    if p.starts_with(&src_dir) {
                        return true;
                    }
                    // Build copy: file is under <project>/.fbuild/build/.../src/
                    let f_normalized = f.replace('/', std::path::MAIN_SEPARATOR_STR);
                    f_normalized.starts_with(&project_prefix)
                        && f_normalized.contains(&build_src_suffix)
                        && !f_normalized.contains("cache")
                })
                .unwrap_or(false)
        })
        .collect();

    if source_entries.is_empty() {
        output::result("No source files found in compile_commands.json under src/");
        return Ok(());
    }

    // Step 4: Find GCC toolchain builtin include dirs
    let gcc_includes = fbuild_packages::toolchain::clang::find_gcc_builtin_include_dirs();
    if !gcc_includes.is_empty() {
        output::progress(format!(
            "Found {} GCC builtin include dir(s)",
            gcc_includes.len()
        ));
        if verbose {
            for inc in &gcc_includes {
                output::debug(format!("  {}", inc.display()));
            }
        }
    }

    // Step 5: Preprocess compile_commands.json for IWYU
    // Transform entries directly as JSON: remove --target=, dedup -D, convert -I to -isystem
    // FastLED/fbuild#911 — path-shape slash normalization goes through
    // `NormalizedPath::display_slash()`.
    let src_prefix = fbuild_core::path::NormalizedPath::from(src_dir.as_path())
        .display_slash()
        .to_lowercase();
    let iwyu_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut new_entry = entry.clone();
            if let Some(args) = entry.get("arguments").and_then(|a| a.as_array()) {
                let mut new_args: Vec<serde_json::Value> =
                    Vec::with_capacity(args.len() + gcc_includes.len() * 2);
                let mut seen_defines = std::collections::HashSet::new();

                for arg_val in args {
                    let arg = arg_val.as_str().unwrap_or("");

                    // Remove --target= flags
                    if arg.starts_with("--target=") {
                        continue;
                    }

                    // Remove GCC-only flags unsupported by IWYU's clang
                    if matches!(
                        arg,
                        "-freorder-blocks"
                            | "-fno-jump-tables"
                            | "-flto"
                            | "-flto=auto"
                            | "-fno-fat-lto-objects"
                            | "-fuse-linker-plugin"
                            | "-ffat-lto-objects"
                            | "-mlongcalls"
                            | "-mdisable-hardware-atomics"
                            | "-fstrict-volatile-bitfields"
                            | "-mtext-section-literals"
                            | "-fno-tree-switch-conversion"
                            | "-mthumb-interwork"
                    ) || arg.starts_with("-mfix-esp32-psram-cache-strategy=")
                    {
                        continue;
                    }

                    // Deduplicate -D flags (keep first occurrence by key)
                    if arg.starts_with("-D") {
                        let key = if let Some(eq_pos) = arg.find('=') {
                            &arg[..eq_pos]
                        } else {
                            arg
                        };
                        if !seen_defines.insert(key.to_string()) {
                            continue;
                        }
                    }

                    // Convert non-project -I to -isystem
                    if let Some(path) = arg.strip_prefix("-I") {
                        // FastLED/fbuild#911 — path-shape slash normalization
                        // goes through `NormalizedPath::display_slash()`.
                        let normalized = fbuild_core::path::NormalizedPath::from(path)
                            .display_slash()
                            .to_lowercase();
                        if normalized.starts_with(&src_prefix) {
                            new_args.push(arg_val.clone());
                        } else {
                            new_args.push(serde_json::Value::String("-isystem".into()));
                            new_args.push(serde_json::Value::String(path.to_string()));
                        }
                        continue;
                    }

                    new_args.push(arg_val.clone());
                }

                // Append GCC toolchain builtin include dirs as -isystem
                for inc in &gcc_includes {
                    new_args.push(serde_json::Value::String("-isystem".into()));
                    new_args.push(serde_json::Value::String(inc.to_string_lossy().to_string()));
                }

                new_entry["arguments"] = serde_json::Value::Array(new_args);
            }
            new_entry
        })
        .collect();

    // Write modified compile_commands.json to .fbuild/iwyu/ for IWYU to read via -p
    let iwyu_dir_path = fbuild_paths::get_project_fbuild_dir(project_path).join("iwyu");
    std::fs::create_dir_all(&iwyu_dir_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!(
            "failed to create {}: {}",
            iwyu_dir_path.display(),
            e
        ))
    })?;
    let iwyu_db_path = iwyu_dir_path.join("compile_commands.json");
    let iwyu_json = serde_json::to_string_pretty(&iwyu_entries).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to serialize IWYU compile database: {}", e))
    })?;
    // Atomic write — FastLED/fbuild#844 bridge pair 6 (IWYU compile DB).
    fbuild_core::fs::write_atomic(&iwyu_db_path, iwyu_json)
        .await
        .map_err(|e| {
            fbuild_core::FbuildError::Other(format!(
                "failed to write {}: {}",
                iwyu_db_path.display(),
                e
            ))
        })?;
    // Step 6: Set up zccache-style content-addressed cache for IWYU results.
    // Cache key = blake3(source_content + iwyu_entry_json) per file.
    let cache_dir = iwyu_dir_path.join("cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to create {}: {}", cache_dir.display(), e))
    })?;

    // Build a lookup from file path → preprocessed IWYU entry JSON for cache keying.
    let iwyu_entry_map: std::collections::HashMap<String, String> = iwyu_entries
        .iter()
        .filter_map(|e| {
            let file = e.get("file")?.as_str()?.to_string();
            let json = serde_json::to_string(e).ok()?;
            Some((file, json))
        })
        .collect();

    output::progress(format!(
        "Running include-what-you-use on {} source file(s)...",
        source_entries.len()
    ));

    // Step 7: Run IWYU in parallel with caching
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(4);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let tool_arc = std::sync::Arc::new(tool_path);
    let iwyu_dir = std::sync::Arc::new(iwyu_dir_path);
    let src_dir_arc = std::sync::Arc::new(src_dir.clone());
    let cache_dir_arc = std::sync::Arc::new(cache_dir);
    let entry_map_arc = std::sync::Arc::new(iwyu_entry_map);

    // Collect source file paths from the filtered entries
    let source_files: Vec<String> = source_entries
        .iter()
        .filter_map(|e| e.get("file").and_then(|f| f.as_str()).map(String::from))
        .collect();

    let mut handles = Vec::new();
    for file in source_files {
        let sem = semaphore.clone();
        let tool = tool_arc.clone();
        let p_dir = iwyu_dir.clone();
        let src = src_dir_arc.clone();
        let cache_d = cache_dir_arc.clone();
        let emap = entry_map_arc.clone();
        let verbose_flag = verbose;
        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .expect("fbuild-cli: clang-tool semaphore is never closed before all tasks finish");
            let src_path = src.as_ref().clone();

            // Compute blake3 cache key from source content + compile entry
            let cache_key = iwyu_cache_key(&file, &emap);
            let cache_path: Option<std::path::PathBuf> = cache_key
                .as_ref()
                .map(|k| cache_d.join(format!("{}.txt", k)));

            // Check cache
            if let Some(ref cp) = cache_path {
                if cp.exists() {
                    if let Ok(cached) = std::fs::read_to_string(cp) {
                        return (file, Ok(cached), true, src_path);
                    }
                }
            }

            // Cache miss — run IWYU. Parallel async fan-out inside the CLI binary
            // (no daemon containment group in this process).
            // allow-direct-spawn: parallel async fan-out in CLI; no containment group here.
            let mut cmd = tokio::process::Command::new(tool.as_ref());
            cmd.arg("-p").arg(p_dir.as_ref());
            cmd.arg("-Xiwyu").arg("--no_comments");
            cmd.arg("-Xiwyu").arg("--quoted_includes_first");
            cmd.arg("-Xiwyu").arg("--max_line_length=100");
            if verbose_flag {
                cmd.arg("-Xiwyu").arg("--verbose=3");
            }
            cmd.arg(&file);
            // FastLED/fbuild#810: cap each IWYU invocation at 120s so a wedged
            // subprocess can't hang the whole fan-out forever.
            let output =
                match tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output()).await
                {
                    Ok(res) => res,
                    Err(_) => {
                        return (
                            file,
                            Err("include-what-you-use timed out after 120s".to_string()),
                            false,
                            src_path,
                        );
                    }
                };

            match output {
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    // Store in cache on success
                    if let Some(ref cp) = cache_path {
                        let _ = std::fs::write(cp, &stderr);
                    }
                    (file, Ok(stderr), false, src_path)
                }
                Err(e) => (file, Err(format!("{}", e)), false, src_path),
            }
        });
        handles.push(handle);
    }

    let mut total_suggestions = 0usize;
    let mut failed_files = Vec::new();
    let mut cache_hits = 0usize;
    let mut cache_misses = 0usize;

    for handle in handles {
        let (file, result, cached, src_path) = handle
            .await
            .map_err(|e| fbuild_core::FbuildError::Other(format!("task join error: {}", e)))?;
        if cached {
            cache_hits += 1;
        } else {
            cache_misses += 1;
        }
        match result {
            Ok(stderr) => {
                let filtered = filter_iwyu_output(&stderr, &src_path);
                if !filtered.trim().is_empty() {
                    // filter_iwyu_output already terminates each line with '\n'; strip
                    // the final trailing newline so result()'s own newline doesn't
                    // double up.
                    output::result(filtered.trim_end_matches('\n'));
                    total_suggestions += filtered
                        .lines()
                        .filter(|l| l.contains("should add") || l.contains("should remove"))
                        .count();
                }
            }
            Err(e) => {
                output::error(format!(
                    "failed to run include-what-you-use on {}: {}",
                    file, e
                ));
                failed_files.push(file);
            }
        }
    }

    output::result("\n--- include-what-you-use summary ---");
    output::result(format!("Suggestions: {}", total_suggestions));
    output::result(format!(
        "Cache:       {} hit(s), {} miss(es)",
        cache_hits, cache_misses
    ));
    if !failed_files.is_empty() {
        output::result(format!("Failed:      {} file(s)", failed_files.len()));
    }

    if !failed_files.is_empty() {
        Err(fbuild_core::FbuildError::BuildFailed(
            "include-what-you-use failed on some files".into(),
        ))
    } else {
        Ok(())
    }
}

/// Compute a blake3 cache key for an IWYU analysis of a source file.
///
/// The key is derived from the source file content and the preprocessed
/// compile_commands.json entry for that file (which includes all flags).
/// This mirrors zccache's content-addressed hashing strategy.
pub fn iwyu_cache_key(
    file: &str,
    entry_map: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let source_content = std::fs::read(file).ok()?;
    let entry_json = entry_map.get(file)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fbuild-iwyu-cache-v1");
    hasher.update(&source_content);
    hasher.update(entry_json.as_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// Filter IWYU output to show only suggestions for files under `src_dir`.
///
/// IWYU outputs blocks like:
/// ```text
/// /path/to/file.h should add these lines:
/// #include <foo.h>
///
/// /path/to/file.h should remove these lines:
/// - #include <bar.h>
///
/// The full include-list for /path/to/file.h:
/// #include <baz.h>
/// ---
/// ```
///
/// We only keep blocks whose file path is under `src_dir`.
pub fn filter_iwyu_output(output: &str, src_dir: &std::path::Path) -> String {
    // FastLED/fbuild#911 — path-shape slash normalization goes through
    // `NormalizedPath::display_slash()`.
    let src_prefix = fbuild_core::path::NormalizedPath::from(src_dir)
        .display_slash()
        .to_lowercase();
    let mut result = String::new();
    let mut current_block = String::new();
    let mut block_is_user_file = false;

    for line in output.lines() {
        // Detect block headers: "path/to/file should add/remove these lines:"
        // or "The full include-list for path/to/file:"
        let is_header = line.contains(" should add these lines")
            || line.contains(" should remove these lines")
            || line.starts_with("The full include-list for ");

        if is_header {
            // Flush previous block if it was a user file
            if block_is_user_file && !current_block.trim().is_empty() {
                result.push_str(&current_block);
                result.push('\n');
            }
            current_block.clear();

            // Check if this new block's file is under src_dir
            let file_path = line
                .split(" should ")
                .next()
                .or_else(|| {
                    line.strip_prefix("The full include-list for ")
                        .and_then(|s| s.strip_suffix(':'))
                })
                .unwrap_or("");
            // FastLED/fbuild#911 — path-shape slash normalization goes
            // through `NormalizedPath::display_slash()`.
            let normalized = fbuild_core::path::NormalizedPath::from(file_path)
                .display_slash()
                .to_lowercase();
            block_is_user_file = normalized.starts_with(&src_prefix);
        }

        current_block.push_str(line);
        current_block.push('\n');
    }

    // Flush last block
    if block_is_user_file && !current_block.trim().is_empty() {
        result.push_str(&current_block);
    }

    result
}

/// Generic runner for clang-based analysis tools (clang-tidy, clang-query).
///
/// 1. Ensure tool binary is installed via ClangComponent (downloads on demand)
/// 2. Generate compile_commands.json via fbuild daemon (build -t compiledb)
/// 3. Run tool on each source file in parallel (ncpus * 2)
#[allow(clippy::too_many_arguments)]
pub async fn run_clang_tool(
    kind: fbuild_packages::toolchain::ClangComponentKind,
    binary_name: &str,
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    extra_args: &[&str],
) -> fbuild_core::Result<()> {
    let project_dir = normalize_path(&project_dir).await?;

    // Step 1: Ensure tool is installed
    let component = fbuild_packages::toolchain::ClangComponent::new(kind);
    let tool_path = component.get_binary(binary_name).await?;
    output::progress(format!("Using {}: {}", binary_name, tool_path.display()));

    // Step 2: Generate compile_commands.json via fbuild daemon
    output::progress("Generating compile_commands.json...");
    run_build(
        project_dir.clone(),
        environment.clone(),
        false, // clean
        verbose,
        None,  // jobs
        false, // quick
        false, // release
        false, // dry_run
        Some("compiledb".to_string()),
        None,
        true, // no_timestamp: compiledb generation doesn't need timestamps
        None,
        false, // bloat_analysis
    )
    .await?;

    // Step 3: Read compile_commands.json to get source files
    let project_path = std::path::Path::new(&project_dir);
    let db_path = project_path.join("compile_commands.json");
    if !db_path.exists() {
        return Err(fbuild_core::FbuildError::Other(
            "compile_commands.json was not generated".into(),
        ));
    }
    let db_content = std::fs::read_to_string(&db_path).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to read compile_commands.json: {}", e))
    })?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&db_content).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("failed to parse compile_commands.json: {}", e))
    })?;

    // Filter to project source files only (under src/)
    let src_dir = project_path.join("src");
    let source_files: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("file").and_then(|f| f.as_str()).map(String::from))
        .filter(|f| {
            let p = std::path::Path::new(f);
            p.starts_with(&src_dir)
        })
        .collect();

    if source_files.is_empty() {
        output::result("No source files found in compile_commands.json under src/");
        return Ok(());
    }
    output::progress(format!(
        "Running {} on {} source file(s)...",
        binary_name,
        source_files.len()
    ));

    // Step 4: Run tool in parallel with ncpus * 2
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(4);
    output::progress(format!("Using {} parallel jobs", jobs));

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let project_dir_arc = std::sync::Arc::new(project_dir.clone());
    let tool_arc = std::sync::Arc::new(tool_path);
    let extra_owned: Vec<String> = extra_args.iter().map(|s| s.to_string()).collect();
    let verbose_flag = verbose;

    let mut handles = Vec::new();
    for file in source_files {
        let sem = semaphore.clone();
        let tool = tool_arc.clone();
        let pd = project_dir_arc.clone();
        let extra = extra_owned.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .expect("fbuild-cli: clang-tool semaphore is never closed before all tasks finish");
            // allow-direct-spawn: parallel async fan-out (clang-tidy) in CLI binary.
            let mut cmd = tokio::process::Command::new(tool.as_ref());
            cmd.arg("-p").arg(pd.as_ref());
            for arg in &extra {
                cmd.arg(arg);
            }
            cmd.arg(&file);
            if !verbose_flag {
                cmd.arg("--quiet");
            }
            // FastLED/fbuild#810: cap each clang-tidy invocation at 120s so a
            // wedged subprocess can't hang the whole fan-out forever.
            let output =
                match tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output()).await
                {
                    Ok(res) => res,
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "clang-tidy timed out after 120s",
                    )),
                };
            (file, output)
        });
        handles.push(handle);
    }

    let mut total_warnings = 0usize;
    let mut total_errors = 0usize;
    let mut failed_files = Vec::new();

    for handle in handles {
        let (file, result) = handle
            .await
            .map_err(|e| fbuild_core::FbuildError::Other(format!("task join error: {}", e)))?;
        match result {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let combined = format!("{}{}", stdout, stderr);
                if !combined.trim().is_empty() {
                    // clang-tidy already terminates each line with '\n'; strip the
                    // final trailing newline so result()'s newline doesn't double up.
                    output::result(combined.trim_end_matches('\n'));
                }
                for line in combined.lines() {
                    if line.contains("warning:") {
                        total_warnings += 1;
                    }
                    if line.contains("error:") {
                        total_errors += 1;
                    }
                }
            }
            Err(e) => {
                output::error(format!("failed to run {} on {}: {}", binary_name, file, e));
                failed_files.push(file);
            }
        }
    }

    output::result(format!("\n--- {} summary ---", binary_name));
    output::result(format!("Warnings: {}", total_warnings));
    output::result(format!("Errors:   {}", total_errors));
    if !failed_files.is_empty() {
        output::result(format!("Failed:   {} file(s)", failed_files.len()));
    }

    if total_errors > 0 || !failed_files.is_empty() {
        Err(fbuild_core::FbuildError::BuildFailed(format!(
            "{} found errors",
            binary_name
        )))
    } else {
        Ok(())
    }
}
