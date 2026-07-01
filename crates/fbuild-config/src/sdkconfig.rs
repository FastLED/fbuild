//! ESP-IDF `sdkconfig` / `sdkconfig.defaults` parser surfacing the keys
//! fbuild's policy code reads. Not a general-purpose sdkconfig parser —
//! we only need a fixed list of boolean keys.
//!
//! See FastLED/fbuild#243 (ESP32 eh_frame strip decision needs these).

use std::path::Path;

/// Summary of sdkconfig keys consumed by build-time policies.
///
/// Defaults match ESP-IDF Arduino's defaults: panic backtrace is ON,
/// gdbstub is OFF, ocdaware is OFF, optimization debug is OFF. So
/// `SdkConfigSummary::default() == arduino_default()` and represents the
/// safe assumption for a project that hasn't customized its sdkconfig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkConfigSummary {
    /// `CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE=y`
    pub panic_print_backtrace: bool,
    /// `CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y`
    pub panic_gdbstub: bool,
    /// `CONFIG_ESP_DEBUG_OCDAWARE=y`
    pub debug_ocdaware: bool,
    /// `CONFIG_COMPILER_OPTIMIZATION_DEBUG=y`
    pub optimization_debug: bool,
}

impl Default for SdkConfigSummary {
    fn default() -> Self {
        Self::arduino_default()
    }
}

impl SdkConfigSummary {
    /// ESP-IDF Arduino defaults (`panic_print_backtrace = true`, others false).
    pub fn arduino_default() -> Self {
        Self {
            panic_print_backtrace: true,
            panic_gdbstub: false,
            debug_ocdaware: false,
            optimization_debug: false,
        }
    }

    /// Parse a single sdkconfig file's contents. Recognized line forms:
    /// - `CONFIG_FOO=y` → true
    /// - `CONFIG_FOO=n` → false (rare; usually `# CONFIG_FOO is not set`)
    /// - `# CONFIG_FOO is not set` → false
    /// - other forms (`CONFIG_FOO=123`, `CONFIG_FOO="bar"`, blank lines,
    ///   comments) → ignored
    ///
    /// Keys not present default to the same values as `arduino_default()`.
    pub fn parse(content: &str) -> Self {
        let mut summary = Self::arduino_default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Handle "# CONFIG_FOO is not set" form
            if let Some(rest) = line.strip_prefix("# ") {
                if let Some(key) = rest.strip_suffix(" is not set") {
                    Self::apply_bool(&mut summary, key, false);
                }
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            // KEY=VALUE form
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            // Only the y/n forms matter to us.
            let bool_val = match value {
                "y" => Some(true),
                "n" => Some(false),
                _ => None,
            };
            if let Some(v) = bool_val {
                Self::apply_bool(&mut summary, key, v);
            }
        }
        summary
    }

    fn apply_bool(s: &mut Self, key: &str, value: bool) {
        match key {
            "CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE" => s.panic_print_backtrace = value,
            "CONFIG_ESP_SYSTEM_PANIC_GDBSTUB" => s.panic_gdbstub = value,
            "CONFIG_ESP_DEBUG_OCDAWARE" => s.debug_ocdaware = value,
            "CONFIG_COMPILER_OPTIMIZATION_DEBUG" => s.optimization_debug = value,
            _ => {}
        }
    }

    /// Read sdkconfig / sdkconfig.defaults from the given project dir.
    /// Precedence: `sdkconfig` (active config) wins over `sdkconfig.defaults`.
    /// If neither file exists, returns `arduino_default()`.
    pub fn from_project_dir(project_dir: &Path) -> Self {
        let active = project_dir.join("sdkconfig");
        if active.is_file() {
            if let Ok(s) = std::fs::read_to_string(&active) {
                return Self::parse(&s);
            }
        }
        let defaults = project_dir.join("sdkconfig.defaults");
        if defaults.is_file() {
            if let Ok(s) = std::fs::read_to_string(&defaults) {
                return Self::parse(&s);
            }
        }
        Self::arduino_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        TempDir::new_in(fbuild_paths::temp_subdir("fbuild-config-sdkconfig-tests")).unwrap()
    }

