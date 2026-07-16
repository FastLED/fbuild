//! `fbuild lib-select` diagnostic subcommand.
//!
//! Drives the same library-selection resolver the daemon uses (the
//! `fbuild-library-select` crate) directly against an already-installed
//! framework cache and the project's own `src/`/`include/`/`lib/` trees, then
//! prints the result in three flavors:
//!
//! - default: one library name per line, sorted, suitable for piping to grep;
//! - `--explain`: human-readable summary with per-library attribution and the
//!   list of unresolved `#include`s — handy for debugging
//!   `FastLED/fbuild#202` / `#204`-style "library not found" issues;
//! - `--json`: machine-readable; mutually exclusive with `--explain`.
//!
//! The CLI does **not** download anything: if the framework cache root is
//! missing, it prints a clear "framework not installed; run `fbuild build`
//! once first" message and exits 2. Downloading is the daemon's job.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_config::PlatformIOConfig;
use fbuild_core::Platform;
use fbuild_core::path::normalize_for_key;
use fbuild_library_select::{Selection, resolve_active};
use fbuild_packages::Framework;
use fbuild_packages::library::FrameworkLibrary;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use serde_json::json;
use walkdir::{DirEntry, WalkDir};

/// Top-level entry point. Returns a process exit code.
pub fn run(project_dir: &Path, env: Option<&str>, explain: bool, json: bool) -> i32 {
    // FastLED/fbuild#844 sync-context allowlist: `fbuild lib-select`
    // is a synchronous diagnostic CLI entry point dispatched directly
    // by clap, with no tokio runtime in scope. File is allowlisted in
    // `dylints/ban_std_fs_canonicalize/src/allowlist.txt`.
    let project_dir = match std::fs::canonicalize(project_dir) {
        Ok(p) => p,
        Err(err) => {
            eprintln!(
                "lib-select: project '{}' not accessible: {}",
                project_dir.display(),
                err
            );
            return 2;
        }
    };

    let ini_path = project_dir.join("platformio.ini");
    let config = match PlatformIOConfig::from_path(&ini_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("lib-select: failed to read {}: {}", ini_path.display(), err);
            return 2;
        }
    };

    let env_name: String = match env {
        Some(name) => name.to_string(),
        None => match config.get_default_environment() {
            Some(name) => name.to_string(),
            None => {
                eprintln!("lib-select: no environment given and no default in platformio.ini");
                return 2;
            }
        },
    };

    let env_cfg = match config.get_env_config(&env_name) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("lib-select: {}", err);
            return 2;
        }
    };

    let platform_str = env_cfg.get("platform").cloned().unwrap_or_default();
    let board = env_cfg
        .get("board")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    // We deliberately don't read `framework`: every supported `Platform`
    // already implies its (single) framework, and PlatformIO's INI uses
    // `framework = arduino` near-uniformly for the boards we handle.

    let platform = match Platform::from_platform_str(&platform_str) {
        Some(p) => p,
        None => {
            eprintln!(
                "lib-select: unrecognized platform '{}' for env '{}'",
                platform_str, env_name
            );
            return 2;
        }
    };

    let framework_info = match resolve_framework(&project_dir, platform, &board) {
        Ok(info) => info,
        Err(msg) => {
            eprintln!("lib-select: {}", msg);
            return 2;
        }
    };

    let libraries = discover_framework_libraries(&framework_info.libraries_dir);

    let scan_roots = project_scan_roots(&project_dir);
    let seeds = collect_project_seeds(&scan_roots);
    let search_paths = project_search_paths(&scan_roots);

    // The diagnostic must match build-time LDF behavior: optional headers in
    // inactive branches are not library dependencies. Sketch-local defines
    // are collected by `resolve_active`; board/compiler defines are supplied
    // by platform orchestrators during an actual build.
    let selection = resolve_active(&seeds, &search_paths, &libraries, &HashMap::new());

    if json {
        emit_json(
            &project_dir,
            &env_name,
            &framework_info,
            &libraries,
            &seeds,
            &selection,
        );
    } else if explain {
        emit_explain(
            &project_dir,
            &env_name,
            &framework_info,
            &libraries,
            &seeds,
            &selection,
        );
    } else {
        for name in &selection.required_libraries {
            println!("{}", name);
        }
    }

    0
}

/// Resolved framework cache info needed to discover libraries.
struct FrameworkInfo {
    name: String,
    root: PathBuf,
    libraries_dir: PathBuf,
}

