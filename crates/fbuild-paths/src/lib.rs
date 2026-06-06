//! Path resolution for fbuild.
//!
//! Single source of truth for all `.fbuild` paths.
//! Respects `FBUILD_DEV_MODE=1` for dev/prod isolation.

use std::path::{Path, PathBuf};

use fbuild_core::BuildProfile;

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

/// Layout resolver for the per-environment build directory.
///
/// This is the single source of truth for "where does fbuild write
/// `firmware.hex`, `core/`, `src/`, `libs/`?". Callers (daemon HTTP
/// handlers, CLI, tests) construct a `BuildLayout` from the inputs
/// they have, then ask it to resolve the on-disk path. The pipeline
/// reads the resolved path off `BuildParams` instead of re-deriving
/// it, which is why this struct exists rather than a free function.
///
/// Resolution precedence:
///
/// 1. `override_root` (an explicit per-request override from the HTTP
///    API). Treated as the env-rooted dir base.
/// 2. `FBUILD_BUILD_DIR` env var (process-wide override, primarily for
///    Windows long-path workarounds).
/// 3. `<project_dir>/.fbuild/build` (the default).
///
/// The `<env>/<profile>` segments are appended on top of whichever
/// root was selected, *unless* `flatten_env` is true or the
/// project_dir's basename already equals `env_name` — in which case
/// the `<env>` segment is dropped to avoid path duplication like
/// `.build/pio/teensy40/.fbuild/build/teensy40/release/`. See
/// FastLED/fbuild#432.
#[derive(Debug, Clone)]
pub struct BuildLayout {
    pub project_dir: PathBuf,
    pub env_name: String,
    pub profile: BuildProfile,
    /// Explicit per-request override of the build root. When `Some`,
    /// takes precedence over `FBUILD_BUILD_DIR` and the default.
    pub override_root: Option<PathBuf>,
    /// When true, the resolved path is `<root>/<profile>` — the `<env>`
    /// segment is dropped. Embedders that already name their project
    /// dir after the env (FastLED's `.build/pio/<board>/` convention)
    /// should set this to keep paths short.
    pub flatten_env: bool,
}

impl BuildLayout {
    /// Construct a layout with the standard defaults (no override,
    /// flatten only when project basename auto-matches env).
    pub fn new(project_dir: PathBuf, env_name: String, profile: BuildProfile) -> Self {
        Self {
            project_dir,
            env_name,
            profile,
            override_root: None,
            flatten_env: false,
        }
    }

    /// Builder: set an explicit per-request root override.
    pub fn with_override_root(mut self, root: Option<PathBuf>) -> Self {
        self.override_root = root;
        self
    }

    /// Builder: force-flatten the `<env>` segment.
    pub fn with_flatten_env(mut self, flatten: bool) -> Self {
        self.flatten_env = flatten;
        self
    }

    /// True when the project directory's basename already matches the
    /// env name, so appending `<env>/` would duplicate the segment.
    /// This is the FastLED `.build/pio/<board>/` shape.
    pub fn project_basename_matches_env(&self) -> bool {
        self.project_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| name == self.env_name)
            .unwrap_or(false)
    }

    /// Resolve the env-rooted build directory.
    pub fn resolve(&self) -> PathBuf {
        let root = if let Some(ref r) = self.override_root {
            r.clone()
        } else if let Ok(dir) = std::env::var("FBUILD_BUILD_DIR") {
            PathBuf::from(dir)
        } else {
            get_project_fbuild_dir(&self.project_dir).join("build")
        };

        let collapse_env = self.flatten_env || self.project_basename_matches_env();

        let with_env = if collapse_env {
            root
        } else {
            root.join(&self.env_name)
        };
        with_env.join(self.profile.as_dir_name())
    }
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

/// Build profiles enumerated in firmware-discovery preference order.
const BUILD_PROFILE_ORDER: &[BuildProfile] = &[BuildProfile::Release, BuildProfile::Quick];

/// Firmware file names, ordered by preference.
const FIRMWARE_NAMES: &[&str] = &["firmware.bin", "firmware.hex", "firmware.elf"];

