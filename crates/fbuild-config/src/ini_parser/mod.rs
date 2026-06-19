//! PlatformIO INI parser with environment inheritance and variable substitution.
//!
//! Features:
//! - Sections: `[env:name]`, `[platformio]`, custom sections
//! - `extends` directive for environment inheritance
//! - Variable substitution: `${section.key}` or `${env:section.key}`
//! - Multi-line values (continuation lines starting with whitespace)
//! - Inline comments (` ; comment` or ` # comment`)
//! - Base `[env]` section merged into all `[env:*]` sections

mod parser;
#[cfg(test)]
mod tests;
mod values;
mod variables;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::pio_env::PioEnvOverrides;

use self::parser::{parse_ini, resolve_all_envs};
use self::values::{
    parse_flags, parse_lib_deps, parse_list_values, parse_path_list, strip_inline_comment,
};

/// Parsed platformio.ini configuration.
pub struct PlatformIOConfig {
    /// Raw sections: section_name -> key -> value
    sections: HashMap<String, HashMap<String, String>>,
    /// Resolved environment configs (with inheritance applied)
    resolved_envs: HashMap<String, HashMap<String, String>>,
    /// Path to the platformio.ini file
    path: PathBuf,
    /// Per-request `PLATFORMIO_*` env var overrides forwarded from the caller.
    ///
    /// Used by getters to honor env-driven overrides without reading
    /// `std::env::var` directly. The daemon does not inherit caller env vars,
    /// so all `PLATFORMIO_*` config must flow through this struct.
    overrides: PioEnvOverrides,
}

impl PlatformIOConfig {
    /// Parse a platformio.ini file with no env var overrides.
    ///
    /// Equivalent to `from_path_with_overrides(path, PioEnvOverrides::empty())`.
    pub fn from_path(path: &Path) -> fbuild_core::Result<Self> {
        Self::from_path_with_overrides(path, PioEnvOverrides::empty())
    }