/// Map (platform, board) → an installed framework on disk.
///
/// Each platform's framework lives at a different cache path. We instantiate
/// the relevant `fbuild_packages::library::*` type and ask it for
/// `get_libraries_dir()`. We do **not** download anything: if the resolved
/// path doesn't exist we return an Err and the caller prints a clear
/// "framework not installed" message.
fn resolve_framework(
    project_dir: &Path,
    platform: Platform,
    board: &str,
) -> Result<FrameworkInfo, String> {
    use fbuild_packages::library::{
        Apollo3Cores, AvrFramework, Ch32vCores, Esp32Framework, Esp8266Framework, Nrf52Cores,
        RenesasCores, Rp2040Cores, SamCores, SilabsCores, Stm32Cores, TeensyCores,
    };

    /// Build a `FrameworkInfo` from a libraries dir + display name. Pulled out
    /// so each match arm can stay a one-liner.
    fn info(name: &str, libs: PathBuf) -> (String, PathBuf, PathBuf) {
        let root = libs
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| libs.clone());
        (name.to_string(), libs, root)
    }

    let (name, libraries_dir, root): (String, PathBuf, PathBuf) = match platform {
        Platform::Teensy => info(
            "framework-arduinoteensy",
            TeensyCores::new(project_dir).get_libraries_dir(),
        ),
        Platform::Ststm32 => info(
            "Arduino_Core_STM32",
            Stm32Cores::new(project_dir).get_libraries_dir(),
        ),
        Platform::AtmelAvr | Platform::AtmelMegaAvr => {
            // AVR is data-driven by board core; "arduino" covers the
            // overwhelming majority of boards (uno, mega, nano, leonardo, ...).
            // Boards needing alt cores (MiniCore, ATTinyCore, ...) won't
            // resolve — caller can re-run after `fbuild build` populates them.
            let fw = AvrFramework::for_core("arduino", project_dir)
                .map_err(|e| format!("AVR framework lookup failed for core 'arduino': {}", e))?;
            info("framework-arduino-avr", fw.get_libraries_dir())
        }
        Platform::Espressif32 => {
            // `Esp32Framework::new` accepts an MCU but only consults it for
            // post-discovery operations; the libraries dir is the same for
            // every ESP32 variant.
            info(
                "framework-arduinoespressif32",
                Esp32Framework::new(project_dir, board).get_libraries_dir(),
            )
        }
        Platform::Espressif8266 => info(
            "framework-arduinoespressif8266",
            Esp8266Framework::new(project_dir).get_libraries_dir(),
        ),
        Platform::RaspberryPi => info(
            "framework-arduinopico",
            Rp2040Cores::new(project_dir).get_libraries_dir(),
        ),
        Platform::NordicNrf52 => info(
            "framework-arduinoadafruitnrf52",
            Nrf52Cores::new(project_dir).get_libraries_dir(),
        ),
        Platform::AtmelSam => info(
            "framework-arduinosam",
            SamCores::new(project_dir).get_libraries_dir(),
        ),
        Platform::RenesasRa => info(
            "framework-arduinorenesas",
            RenesasCores::new(project_dir).get_libraries_dir(),
        ),
        Platform::SiliconLabs => info(
            "framework-arduinosilabs",
            SilabsCores::new(project_dir).get_libraries_dir(),
        ),
        Platform::Ch32v => info(
            "framework-arduinoch32v",
            Ch32vCores::new(project_dir).get_libraries_dir(),
        ),
        Platform::Apollo3 => info(
            "framework-arduinoapollo3",
            Apollo3Cores::new(project_dir).get_libraries_dir(),
        ),
        Platform::Wasm => {
            return Err(
                "WASM (emscripten) does not use a libraries/ tree; lib-select is not applicable"
                    .to_string(),
            );
        }
        Platform::NxpLpc => {
            // NXP LPC8xx is bare-metal CMSIS — no Arduino-style libraries/ tree.
            // lib-select has nothing to discover; FastLED is consumed as source.
            return Err(
                "NXP LPC8xx (bare-metal CMSIS) does not use a libraries/ tree; lib-select is not applicable"
                    .to_string(),
            );
        }
    };

    if !libraries_dir.is_dir() {
        return Err(format!(
            "framework not installed at {} (run `fbuild build` once first to populate the cache)",
            libraries_dir.display()
        ));
    }

    Ok(FrameworkInfo {
        name,
        root,
        libraries_dir,
    })
}

/// Mirror of `fbuild_build::framework_libs::framework_include_scan_roots`.
///
/// Kept in-crate (rather than depending on `fbuild-build`) so this diagnostic
/// is a leaf binary and the CLI doesn't drag in the orchestrator graph.
fn project_scan_roots(project_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for sub in ["src", "include", "lib"] {
        let p = project_dir.join(sub);
        if p.exists() && !roots.iter().any(|r: &PathBuf| r == &p) {
            roots.push(p);
        }
    }
    roots
}

fn project_search_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = roots.to_vec();
    for root in roots {
        if !is_library_root(root) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            push_existing_unique(&mut paths, dir.clone());
            push_existing_unique(&mut paths, dir.join("src"));
        }
    }
    paths
}

