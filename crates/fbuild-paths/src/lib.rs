//! Path resolution for fbuild.
//!
//! Single source of truth for all `.fbuild` paths.
//! Respects `FBUILD_DEV_MODE=1` for dev/prod isolation.

use std::path::{Path, PathBuf};

/// Check if running in development mode.
pub fn is_dev_mode() -> bool {
    std::env::var("FBUILD_DEV_MODE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Root fbuild directory: `~/.fbuild/{dev|prod}`
pub fn get_fbuild_root() -> PathBuf {
    let home = dirs_next().expect("could not determine home directory");
    let mode = if is_dev_mode() { "dev" } else { "prod" };
    home.join(".fbuild").join(mode)
}

/// Root fbuild directory for the OTHER mode (cross-mode fallback).
///
/// If current mode is dev, returns prod root, and vice versa.
/// Used for cross-mode daemon discovery.
pub fn get_other_fbuild_root() -> PathBuf {
    let home = dirs_next().expect("could not determine home directory");
    let mode = if is_dev_mode() { "prod" } else { "dev" };
    home.join(".fbuild").join(mode)
}

/// Daemon files directory.
pub fn get_daemon_dir() -> PathBuf {
    get_fbuild_root().join("daemon")
}

/// Daemon PID file path.
pub fn get_daemon_pid_file() -> PathBuf {
    get_daemon_dir().join("fbuild_daemon.pid")
}

/// Daemon port file path (written by daemon so clients can discover the port).
pub fn get_daemon_port_file() -> PathBuf {
    get_daemon_dir().join("daemon.port")
}

/// Daemon log file path.
pub fn get_daemon_log_file() -> PathBuf {
    get_daemon_dir().join("daemon.log")
}

/// Daemon status file path (written by daemon for CLI-side status reading without HTTP).
pub fn get_daemon_status_file() -> PathBuf {
    get_daemon_dir().join("daemon_status.json")
}

/// Global cache root (or `FBUILD_CACHE_DIR` override).
pub fn get_cache_root() -> PathBuf {
    if let Ok(dir) = std::env::var("FBUILD_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    get_fbuild_root().join("cache")
}

/// Project-local `.fbuild` directory.
pub fn get_project_fbuild_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".fbuild")
}

/// Project build root.
///
/// Priority:
/// 1. `FBUILD_BUILD_DIR` environment variable (explicit override, useful for
///    Windows where long project paths can exceed the 260 character limit)
/// 2. Default: `<project_dir>/.fbuild/build/`
pub fn get_project_build_root(project_dir: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("FBUILD_BUILD_DIR") {
        return PathBuf::from(dir);
    }
    get_project_fbuild_dir(project_dir).join("build")
}

/// Read and validate a port number from a port file.
fn read_port_from_file(path: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(path).ok()?;
    let port: u16 = content.trim().parse().ok()?;
    if port > 0 {
        Some(port)
    } else {
        None
    }
}

/// Daemon port.
///
/// Priority:
/// 1. `FBUILD_DAEMON_PORT` environment variable (if set and valid 1–65535)
/// 2. Port file in current mode's daemon dir (if exists and valid)
/// 3. Port file in OTHER mode's daemon dir (cross-mode fallback —
///    handles dev daemon running but client not in dev mode, or vice versa)
/// 4. Mode-based default: 8865 (dev) or 8765 (prod)
pub fn get_daemon_port() -> u16 {
    // Priority 1: env var
    if let Ok(port_str) = std::env::var("FBUILD_DAEMON_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            if port > 0 {
                return port;
            }
        }
    }

    // Priority 2: port file in current mode's daemon dir
    let port_file = get_daemon_port_file();
    if let Some(port) = read_port_from_file(&port_file) {
        return port;
    }

    // Priority 3: cross-mode fallback — check the OTHER mode's port file
    let other_port_file = get_other_fbuild_root().join("daemon").join("daemon.port");
    if let Some(port) = read_port_from_file(&other_port_file) {
        return port;
    }

    // Priority 4: mode-based default
    if is_dev_mode() {
        8865
    } else {
        8765
    }
}

/// Daemon URL.
pub fn get_daemon_url() -> String {
    format!("http://127.0.0.1:{}", get_daemon_port())
}

/// PlatformIO home directory: `PLATFORMIO_HOME` env var or `~/.platformio`.
pub fn get_platformio_home() -> PathBuf {
    if let Ok(dir) = std::env::var("PLATFORMIO_HOME") {
        return PathBuf::from(dir);
    }
    dirs_next()
        .expect("could not determine home directory")
        .join(".platformio")
}

/// Path to a PlatformIO package: `<platformio_home>/packages/<package_name>`.
pub fn get_platformio_package(package_name: &str) -> PathBuf {
    get_platformio_home().join("packages").join(package_name)
}

/// Build profile names, ordered by preference for firmware discovery.
const BUILD_PROFILES: &[&str] = &["release", "quick"];

/// Firmware file names, ordered by preference.
const FIRMWARE_NAMES: &[&str] = &["firmware.bin", "firmware.hex", "firmware.elf"];

