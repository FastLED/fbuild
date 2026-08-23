//! Path resolution for fbuild.
//!
//! Single source of truth for all `.fbuild` paths.
//! Respects `FBUILD_DEV_MODE=1` for dev/prod isolation.

use std::path::{Path, PathBuf};

use fbuild_core::BuildProfile;

pub mod daemon_ownership;
pub mod dev_daemon_namespace;
pub mod running_process;

/// The project-local and home-local fbuild directory segment: `.fbuild`.
///
/// This is the canonical spelling. Nothing outside this crate should write
/// the literal — see the `ban_raw_fbuild_path` Dylint
/// (FastLED/fbuild#1349).
pub const FBUILD_DIR_NAME: &str = ".fbuild";

/// The build-tree segment directly under [`FBUILD_DIR_NAME`]: `build`.
///
/// `<project>/.fbuild/build/` is the default build root; note that
/// `FBUILD_BUILD_DIR` can replace the whole root, so prefer
/// [`get_project_build_root`] or [`BuildLayout`] over joining this
/// segment by hand.
pub const BUILD_DIR_NAME: &str = "build";

/// Check if running in development mode.
pub fn is_dev_mode() -> bool {
    std::env::var("FBUILD_DEV_MODE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Root fbuild directory: `~/.fbuild/{dev|prod}`
///
/// Panics when the home directory cannot be determined. Callers that must
/// degrade instead of panicking want [`try_get_fbuild_root`].
pub fn get_fbuild_root() -> PathBuf {
    try_get_fbuild_root().expect("could not determine home directory")
}

/// [`get_fbuild_root`] for callers that report a missing home directory
/// rather than panicking on it.
///
/// Exists because several tool resolvers return `Option`/`Result` precisely
/// so a home-less environment is a diagnosable failure and not a crash; they
/// had each hand-rolled this path to keep that property (FastLED/fbuild#1349).
pub fn try_get_fbuild_root() -> Option<PathBuf> {
    let mode = if is_dev_mode() { "dev" } else { "prod" };
    Some(dirs_next()?.join(FBUILD_DIR_NAME).join(mode))
}

/// The segment holding fbuild-managed external tools, under
/// [`get_fbuild_root`]: `tools`.
pub const TOOLS_DIR_NAME: &str = "tools";

/// Where fbuild installs and looks for managed external tools:
/// `~/.fbuild/{dev|prod}/tools`.
///
/// Panics when the home directory cannot be determined; see
/// [`try_get_tools_dir`].
pub fn get_tools_dir() -> PathBuf {
    get_fbuild_root().join(TOOLS_DIR_NAME)
}

/// [`get_tools_dir`] for callers that report a missing home directory rather
/// than panicking on it.
pub fn try_get_tools_dir() -> Option<PathBuf> {
    Some(try_get_fbuild_root()?.join(TOOLS_DIR_NAME))
}

/// Human-facing label for [`get_tools_dir`] when the real path cannot be
/// resolved — `~/.fbuild/{dev|prod}/tools`.
///
/// Diagnostics that tell a user where to install a managed tool need
/// *something* to print even on a host with no discoverable home directory.
/// Producing it here keeps those messages honest about the current mode, and
/// keeps the `.fbuild` spelling from being re-typed at each call site.
pub fn tools_dir_label() -> String {
    let mode = if is_dev_mode() { "dev" } else { "prod" };
    format!("~/{FBUILD_DIR_NAME}/{mode}/{TOOLS_DIR_NAME}")
}

/// Root fbuild directory for the OTHER mode (cross-mode fallback).
///
/// If current mode is dev, returns prod root, and vice versa.
/// Used for cross-mode daemon discovery.
pub fn get_other_fbuild_root() -> PathBuf {
    let home = dirs_next().expect("could not determine home directory");
    let mode = if is_dev_mode() { "prod" } else { "dev" };
    home.join(FBUILD_DIR_NAME).join(mode)
}

/// Daemon files directory.
pub fn get_daemon_dir() -> PathBuf {
    get_fbuild_root().join("daemon")
}

/// Daemon PID file path.
pub fn get_daemon_pid_file() -> PathBuf {
    get_daemon_dir().join("fbuild_daemon.pid")
}

/// Short, stable hex key identifying this daemon *endpoint*: a hash of the
/// backend version + the cache identity (mode + trust + cache-root + schema).
///
/// FastLED/fbuild#1009: the default endpoint used to be a fixed per-user port
/// (8765/8865) shared by every checkout, so a daemon of a *different version*
/// could silently serve another checkout's builds. Keying the endpoint on
/// version+identity means daemons of different versions land on distinct ports
/// and distinct port files — they can no longer serve each other. Two checkouts
/// of the SAME version sharing the SAME cache still (correctly) share a daemon,
/// which is not a wrong-version hazard.
///
/// `fbuild-paths` is workspace-versioned, so `env!("CARGO_PKG_VERSION")` here is
/// identical to the value compiled into the CLI and the daemon — both sides
/// derive the same key without any handshake.
pub fn daemon_endpoint_key() -> String {
    let identity = crate::running_process::DaemonCacheIdentity::discover();
    let material = format!("{}|{}", env!("CARGO_PKG_VERSION"), identity.label_value());
    endpoint_key_from_material(&material)
}

/// FNV-1a (64-bit) of `material`, formatted as 16 lowercase hex chars.
/// Deterministic + dependency-free; pure (unit-tested).
fn endpoint_key_from_material(material: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in material.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Deterministic default daemon port derived from [`daemon_endpoint_key`].
///
/// On Windows this lands in 10000–49151, below the dynamic range that
/// Hyper-V/HNS commonly reserves in excluded blocks. Other platforms use the
/// IANA dynamic range (49152–65535). The result is stable for a given
/// version+identity; distinct versions (and dev vs prod, since mode is part of
/// the identity) get distinct ports. `FBUILD_DAEMON_PORT` still overrides this
/// (see [`get_daemon_port`]).
pub fn default_daemon_port() -> u16 {
    port_from_endpoint_key(&daemon_endpoint_key())
}

/// Map a hex endpoint key into the platform's default daemon-port window.
/// Pure and deterministic (unit-tested).
fn port_from_endpoint_key(key: &str) -> u16 {
    let (low, span): (u32, u32) = if fbuild_core::platform::host::is_windows() {
        (10000, 49152 - 10000)
    } else {
        (49152, 65536 - 49152)
    };

    let n = u64::from_str_radix(key, 16).unwrap_or(0);
    (low + (n % u64::from(span)) as u32) as u16
}

/// Daemon port file path (written by daemon so clients can discover the port).
///
/// Keyed by [`daemon_endpoint_key`] (FastLED/fbuild#1009) so daemons of
/// different versions/identities write distinct files and never read each
/// other's port.
pub fn get_daemon_port_file() -> PathBuf {
    get_daemon_dir().join(format!("daemon-{}.port", daemon_endpoint_key()))
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

/// Root directory for short-lived working / scratch dirs.
///
/// FastLED/fbuild#844 ("Bridge pair 10"). Returns `~/.fbuild/{dev|prod}/tmp`.
/// This is the rooted alternative to `std::env::temp_dir()` and
/// `tempfile::tempdir()`; per-platform package extractors, framework
/// hydration, linker scratch dirs, etc. all live under this root so
/// every byte fbuild writes is reachable from a single user-visible
/// directory.
///
/// Note: this does NOT create the directory — pair with
/// [`temp_subdir`] for the create-on-use pattern.
pub fn dev_or_prod_temp_root() -> PathBuf {
    get_fbuild_root().join("tmp")
}

/// Get (and create) a named subdirectory under [`dev_or_prod_temp_root`].
///
/// Best-effort `create_dir_all`: if creation fails the returned path
/// still points where the caller asked, and the next filesystem op
/// will surface the real error. This matches the convention every
/// other "ensure dir" helper in fbuild-paths uses.
pub fn temp_subdir(name: &str) -> PathBuf {
    let dir = dev_or_prod_temp_root().join(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Project-local `.fbuild` directory.
pub fn get_project_fbuild_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(FBUILD_DIR_NAME)
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
    get_project_fbuild_dir(project_dir).join(BUILD_DIR_NAME)
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
            get_project_fbuild_dir(&self.project_dir).join(BUILD_DIR_NAME)
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
    if port > 0 { Some(port) } else { None }
}

/// Daemon port.
///
/// Priority:
/// 1. `FBUILD_DAEMON_PORT` environment variable (if set and valid 1–65535)
/// 2. Port file for this endpoint (if it exists and is valid)
/// 3. [`default_daemon_port`] — a deterministic per-(version, cache-identity)
///    port
///
/// FastLED/fbuild#1009: the endpoint is keyed by version+identity (via
/// [`daemon_endpoint_key`]) rather than a fixed per-user port, so a daemon of a
/// different version can no longer bind the same endpoint and serve another
/// checkout's builds. The old cross-mode fallback (dev CLI adopting a prod
/// daemon and vice-versa) was an *anti*-isolation bridge and is intentionally
/// dropped — dev and prod already have separate roots and now separate ports.
pub fn get_daemon_port() -> u16 {
    // Priority 1: env var override.
    if let Ok(port_str) = std::env::var("FBUILD_DAEMON_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            if port > 0 {
                return port;
            }
        }
    }

    // Priority 2: this endpoint's port file — but only if we cannot prove its
    // writer is dead. FastLED/fbuild#1213: a crashed daemon's port file was
    // trusted verbatim forever.
    if let Some(port) = read_port_from_file(&get_daemon_port_file()) {
        if !recorded_daemon_owner_is_dead() {
            return port;
        }
        // Falls through to the derived default. Note this is usually the SAME
        // number, because the port is deterministic — see the note on
        // `recorded_daemon_owner_is_dead`.
    }

    // Priority 3: deterministic per-endpoint default.
    default_daemon_port()
}

/// Can we *prove* the daemon that wrote this endpoint's records is gone?
///
/// Fails safe: anything short of positive evidence of death returns `false`,
/// so a live daemon's endpoint is never discarded. In particular a missing
/// owner claim returns `false`, because the daemon writes its pid/port files
/// before the claim — treating that window as death would make a starting
/// daemon look dead.
///
/// The exe-stem check makes this PID-recycling-safe: a recycled PID now
/// running some other program means the daemon itself is gone.
///
/// Scope note: because [`default_daemon_port`] is deterministic, discarding a
/// stale port file usually yields the *same* port number. This is a
/// correctness/hygiene fix (stop trusting a record whose writer is provably
/// gone), NOT the reason a client can keep failing to reach a dead endpoint —
/// see the PR discussion on FastLED/fbuild#1213.
fn recorded_daemon_owner_is_dead() -> bool {
    let Some(claim) = daemon_ownership::read_owner_claim() else {
        return false;
    };
    if !fbuild_core::platform::process::pid_is_alive(claim.pid) {
        return true;
    }
    // Alive PID: only a *successful* probe showing a different program counts
    // as death, since `pid_exe_stem_matches` fails closed.
    match fbuild_core::platform::process::pid_executable_path(claim.pid) {
        Some(_) => !fbuild_core::platform::process::pid_exe_stem_matches(
            claim.pid,
            daemon_ownership::DAEMON_EXE_STEM,
        ),
        None => false,
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
    find_firmware(project_dir, env_name, None).map(|p| {
        p.parent()
            .expect("fbuild-paths: find_firmware always returns a file under a directory")
            .to_path_buf()
    })
}

fn dirs_next() -> Option<PathBuf> {
    if fbuild_core::platform::host::is_windows() {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    } else {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mode_port() {
        // Note: can't set env vars in parallel tests safely, and the
        // function's own priority chain (env var > endpoint port file >
        // per-endpoint default) legitimately returns any u16 > 0. Assert only
        // the contract the function actually promises.
        let port = get_daemon_port();
        assert!(port > 0);
    }

    /// FastLED/fbuild#1213: the liveness gate must only fire on *positive*
    /// evidence that the recorded daemon is gone. With no owner claim on
    /// disk — including the window where a starting daemon has written its
    /// port file but not yet its claim — the endpoint must be trusted.
    #[test]
    fn owner_liveness_gate_fails_safe_without_a_claim() {
        // `read_owner_claim` returns None when the claim file is absent or
        // malformed; in a test process there is no daemon claim for this
        // endpoint, so this exercises the fail-safe branch.
        if daemon_ownership::read_owner_claim().is_none() {
            assert!(
                !recorded_daemon_owner_is_dead(),
                "absent owner claim must NOT be read as a dead daemon"
            );
        }
    }

    /// A claim naming this very test process (alive, but not `fbuild-daemon`)
    /// must be classified as dead — that is the PID-recycling guard.
    #[test]
    fn owner_liveness_gate_treats_a_recycled_pid_as_dead() {
        // Probe the primitives directly rather than writing a claim to the
        // process-global claim path, which would race other tests.
        let pid = std::process::id();
        assert!(fbuild_core::platform::process::pid_is_alive(pid));
        assert!(
            !fbuild_core::platform::process::pid_exe_stem_matches(
                pid,
                daemon_ownership::DAEMON_EXE_STEM
            ),
            "the test binary must not be mistaken for {}",
            daemon_ownership::DAEMON_EXE_STEM
        );
    }

    #[test]
    fn endpoint_key_is_deterministic_16_hex() {
        // FastLED/fbuild#1009: the key must be deterministic per (version,
        // identity) so the CLI and daemon derive the same endpoint. Tested on
        // the pure hasher so it's immune to parallel env-var mutation.
        let a = endpoint_key_from_material("2.4.0|mode=prod;trust=local;schema=1;cache=/x");
        let b = endpoint_key_from_material("2.4.0|mode=prod;trust=local;schema=1;cache=/x");
        assert_eq!(a, b, "same material must hash identically");
        assert_eq!(a.len(), 16, "expected 16 hex chars, got {a:?}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different version → different key (this is the #1009 isolation).
        let other_version =
            endpoint_key_from_material("2.5.0|mode=prod;trust=local;schema=1;cache=/x");
        assert_ne!(a, other_version, "different version must key differently");
        // Live key is well-formed too.
        let live = daemon_endpoint_key();
        assert_eq!(live.len(), 16);
        assert!(live.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn default_daemon_port_is_in_host_range() {
        let p = default_daemon_port();
        if fbuild_core::platform::host::is_windows() {
            assert!(
                (10000..49152).contains(&p),
                "Windows daemon port {p} overlaps the dynamic exclusion range"
            );
        } else {
            assert!(
                (49152..=65535).contains(&p),
                "port {p} outside dynamic range"
            );
        }
    }

    #[test]
    fn port_from_key_is_deterministic_and_ranged() {
        // Same key → same port; keys differing (e.g. by version or checkout)
        // map into the platform range and generally differ.
        assert_eq!(
            port_from_endpoint_key("0123456789abcdef"),
            port_from_endpoint_key("0123456789abcdef")
        );
        for key in ["0000000000000000", "ffffffffffffffff", "deadbeefcafef00d"] {
            let p = port_from_endpoint_key(key);
            if fbuild_core::platform::host::is_windows() {
                assert!((10000..49152).contains(&p), "key {key} -> {p} out of range");
            } else {
                assert!(
                    (49152..=65535).contains(&p),
                    "key {key} -> {p} out of range"
                );
            }
        }
        // Two distinct version/identity keys should not collapse to one port
        // for these representative values.
        assert_ne!(
            port_from_endpoint_key("1111111111111111"),
            port_from_endpoint_key("2222222222222222")
        );
    }

    #[test]
    fn port_file_name_is_endpoint_keyed() {
        // The port file must carry the endpoint key so different versions /
        // identities never read each other's port. FastLED/fbuild#1009.
        let file = get_daemon_port_file();
        let name = file.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("daemon-") && name.ends_with(".port"),
            "unexpected port file name: {name}"
        );
        assert!(name.contains(&daemon_endpoint_key()));
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
        let tmp = temp_subdir("fbuild_test_find_fw_none");
        assert!(find_firmware(&tmp, "esp32dev", None).is_none());
    }

    #[test]
    fn find_firmware_finds_bin_in_release_profile() {
        let tmp = temp_subdir("fbuild_test_find_fw_bin");
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
        let tmp = temp_subdir("fbuild_test_find_fw_pref");
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
        let tmp = temp_subdir("fbuild_test_find_fw_specific");
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
        let tmp = temp_subdir("fbuild_test_find_fw_collapsed");
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
        let tmp = temp_subdir("fbuild_test_find_fw_pio");
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
    fn dev_or_prod_temp_root_lives_under_fbuild_root() {
        let temp_root = dev_or_prod_temp_root();
        let fbuild_root = get_fbuild_root();
        assert!(
            temp_root.starts_with(&fbuild_root),
            "temp root {} must live under fbuild root {}",
            temp_root.display(),
            fbuild_root.display()
        );
        assert!(temp_root.ends_with("tmp"));
    }

    #[test]
    fn temp_subdir_creates_and_returns_path() {
        let dir = temp_subdir("__fbuild_test_temp_subdir__");
        assert!(dir.exists() || dir.parent().map(|p| !p.exists()).unwrap_or(true));
        // Cleanup best-effort.
        let _ = std::fs::remove_dir_all(&dir);
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
