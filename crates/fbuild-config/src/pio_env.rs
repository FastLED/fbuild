//! PlatformIO env var override container, decoupled from `std::env`.
//!
//! `PioEnvOverrides` is a per-request snapshot of `PLATFORMIO_*` environment
//! variables forwarded from the CLI caller to the daemon over HTTP. The daemon
//! does not inherit caller env vars, so all env-driven config must flow through
//! this struct rather than being read from `std::env::var` inside the build
//! pipeline.
//!
//! Only `fbuild-cli` (the entry point) and `fbuild-paths` (process-startup
//! fallbacks) may read `PLATFORMIO_*` directly from the process environment.

use std::collections::BTreeMap;
use std::collections::HashSet;

/// Canonical list of `PLATFORMIO_*` env vars fbuild understands and acts on.
pub const SUPPORTED_PIO_ENV_VARS: &[&str] = &[
    "PLATFORMIO_SRC_DIR",
    "PLATFORMIO_BUILD_FLAGS",
    "PLATFORMIO_BUILD_SRC_FLAGS",
    "PLATFORMIO_BUILD_UNFLAGS",
    "PLATFORMIO_BUILD_SRC_FILTER",
    "PLATFORMIO_UPLOAD_PORT",
    "PLATFORMIO_DEFAULT_ENVS",
    "PLATFORMIO_INCLUDE_DIR",
    "PLATFORMIO_LIB_DIR",
    "PLATFORMIO_LIB_EXTRA_DIRS",
    "PLATFORMIO_CORE_DIR",
    "PLATFORMIO_WORKSPACE_DIR",
    "PLATFORMIO_BUILD_DIR",
    "PLATFORMIO_LIBDEPS_DIR",
    "PLATFORMIO_PACKAGES_DIR",
    "PLATFORMIO_PLATFORMS_DIR",
    "PLATFORMIO_BOARDS_DIR",
    "PLATFORMIO_CACHE_DIR",
    "PLATFORMIO_BUILD_CACHE_DIR",
    "PLATFORMIO_DATA_DIR",
    "PLATFORMIO_TEST_DIR",
    "PLATFORMIO_GLOBALLIB_DIR",
    "PLATFORMIO_RUN_JOBS",
    "PLATFORMIO_UPLOAD_FLAGS",
];

/// `PLATFORMIO_*` env vars fbuild recognizes but does not act on. Setting one
/// of these triggers a "recognized but no-op" warning instead of an "unsupported
/// and ignored" warning.
pub const WARN_ONLY_PIO_ENV_VARS: &[&str] = &[
    "PLATFORMIO_AUTH_TOKEN",
    "PLATFORMIO_FORCE_ANSI",
    "PLATFORMIO_NO_ANSI",
    "PLATFORMIO_DISABLE_PROGRESSBAR",
    "PLATFORMIO_SYSTEM_TYPE",
    "PLATFORMIO_EXTRA_SCRIPTS",
    "PLATFORMIO_REMOTE_AGENT_DIR",
    "PLATFORMIO_MONITOR_DIR",
    "PLATFORMIO_SHARED_DIR",
    // Deprecated alias for PLATFORMIO_CORE_DIR — recognized so the scanner
    // doesn't flag it as unknown, but get_core_dir() also accepts it as a
    // fallback with a deprecation warning emitted at the read site.
    "PLATFORMIO_HOME",
];

/// Per-request snapshot of `PLATFORMIO_*` env vars.
///
/// Construct via `from_map` (typically from a deserialized HTTP request body)
/// or `empty()` for tests/non-CLI callers. Use the typed accessors to read
/// known vars; an empty string is treated as unset.
#[derive(Debug, Clone, Default)]
pub struct PioEnvOverrides {
    map: BTreeMap<String, String>,
}

impl PioEnvOverrides {
    /// Empty overrides — all accessors return `None`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Wrap a map of env var name → value.
    pub fn from_map(map: BTreeMap<String, String>) -> Self {
        Self { map }
    }