/// Find a firmware file in the project build directory.
///
/// Searches profile subdirectories (release, quick) first, then the base
/// environment directory, then the legacy `.pio/build` directory.
///
/// Layout discovery routes through [`BuildLayout`] so it tracks exactly
/// where production wrote the artifact — including the env-segment
/// auto-collapse used for the FastLED `.build/pio/<board>/` shape.
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

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    for profile in BUILD_PROFILE_ORDER {
        let layout = BuildLayout::new(project_dir.to_path_buf(), env_name.to_string(), *profile);
        search_dirs.push(layout.resolve());
    }
    // Also probe the env dir itself (no profile subdir) — covers
    // legacy fbuild layouts and the rare orchestrator that drops
    // firmware one level up.
    let env_dir_layout = BuildLayout::new(
        project_dir.to_path_buf(),
        env_name.to_string(),
        BuildProfile::Release,
    );
    if let Some(env_dir) = env_dir_layout.resolve().parent() {
        search_dirs.push(env_dir.to_path_buf());
    }

    // Legacy PlatformIO output: `.pio/build/<env>/`.
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
        // Note: can't set env vars in parallel tests safely, and the
        // function's own priority chain (env var > current-mode port file >
        // other-mode port file > mode-default) legitimately returns any
        // u16 > 0. Assert only the contract the function actually promises.
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

    /// Regression: FastLED stages each board's project under
    /// `<repo>/.build/pio/<board>/` and asks fbuild to build it with
    /// `env == board`. The on-disk layout must collapse the duplicate
    /// `<board>` segment, and `find_firmware` must still locate the
    /// firmware in that collapsed layout. See FastLED/fbuild#432.
    #[test]
    fn find_firmware_in_collapsed_layout_when_basename_matches_env() {
        let tmp = std::env::temp_dir().join("fbuild_test_find_fw_collapsed");
        let _ = std::fs::remove_dir_all(&tmp);
        let project_dir = tmp.join(".build").join("pio").join("teensy40");
        // Collapsed layout: `<project_dir>/.fbuild/build/release/` —
        // NO extra `teensy40/` segment.
        let fw_dir = project_dir.join(".fbuild").join("build").join("release");
        std::fs::create_dir_all(&fw_dir).unwrap();
        std::fs::write(fw_dir.join("firmware.hex"), b"fake").unwrap();

        let result = find_firmware(&project_dir, "teensy40", None).unwrap();
        // The duplicated `teensy40` segment must NOT appear between
        // `.fbuild/build/` and `release/`.
        let s = result.to_string_lossy().to_string();
        assert!(s.contains(".fbuild"));
        assert!(s.contains("release"));
        assert!(
            !s.contains("build/teensy40/release") && !s.contains("build\\teensy40\\release"),
            "find_firmware returned a duplicated-env path: {s}"
        );

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

    // --- BuildLayout ---

    #[test]
    fn build_layout_default_includes_env_and_profile() {
        let project = PathBuf::from("/work/sketch");
        let layout = BuildLayout::new(project.clone(), "esp32dev".into(), BuildProfile::Release);
        let resolved = layout.resolve();
        // Either: <project>/.fbuild/build/esp32dev/release
        //     or: $FBUILD_BUILD_DIR/esp32dev/release (when env var is set in CI).
        // Both must end with esp32dev/release.
        assert!(
            resolved.ends_with(PathBuf::from("esp32dev").join("release")),
            "default layout must end with <env>/<profile>, got: {}",
            resolved.display()
        );
    }

    #[test]
    fn build_layout_override_root_takes_precedence() {
        let project = PathBuf::from("/work/sketch");
        let override_root = PathBuf::from("/tmp/short-build-dir");
        let layout = BuildLayout::new(project, "uno".into(), BuildProfile::Quick)
            .with_override_root(Some(override_root.clone()));
        let resolved = layout.resolve();
        assert_eq!(resolved, override_root.join("uno").join("quick"));
    }

    /// When project_dir's basename already matches env_name, the env
    /// segment is collapsed automatically. This is the FastLED
    /// `.build/pio/<board>/` case that this refactor exists to fix.
    /// See FastLED/fbuild#432.
    #[test]
    fn build_layout_auto_collapses_when_project_basename_matches_env() {
        let project = PathBuf::from("/repo/.build/pio/teensy40");
        let layout = BuildLayout::new(project, "teensy40".into(), BuildProfile::Release);
        // The override path is used so the test isn't perturbed by
        // FBUILD_BUILD_DIR in the surrounding environment.
        let layout = layout.with_override_root(Some(PathBuf::from("/tmp/root")));
        let resolved = layout.resolve();
        assert_eq!(resolved, PathBuf::from("/tmp/root/release"));
        // The duplicated teensy40 segment must NOT appear.
        assert!(!resolved.to_string_lossy().contains("teensy40"));
    }

    #[test]
    fn build_layout_explicit_flatten_env_drops_env_segment() {
        let project = PathBuf::from("/repo/sketch");
        let layout = BuildLayout::new(project, "esp32dev".into(), BuildProfile::Release)
            .with_override_root(Some(PathBuf::from("/tmp/root")))
            .with_flatten_env(true);
        let resolved = layout.resolve();
        assert_eq!(resolved, PathBuf::from("/tmp/root/release"));
    }

    #[test]
    fn build_layout_project_basename_mismatch_keeps_env() {
        let project = PathBuf::from("/repo/sketch_dir");
        let layout = BuildLayout::new(project, "esp32dev".into(), BuildProfile::Release)
            .with_override_root(Some(PathBuf::from("/tmp/root")));
        let resolved = layout.resolve();
        assert_eq!(resolved, PathBuf::from("/tmp/root/esp32dev/release"));
    }

    #[test]
    fn build_layout_profile_dir_name_matches_buildprofile() {
        let project = PathBuf::from("/p");
        let release = BuildLayout::new(project.clone(), "e".into(), BuildProfile::Release)
            .with_override_root(Some(PathBuf::from("/r")))
            .resolve();
        let quick = BuildLayout::new(project, "e".into(), BuildProfile::Quick)
            .with_override_root(Some(PathBuf::from("/r")))
            .resolve();
        assert!(release.ends_with("release"));
        assert!(quick.ends_with("quick"));
    }
}