    /// Parse a platformio.ini file and attach `PLATFORMIO_*` env var overrides.
    ///
    /// The overrides are consulted by getters before falling back to INI values,
    /// allowing CLI callers to forward env vars to the daemon over HTTP without
    /// the daemon process needing to inherit them.
    pub fn from_path_with_overrides(
        path: &Path,
        overrides: PioEnvOverrides,
    ) -> fbuild_core::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            fbuild_core::FbuildError::ConfigError(format!(
                "failed to read {}: {}",
                path.display(),
                e
            ))
        })?;

        let sections = parse_ini(&content)?;
        let resolved_envs = resolve_all_envs(&sections)?;

        Ok(Self {
            sections,
            resolved_envs,
            path: path.to_path_buf(),
            overrides,
        })
    }

    /// Borrow the env var overrides attached to this config.
    pub fn overrides(&self) -> &PioEnvOverrides {
        &self.overrides
    }

    /// List all environment names (from `[env:name]` sections).
    pub fn get_environments(&self) -> Vec<&str> {
        let mut envs: Vec<&str> = self.resolved_envs.keys().map(|s| s.as_str()).collect();
        envs.sort();
        envs
    }

    /// Check if an environment exists.
    pub fn has_environment(&self, env_name: &str) -> bool {
        self.resolved_envs.contains_key(env_name)
    }

    /// Get resolved config for an environment (with inheritance applied).
    pub fn get_env_config(&self, env_name: &str) -> fbuild_core::Result<&HashMap<String, String>> {
        self.resolved_envs.get(env_name).ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(format!("environment '{}' not found", env_name))
        })
    }

    /// Get the default environment name.
    ///
    /// Priority:
    /// 1. forwarded `PLATFORMIO_DEFAULT_ENVS` override (first value)
    /// 2. `[platformio]` section `default_envs` key (first value)
    /// 3. First environment in file order
    /// 4. None if no environments
    pub fn get_default_environment(&self) -> Option<&str> {
        // Check forwarded PLATFORMIO_DEFAULT_ENVS first.
        if let Some(defaults) = self.overrides.get_default_envs() {
            let first = defaults.split(',').next().unwrap_or("").trim();
            if !first.is_empty() && self.resolved_envs.contains_key(first) {
                return Some(
                    self.resolved_envs
                        .keys()
                        .find(|k| k.as_str() == first)
                        .map(|k| k.as_str())
                        .unwrap(),
                );
            }
        }

        // Fall back to [platformio].default_envs.
        if let Some(pio) = self.sections.get("platformio") {
            if let Some(defaults) = pio.get("default_envs") {
                let first = defaults.split(',').next().unwrap_or("").trim();
                if !first.is_empty() && self.resolved_envs.contains_key(first) {
                    return Some(
                        self.resolved_envs
                            .keys()
                            .find(|k| k.as_str() == first)
                            .map(|k| k.as_str())
                            .unwrap(),
                    );
                }
            }
        }
        // Fall back to first environment
        let mut envs: Vec<&str> = self.resolved_envs.keys().map(|s| s.as_str()).collect();
        envs.sort();
        envs.into_iter().next()
    }

    /// Get build flags for an environment, parsed into a list.
    ///
    /// Handles:
    /// - Space-separated flags on one line
    /// - Multi-line flags (one per line)
    /// - `-D FLAG` → `-DFLAG` normalization
    pub fn get_build_flags(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        if let Some(flags) = self.overrides.get_build_flags() {
            return Ok(parse_flags(flags));
        }

        let config = self.get_env_config(env_name)?;
        match config.get("build_flags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(Vec::new()),
        }
    }

    /// Get build_src_flags for an environment (sketch-only flags).
    pub fn get_build_src_flags(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        if let Some(flags) = self.overrides.get_build_src_flags() {
            return Ok(parse_flags(flags));
        }

        let config = self.get_env_config(env_name)?;
        match config.get("build_src_flags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(Vec::new()),
        }
    }

    /// Get build_unflags for an environment.
    ///
    /// Priority:
    /// 1. `PLATFORMIO_BUILD_UNFLAGS` forwarded override
    /// 2. `build_unflags`
    pub fn get_build_unflags(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        if let Some(flags) = self.overrides.get_build_unflags() {
            return Ok(parse_flags(flags));
        }

        let config = self.get_env_config(env_name)?;
        match config.get("build_unflags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(Vec::new()),
        }
    }

    /// Get build_type for an environment.
    ///
    /// PlatformIO defaults this to `release`.
    pub fn get_build_type(&self, env_name: &str) -> fbuild_core::Result<String> {
        let config = self.get_env_config(env_name)?;
        Ok(config
            .get("build_type")
            .map(|value| strip_inline_comment(value))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "release".to_string()))
    }

    /// Get debug_build_flags for an environment.
    ///
    /// PlatformIO defaults to `-Og -g2 -ggdb2` when the option is not set.
    pub fn get_debug_build_flags(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("debug_build_flags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(vec![
                "-Og".to_string(),
                "-g2".to_string(),
                "-ggdb2".to_string(),
            ]),
        }
    }

    /// Get source filter rules for an environment.
    ///
    /// Priority:
    /// 1. `PLATFORMIO_BUILD_SRC_FILTER` forwarded override
    /// 2. `build_src_filter`
    /// 3. legacy `src_filter`
    pub fn get_source_filter(&self, env_name: &str) -> fbuild_core::Result<Option<String>> {
        if let Some(value) = self.overrides.get_build_src_filter() {
            let cleaned = strip_inline_comment(value);
            if !cleaned.is_empty() {
                return Ok(Some(cleaned.to_string()));
            }
        }

        let config = self.get_env_config(env_name)?;
        for key in ["build_src_filter", "src_filter"] {
            if let Some(value) = config.get(key) {
                let cleaned = strip_inline_comment(value);
                if !cleaned.is_empty() {
                    return Ok(Some(cleaned.to_string()));
                }
            }
        }

        Ok(None)
    }

    /// Get library dependencies for an environment.
    pub fn get_lib_deps(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("lib_deps") {
            Some(deps) => Ok(parse_lib_deps(deps)),
            None => Ok(Vec::new()),
        }
    }

    /// Get extra library search directories for an environment.
    pub fn get_lib_extra_dirs(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        if let Some(dirs) = self.overrides.get_lib_extra_dirs() {
            return Ok(parse_path_list(dirs));
        }

        let config = self.get_env_config(env_name)?;
        match config.get("lib_extra_dirs") {
            Some(dirs) => Ok(parse_list_values(dirs)),
            None => Ok(Vec::new()),
        }
    }

    /// Get lib_ignore for an environment (libraries to skip).
    pub fn get_lib_ignore(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("lib_ignore") {
            Some(deps) => Ok(parse_lib_deps(deps)),
            None => Ok(Vec::new()),
        }
    }

    /// Get `extra_scripts` for an environment.
    ///
    /// PlatformIO treats entries without an explicit prefix as POST scripts.
    /// Values may be provided comma-separated or as a multi-line list.
    pub fn get_extra_scripts(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("extra_scripts") {
            Some(scripts) => Ok(parse_list_values(scripts)),
            None => Ok(Vec::new()),
        }
    }

    /// Get `board_build.embed_files` for an environment (binary files to embed).
    pub fn get_embed_files(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let overrides = self.get_board_overrides(env_name)?;
        match overrides.get("embed_files") {
            Some(files) => Ok(parse_lib_deps(files)),
            None => Ok(Vec::new()),
        }
    }

    /// Get `board_build.embed_txtfiles` for an environment (text files to embed with null terminator).
    pub fn get_embed_txtfiles(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let overrides = self.get_board_overrides(env_name)?;
        match overrides.get("embed_txtfiles") {
            Some(files) => Ok(parse_lib_deps(files)),
            None => Ok(Vec::new()),
        }
    }

    /// Get src_dir setting, checking forwarded override and ini config.
    pub fn get_src_dir(&self, env_name: &str) -> fbuild_core::Result<Option<String>> {
        // Forwarded PLATFORMIO_SRC_DIR takes precedence.
        if let Some(env_val) = self.overrides.get_src_dir() {
            return Ok(Some(env_val.to_string()));
        }

        let config = self.get_env_config(env_name)?;
        if let Some(src_dir) = config.get("src_dir") {
            // Strip inline comments
            let cleaned = strip_inline_comment(src_dir);
            if !cleaned.is_empty() {
                return Ok(Some(cleaned));
            }
        }

        // Check [platformio] section
        if let Some(pio) = self.sections.get("platformio") {
            if let Some(src_dir) = pio.get("src_dir") {
                let cleaned = strip_inline_comment(src_dir);
                if !cleaned.is_empty() {
                    return Ok(Some(cleaned));
                }
            }
        }

        Ok(None)
    }

    /// Get board_build.* and board_upload.* overrides from the environment config.
    pub fn get_board_overrides(
        &self,
        env_name: &str,
    ) -> fbuild_core::Result<HashMap<String, String>> {
        let config = self.get_env_config(env_name)?;
        let mut overrides = HashMap::new();

        for (key, value) in config {
            if let Some(stripped) = key.strip_prefix("board_build.") {
                overrides.insert(stripped.to_string(), value.clone());
            } else if let Some(stripped) = key.strip_prefix("board_upload.") {
                overrides.insert(format!("upload.{}", stripped), value.clone());
            } else if ["monitor_filters", "check_tool"].contains(&key.as_str()) {
                overrides.insert(key.clone(), value.clone());
            }
        }

        Ok(overrides)
    }

    /// Get the file path this config was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