    #[test]
    fn arduino_default_values() {
        let d = SdkConfigSummary::arduino_default();
        assert!(d.panic_print_backtrace);
        assert!(!d.panic_gdbstub);
        assert!(!d.debug_ocdaware);
        assert!(!d.optimization_debug);
    }

    #[test]
    fn parse_empty_returns_default() {
        assert_eq!(
            SdkConfigSummary::parse(""),
            SdkConfigSummary::arduino_default()
        );
    }

    #[test]
    fn parse_panic_print_backtrace_y() {
        let s = SdkConfigSummary::parse("CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE=y\n");
        assert!(s.panic_print_backtrace);
    }

    #[test]
    fn parse_panic_print_backtrace_not_set() {
        let s = SdkConfigSummary::parse("# CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE is not set\n");
        assert!(!s.panic_print_backtrace);
    }

    #[test]
    fn parse_panic_gdbstub_y() {
        let s = SdkConfigSummary::parse("CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y\n");
        assert!(s.panic_gdbstub);
    }

    #[test]
    fn parse_debug_ocdaware_y() {
        let s = SdkConfigSummary::parse("CONFIG_ESP_DEBUG_OCDAWARE=y\n");
        assert!(s.debug_ocdaware);
    }

    #[test]
    fn parse_optimization_debug_y() {
        let s = SdkConfigSummary::parse("CONFIG_COMPILER_OPTIMIZATION_DEBUG=y\n");
        assert!(s.optimization_debug);
    }

    #[test]
    fn parse_mixed_only_flips_specified_key() {
        // Start from arduino default; enable only gdbstub.
        let s = SdkConfigSummary::parse("CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y\n");
        let baseline = SdkConfigSummary::arduino_default();
        assert_eq!(s.panic_print_backtrace, baseline.panic_print_backtrace);
        assert!(s.panic_gdbstub);
        assert_eq!(s.debug_ocdaware, baseline.debug_ocdaware);
        assert_eq!(s.optimization_debug, baseline.optimization_debug);
    }

    #[test]
    fn parse_ignores_unknown_and_non_bool_forms() {
        let content = "\
# This is a comment
# Another comment

CONFIG_FOO=123
CONFIG_BAR=\"hello\"
CONFIG_UNKNOWN_KEY=y
not_a_kv_line
";
        let s = SdkConfigSummary::parse(content);
        assert_eq!(s, SdkConfigSummary::arduino_default());
    }

    #[test]
    fn parse_multiple_keys() {
        let content = "\
CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE=y
CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y
CONFIG_ESP_DEBUG_OCDAWARE=y
CONFIG_COMPILER_OPTIMIZATION_DEBUG=y
";
        let s = SdkConfigSummary::parse(content);
        assert!(s.panic_print_backtrace);
        assert!(s.panic_gdbstub);
        assert!(s.debug_ocdaware);
        assert!(s.optimization_debug);
    }

    #[test]
    fn from_project_dir_empty_returns_default() {
        let tmp = tempdir();
        let s = SdkConfigSummary::from_project_dir(tmp.path());
        assert_eq!(s, SdkConfigSummary::arduino_default());
    }

    #[test]
    fn from_project_dir_defaults_only() {
        let tmp = tempdir();
        fs::write(
            tmp.path().join("sdkconfig.defaults"),
            "CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y\n",
        )
        .unwrap();
        let s = SdkConfigSummary::from_project_dir(tmp.path());
        assert!(s.panic_gdbstub);
    }

    #[test]
    fn from_project_dir_active_wins_over_defaults() {
        let tmp = tempdir();
        // defaults says gdbstub=y
        fs::write(
            tmp.path().join("sdkconfig.defaults"),
            "CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y\n",
        )
        .unwrap();
        // active sdkconfig overrides: ocdaware=y, gdbstub not set
        fs::write(
            tmp.path().join("sdkconfig"),
            "CONFIG_ESP_DEBUG_OCDAWARE=y\n",
        )
        .unwrap();
        let s = SdkConfigSummary::from_project_dir(tmp.path());
        assert!(s.debug_ocdaware);
        // gdbstub must NOT be set from defaults (sdkconfig wins entirely)
        assert!(!s.panic_gdbstub);
    }

    #[test]
    fn default_equals_arduino_default() {
        assert_eq!(
            SdkConfigSummary::default(),
            SdkConfigSummary::arduino_default()
        );
    }
}