fn collect_project_seeds(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    for root in roots {
        if is_library_root(root) {
            continue;
        }
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(should_scan_entry)
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if is_translation_unit(entry.path()) {
                seeds.push(entry.path().to_path_buf());
            }
        }
    }
    seeds
}

fn push_existing_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() && !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn is_library_root(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("lib"))
        .unwrap_or(false)
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".pio"
            | ".fbuild"
            | ".zap"
            | ".build"
            | "build"
            | "target"
            | ".venv"
            | "venv"
            | "node_modules"
            | "__pycache__"
    )
}

fn is_translation_unit(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(ext.as_str(), "c" | "cpp" | "cc" | "cxx" | "s" | "ino")
}

/// For each selected library, find one reached file under its include dirs to
/// show as "triggered by". We canonicalize both sides so prefix matching works
/// across the same path-canonicalization quirks (`/private/var`, `\\?\`, ...)
/// that the resolver itself handles.
fn first_reached_under(included: &[PathBuf], lib: &FrameworkLibrary) -> Option<PathBuf> {
    let canon_dirs: Vec<PathBuf> = lib
        .include_dirs
        .iter()
        .map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()))
        .collect();
    included
        .iter()
        .find(|p| canon_dirs.iter().any(|d| normalized_path_is_under(p, d)))
        .cloned()
}

fn normalized_path_is_under(path: &Path, dir: &Path) -> bool {
    let path_key = normalize_for_key(path);
    let dir_key = normalize_for_key(dir);
    if path_key == dir_key {
        return true;
    }
    let dir_prefix = if dir_key.ends_with('/') {
        dir_key
    } else {
        format!("{dir_key}/")
    };
    path_key.starts_with(&dir_prefix)
}

fn emit_explain(
    project_dir: &Path,
    env_name: &str,
    framework: &FrameworkInfo,
    libraries: &[FrameworkLibrary],
    seeds: &[PathBuf],
    sel: &Selection,
) {
    println!("project: {}", project_dir.display());
    println!("env:     {}", env_name);
    println!(
        "framework: {} @ {}",
        framework.name,
        framework.root.display()
    );
    println!();
    println!("selected libraries ({}):", sel.required_libraries.len());
    for name in &sel.required_libraries {
        if let Some(lib) = libraries.iter().find(|l| &l.name == name) {
            let trigger = first_reached_under(&sel.included_files, lib)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string());
            println!(
                "  {:<14} (include_dirs: {}, sources: {}, triggered by: {})",
                lib.name,
                lib.include_dirs.len(),
                lib.source_files.len(),
                trigger
            );
        }
    }
    println!();
    println!("unresolved includes ({}):", sel.unresolved.len());
    for u in &sel.unresolved {
        println!("  {}", u);
    }
    println!();
    println!(
        "scanned: {} source files, {} headers reached.",
        seeds.len(),
        sel.included_files.len()
    );
}

fn emit_json(
    project_dir: &Path,
    env_name: &str,
    framework: &FrameworkInfo,
    libraries: &[FrameworkLibrary],
    seeds: &[PathBuf],
    sel: &Selection,
) {
    let selected: Vec<serde_json::Value> = sel
        .required_libraries
        .iter()
        .map(|name| {
            let lib = libraries.iter().find(|l| &l.name == name);
            let triggered = lib
                .and_then(|l| first_reached_under(&sel.included_files, l))
                .map(|p| p.display().to_string());
            json!({
                "name": name,
                "include_dirs": lib.map(|l| l.include_dirs.len()).unwrap_or(0),
                "source_count": lib.map(|l| l.source_files.len()).unwrap_or(0),
                "triggered_by": triggered,
            })
        })
        .collect();

    let payload = json!({
        "project": project_dir.display().to_string(),
        "env": env_name,
        "framework": {
            "name": framework.name,
            "root": framework.root.display().to_string(),
            "libraries_dir": framework.libraries_dir.display().to_string(),
        },
        "seeds": seeds
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "included_files": sel
            .included_files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        "selected": selected,
        "unresolved": sel.unresolved,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .expect("fbuild-cli: lib-select JSON payload is built from primitives, serialization is infallible")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_path_is_under_matches_exact_path() {
        assert!(normalized_path_is_under(
            Path::new("/foo/bar"),
            Path::new("/foo/bar")
        ));
    }

    #[test]
    fn normalized_path_is_under_matches_nested_path() {
        assert!(normalized_path_is_under(
            Path::new("/foo/bar/baz.h"),
            Path::new("/foo/bar")
        ));
    }

    #[test]
    fn normalized_path_is_under_handles_dir_trailing_slash() {
        assert!(normalized_path_is_under(
            Path::new("/foo/bar/baz.h"),
            Path::new("/foo/bar/")
        ));
    }

    #[test]
    fn normalized_path_is_under_rejects_sibling_prefix() {
        assert!(!normalized_path_is_under(
            Path::new("/foo/barbaz/header.h"),
            Path::new("/foo/bar")
        ));
    }
}