/// Find a firmware file in the project build directory.
///
/// Searches profile subdirectories (release, quick) first, then the base
/// environment directory, then the legacy `.pio/build` directory.
///
/// If `firmware_name` is `None`, searches for all known firmware names
/// in preference order.
pub fn find_firmware(
    project_dir: &Path,
    env_name: &str,
    firmware_name: Option<&str>,
) -> Option<PathBuf> {
    let names: Vec<&str> = match firmware_name {
        Some(name) => vec![name],
        None => FIRMWARE_NAMES.to_vec(),
    };

    let base_build_dir = get_project_build_root(project_dir).join(env_name);

    // Check profile subdirectories first (release, quick), then base env dir
    let mut search_dirs: Vec<PathBuf> = BUILD_PROFILES
        .iter()
        .map(|profile| base_build_dir.join(profile))
        .collect();
    search_dirs.push(base_build_dir);

    // Also check legacy .pio/build location
    search_dirs.push(project_dir.join(".pio").join("build").join(env_name));

    for search_dir in &search_dirs {
        if !search_dir.exists() {
            continue;
        }
        for name in &names {
            let candidate = search_dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Find the build directory containing firmware for the given environment.
///
/// Like `find_firmware()` but returns the directory, not the file.
/// Useful when you need sibling files (bootloader.bin, partitions.bin).
pub fn find_firmware_dir(project_dir: &Path, env_name: &str) -> Option<PathBuf> {
    find_firmware(project_dir, env_name, None).map(|p| p.parent().unwrap().to_path_buf())
}

fn dirs_next() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mode_port() {
        // Note: can't set env vars in parallel tests safely,
        // so just test the function exists and returns a valid port
        let port = get_daemon_port();
        assert!(port > 0);
    }

    #[test]
    fn other_fbuild_root_is_opposite_mode() {
        // get_other_fbuild_root should return the opposite mode's root
        let root = get_fbuild_root();
        let other = get_other_fbuild_root();
        // They must differ (one ends with dev, other with prod)
        assert_ne!(root, other);
        let root_str = root.to_string_lossy();
        let other_str = other.to_string_lossy();
        assert!(
            (root_str.ends_with("dev") && other_str.ends_with("prod"))
                || (root_str.ends_with("prod") && other_str.ends_with("dev"))
        );
    }

    #[test]
    fn find_firmware_returns_none_for_missing_dir() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_none");
        assert!(find_firmware(&tmp, "esp32dev", None).is_none());
    }

    #[test]
    fn find_firmware_finds_bin_in_release_profile() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_bin");
        let fw_dir = tmp
            .join(".fbuild")
            .join("build")
            .join("esp32dev")
            .join("release");
        std::fs::create_dir_all(&fw_dir).unwrap();
        let fw_file = fw_dir.join("firmware.bin");
        std::fs::write(&fw_file, b"fake").unwrap();

        let result = find_firmware(&tmp, "esp32dev", None);
        assert_eq!(result.unwrap(), fw_file);

        // find_firmware_dir returns the directory
        let dir = find_firmware_dir(&tmp, "esp32dev");
        assert_eq!(dir.unwrap(), fw_dir);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn find_firmware_prefers_release_over_quick() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_pref");
        let release_dir = tmp
            .join(".fbuild")
            .join("build")
            .join("env1")
            .join("release");
        let quick_dir = tmp.join(".fbuild").join("build").join("env1").join("quick");
        std::fs::create_dir_all(&release_dir).unwrap();
        std::fs::create_dir_all(&quick_dir).unwrap();
        std::fs::write(release_dir.join("firmware.hex"), b"rel").unwrap();
        std::fs::write(quick_dir.join("firmware.hex"), b"quick").unwrap();

        let result = find_firmware(&tmp, "env1", None).unwrap();
        // release is searched first
        assert!(result.to_string_lossy().contains("release"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn find_firmware_specific_name() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_specific");
        let fw_dir = tmp
            .join(".fbuild")
            .join("build")
            .join("myenv")
            .join("release");
        std::fs::create_dir_all(&fw_dir).unwrap();
        std::fs::write(fw_dir.join("firmware.bin"), b"bin").unwrap();
        std::fs::write(fw_dir.join("firmware.hex"), b"hex").unwrap();

        // When asking for specific name, only that name matches
        let result = find_firmware(&tmp, "myenv", Some("firmware.hex")).unwrap();
        assert!(result.to_string_lossy().contains("firmware.hex"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn find_firmware_legacy_pio_build() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_pio");
        let pio_dir = tmp.join(".pio").join("build").join("uno");
        std::fs::create_dir_all(&pio_dir).unwrap();
        std::fs::write(pio_dir.join("firmware.hex"), b"legacy").unwrap();

        let result = find_firmware(&tmp, "uno", None).unwrap();
        assert!(result.to_string_lossy().contains(".pio"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn platformio_home_defaults_to_dot_platformio() {
        // When PLATFORMIO_HOME is not set, should be ~/.platformio
        let home = get_platformio_home();
        assert!(home.ends_with(".platformio"));
    }

    #[test]
    fn platformio_package_appends_packages_subdir() {
        let pkg = get_platformio_package("tool-avrdude");
        assert!(pkg.ends_with("packages/tool-avrdude") || pkg.ends_with("packages\\tool-avrdude"));
    }
}