    /// Borrow the underlying map.
    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.map
    }

    /// Consume into the underlying map.
    pub fn into_map(self) -> BTreeMap<String, String> {
        self.map
    }

    /// True if no overrides are set.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Get a raw env var value, treating empty strings as unset.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    // ---- Phase 1 typed accessors ----

    pub fn get_src_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_SRC_DIR")
    }
    pub fn get_build_flags(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_FLAGS")
    }
    pub fn get_build_src_flags(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_SRC_FLAGS")
    }
    pub fn get_build_unflags(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_UNFLAGS")
    }
    pub fn get_build_src_filter(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_SRC_FILTER")
    }
    pub fn get_default_envs(&self) -> Option<&str> {
        self.get("PLATFORMIO_DEFAULT_ENVS")
    }
    pub fn get_include_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_INCLUDE_DIR")
    }
    pub fn get_lib_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_LIB_DIR")
    }
    pub fn get_lib_extra_dirs(&self) -> Option<&str> {
        self.get("PLATFORMIO_LIB_EXTRA_DIRS")
    }
    pub fn get_upload_port(&self) -> Option<&str> {
        self.get("PLATFORMIO_UPLOAD_PORT")
    }
    pub fn get_upload_flags(&self) -> Option<&str> {
        self.get("PLATFORMIO_UPLOAD_FLAGS")
    }
    pub fn get_run_jobs(&self) -> Option<usize> {
        self.get("PLATFORMIO_RUN_JOBS").and_then(|s| s.parse().ok())
    }

    // ---- Phase 2 directory accessors ----

    /// `PLATFORMIO_CORE_DIR`, falling back to deprecated `PLATFORMIO_HOME`.
    pub fn get_core_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_CORE_DIR")
            .or_else(|| self.get("PLATFORMIO_HOME"))
    }
    pub fn get_workspace_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_WORKSPACE_DIR")
    }
    pub fn get_build_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_DIR")
    }
    pub fn get_libdeps_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_LIBDEPS_DIR")
    }
    pub fn get_packages_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_PACKAGES_DIR")
    }
    pub fn get_platforms_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_PLATFORMS_DIR")
    }
    pub fn get_boards_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_BOARDS_DIR")
    }
    pub fn get_cache_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_CACHE_DIR")
    }
    pub fn get_build_cache_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_BUILD_CACHE_DIR")
    }
    pub fn get_data_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_DATA_DIR")
    }
    pub fn get_test_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_TEST_DIR")
    }
    pub fn get_globallib_dir(&self) -> Option<&str> {
        self.get("PLATFORMIO_GLOBALLIB_DIR")
    }
}

/// Return names of `PLATFORMIO_*` keys in `map` that are neither supported nor
/// recognized as warn-only. These trigger an "unsupported and ignored" warning.
pub fn scan_unsupported(map: &BTreeMap<String, String>) -> Vec<String> {
    let known: HashSet<&str> = SUPPORTED_PIO_ENV_VARS
        .iter()
        .chain(WARN_ONLY_PIO_ENV_VARS.iter())
        .copied()
        .collect();
    map.keys()
        .filter(|k| k.starts_with("PLATFORMIO_") && !known.contains(k.as_str()))
        .cloned()
        .collect()
}

/// Return names of warn-only `PLATFORMIO_*` keys in `map`. These trigger a
/// "recognized but no-op" warning.
pub fn scan_warn_only(map: &BTreeMap<String, String>) -> Vec<String> {
    let warn: HashSet<&str> = WARN_ONLY_PIO_ENV_VARS.iter().copied().collect();
    map.keys()
        .filter(|k| warn.contains(k.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn empty_overrides_returns_none_for_all_accessors() {
        let o = PioEnvOverrides::empty();
        assert!(o.is_empty());
        assert_eq!(o.get_src_dir(), None);
        assert_eq!(o.get_build_flags(), None);
        assert_eq!(o.get_core_dir(), None);
        assert_eq!(o.get_run_jobs(), None);
    }

    #[test]
    fn get_treats_empty_string_as_unset() {
        let o = PioEnvOverrides::from_map(map(&[("PLATFORMIO_SRC_DIR", "")]));
        assert_eq!(o.get_src_dir(), None);
    }

    #[test]
    fn typed_accessors_return_set_value() {
        let o = PioEnvOverrides::from_map(map(&[
            ("PLATFORMIO_SRC_DIR", "/tmp/src"),
            ("PLATFORMIO_BUILD_FLAGS", "-DFOO=1 -DBAR=2"),
            ("PLATFORMIO_RUN_JOBS", "8"),
        ]));
        assert_eq!(o.get_src_dir(), Some("/tmp/src"));
        assert_eq!(o.get_build_flags(), Some("-DFOO=1 -DBAR=2"));
        assert_eq!(o.get_run_jobs(), Some(8));
    }

    #[test]
    fn run_jobs_invalid_returns_none() {
        let o = PioEnvOverrides::from_map(map(&[("PLATFORMIO_RUN_JOBS", "not-a-number")]));
        assert_eq!(o.get_run_jobs(), None);
    }

    #[test]
    fn core_dir_falls_back_to_home() {
        let o = PioEnvOverrides::from_map(map(&[("PLATFORMIO_HOME", "/opt/pio")]));
        assert_eq!(o.get_core_dir(), Some("/opt/pio"));
    }

    #[test]
    fn core_dir_prefers_core_dir_over_home() {
        let o = PioEnvOverrides::from_map(map(&[
            ("PLATFORMIO_CORE_DIR", "/opt/core"),
            ("PLATFORMIO_HOME", "/opt/home"),
        ]));
        assert_eq!(o.get_core_dir(), Some("/opt/core"));
    }

    #[test]
    fn scan_unsupported_returns_unknown_pio_keys() {
        let m = map(&[
            ("PLATFORMIO_SRC_DIR", "/tmp"),
            ("PLATFORMIO_NONSENSE", "1"),
            ("UNRELATED_VAR", "x"),
        ]);
        let unsupported = scan_unsupported(&m);
        assert_eq!(unsupported, vec!["PLATFORMIO_NONSENSE".to_string()]);
    }

    #[test]
    fn scan_unsupported_ignores_supported_keys() {
        let m = map(&[
            ("PLATFORMIO_SRC_DIR", "/tmp"),
            ("PLATFORMIO_BUILD_FLAGS", "-DFOO"),
            ("PLATFORMIO_CORE_DIR", "/opt"),
        ]);
        assert!(scan_unsupported(&m).is_empty());
    }

    #[test]
    fn scan_unsupported_ignores_warn_only_keys() {
        let m = map(&[
            ("PLATFORMIO_AUTH_TOKEN", "secret"),
            ("PLATFORMIO_HOME", "/opt"),
        ]);
        assert!(scan_unsupported(&m).is_empty());
    }

    #[test]
    fn scan_warn_only_returns_recognized_no_op_keys() {
        let m = map(&[
            ("PLATFORMIO_SRC_DIR", "/tmp"),
            ("PLATFORMIO_AUTH_TOKEN", "secret"),
            ("PLATFORMIO_HOME", "/opt"),
        ]);
        let mut warn = scan_warn_only(&m);
        warn.sort();
        assert_eq!(
            warn,
            vec![
                "PLATFORMIO_AUTH_TOKEN".to_string(),
                "PLATFORMIO_HOME".to_string(),
            ]
        );
    }

    #[test]
    fn scan_warn_only_returns_empty_when_no_warn_keys_set() {
        let m = map(&[("PLATFORMIO_SRC_DIR", "/tmp")]);
        assert!(scan_warn_only(&m).is_empty());
    }
}
