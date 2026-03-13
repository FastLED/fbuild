//! PlatformIO INI parser with environment inheritance and variable substitution.
//!
//! Features:
//! - Sections: `[env:name]`, `[platformio]`, custom sections
//! - `extends` directive for environment inheritance
//! - Variable substitution: `${section.key}` or `${env:section.key}`
//! - Multi-line values (continuation lines starting with whitespace)
//! - Inline comments (` ; comment` or ` # comment`)
//! - Base `[env]` section merged into all `[env:*]` sections

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Parsed platformio.ini configuration.
pub struct PlatformIOConfig {
    /// Raw sections: section_name -> key -> value
    sections: HashMap<String, HashMap<String, String>>,
    /// Resolved environment configs (with inheritance applied)
    resolved_envs: HashMap<String, HashMap<String, String>>,
    /// Path to the platformio.ini file
    path: PathBuf,
}

impl PlatformIOConfig {
    /// Parse a platformio.ini file.
    pub fn from_path(path: &Path) -> fbuild_core::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            fbuild_core::FbuildError::ConfigError(format!(
                "failed to read {}: {}",
                path.display(),
                e
            ))
        })?;

        let sections = Self::parse_ini(&content)?;
        let resolved_envs = Self::resolve_all_envs(&sections)?;

        Ok(Self {
            sections,
            resolved_envs,
            path: path.to_path_buf(),
        })
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
    /// 1. `[platformio]` section `default_envs` key (first value)
    /// 2. First environment in file order
    /// 3. None if no environments
    pub fn get_default_environment(&self) -> Option<&str> {
        // Check [platformio].default_envs
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
        let config = self.get_env_config(env_name)?;
        match config.get("build_flags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(Vec::new()),
        }
    }

    /// Get build_src_flags for an environment (sketch-only flags).
    pub fn get_build_src_flags(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("build_src_flags") {
            Some(flags) => Ok(parse_flags(flags)),
            None => Ok(Vec::new()),
        }
    }

    /// Get library dependencies for an environment.
    pub fn get_lib_deps(&self, env_name: &str) -> fbuild_core::Result<Vec<String>> {
        let config = self.get_env_config(env_name)?;
        match config.get("lib_deps") {
            Some(deps) => Ok(parse_lib_deps(deps)),
            None => Ok(Vec::new()),
        }
    }

    /// Get src_dir setting, checking env var override and ini config.
    pub fn get_src_dir(&self, env_name: &str) -> fbuild_core::Result<Option<String>> {
        // PLATFORMIO_SRC_DIR env var takes precedence
        if let Ok(env_val) = std::env::var("PLATFORMIO_SRC_DIR") {
            if !env_val.is_empty() {
                return Ok(Some(env_val));
            }
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
            }
        }

        Ok(overrides)
    }

    /// Get the file path this config was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // --- Private implementation ---

    /// Parse raw INI content into sections.
    fn parse_ini(content: &str) -> fbuild_core::Result<HashMap<String, HashMap<String, String>>> {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current_section: Option<String> = None;
        let mut current_key: Option<String> = None;
        let mut current_value = String::new();

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip empty lines and pure comment lines
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                // If we're accumulating a multi-line value and hit a blank line,
                // that ends the multi-line value
                if trimmed.is_empty() && current_key.is_some() {
                    if let (Some(ref section), Some(ref key)) = (&current_section, &current_key) {
                        let section_map = sections.entry(section.clone()).or_default();
                        section_map.insert(key.clone(), current_value.trim().to_string());
                        current_key = None;
                        current_value.clear();
                    }
                }
                continue;
            }

            // Section header: [section_name]
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                // Flush previous key-value
                if let (Some(ref section), Some(ref key)) = (&current_section, &current_key) {
                    let section_map = sections.entry(section.clone()).or_default();
                    section_map.insert(key.clone(), current_value.trim().to_string());
                    current_key = None;
                    current_value.clear();
                }

                let name = trimmed[1..trimmed.len() - 1].trim().to_string();
                current_section = Some(name);
                continue;
            }

            // Continuation line (starts with whitespace) — append to current value
            if (line.starts_with(' ') || line.starts_with('\t')) && current_key.is_some() {
                let val = strip_inline_comment(trimmed);
                if !current_value.is_empty() {
                    current_value.push('\n');
                }
                current_value.push_str(&val);
                continue;
            }

            // Key = value line
            if let Some(eq_pos) = trimmed.find('=') {
                // Flush previous key-value
                if let (Some(ref section), Some(ref key)) = (&current_section, &current_key) {
                    let section_map = sections.entry(section.clone()).or_default();
                    section_map.insert(key.clone(), current_value.trim().to_string());
                }

                let key = trimmed[..eq_pos].trim().to_string();
                let value = strip_inline_comment(trimmed[eq_pos + 1..].trim());

                current_key = Some(key);
                current_value = value;
            }
        }

        // Flush last key-value
        if let (Some(ref section), Some(ref key)) = (&current_section, &current_key) {
            let section_map = sections.entry(section.clone()).or_default();
            section_map.insert(key.clone(), current_value.trim().to_string());
        }

        Ok(sections)
    }

    /// Resolve all `[env:*]` sections with inheritance.
    fn resolve_all_envs(
        sections: &HashMap<String, HashMap<String, String>>,
    ) -> fbuild_core::Result<HashMap<String, HashMap<String, String>>> {
        let mut resolved = HashMap::new();

        // Find all env:* sections
        let env_names: Vec<String> = sections
            .keys()
            .filter_map(|k| k.strip_prefix("env:").map(|s| s.to_string()))
            .collect();

        for env_name in &env_names {
            let config = Self::resolve_env(sections, env_name, &mut HashSet::new())?;
            // Apply variable substitution
            let substituted = Self::substitute_vars(sections, &config);
            resolved.insert(env_name.clone(), substituted);
        }

        Ok(resolved)
    }

    /// Resolve a single environment's config, following `extends` chains.
    fn resolve_env(
        sections: &HashMap<String, HashMap<String, String>>,
        env_name: &str,
        visited: &mut HashSet<String>,
    ) -> fbuild_core::Result<HashMap<String, String>> {
        if !visited.insert(env_name.to_string()) {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "circular extends dependency for env:{}",
                env_name
            )));
        }

        let section_key = format!("env:{}", env_name);
        let section = sections.get(&section_key).ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(format!(
                "environment '{}' not found in config",
                env_name
            ))
        })?;

        // Start with base [env] section if it exists
        let mut config: HashMap<String, String> = sections.get("env").cloned().unwrap_or_default();

        // Apply extends (parent values, then child overrides)
        if let Some(extends) = section.get("extends") {
            for parent_ref in extends.split(',') {
                let parent_ref = parent_ref.trim();
                // extends can reference "env:name" or just "name" (treated as section name)
                let parent_name = parent_ref.strip_prefix("env:").unwrap_or(parent_ref);

                if sections.contains_key(&format!("env:{}", parent_name)) {
                    let parent_config = Self::resolve_env(sections, parent_name, visited)?;
                    // Parent values go first, child overrides later
                    for (k, v) in parent_config {
                        config.insert(k, v);
                    }
                } else if let Some(parent_section) = sections.get(parent_name) {
                    // Direct section reference (non-env section)
                    for (k, v) in parent_section {
                        config.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Apply this environment's values (override parents)
        for (k, v) in section {
            if k != "extends" {
                config.insert(k.clone(), v.clone());
            }
        }

        Ok(config)
    }

    /// Apply `${section.key}` variable substitution.
    fn substitute_vars(
        sections: &HashMap<String, HashMap<String, String>>,
        config: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let re = Regex::new(r"\$\{([^}]+)\}").unwrap();
        let max_depth = 10;
        let mut result = config.clone();

        for (_key, value) in result.iter_mut() {
            let mut current = value.clone();
            for _ in 0..max_depth {
                let next = re
                    .replace_all(&current, |caps: &regex::Captures| {
                        let reference = &caps[1];
                        resolve_variable(sections, config, reference)
                    })
                    .to_string();

                if next == current {
                    break;
                }
                current = next;
            }
            *value = current;
        }

        result
    }
}

/// Resolve a variable reference like "section.key" or "env:name.key".
fn resolve_variable(
    sections: &HashMap<String, HashMap<String, String>>,
    current_config: &HashMap<String, String>,
    reference: &str,
) -> String {
    // Try "section.key" format
    if let Some(dot_pos) = reference.find('.') {
        let section_ref = &reference[..dot_pos];
        let key = &reference[dot_pos + 1..];

        // Try env:name format
        let section_name = if section_ref.starts_with("env:") {
            section_ref.to_string()
        } else {
            // Could be env:name or just a section name
            let as_env = format!("env:{}", section_ref);
            if sections.contains_key(&as_env) {
                as_env
            } else {
                section_ref.to_string()
            }
        };

        if let Some(section) = sections.get(&section_name) {
            if let Some(val) = section.get(key) {
                return val.clone();
            }
        }
    }

    // Try as a key in the current config
    if let Some(val) = current_config.get(reference) {
        return val.clone();
    }

    // Unresolved — return as-is
    format!("${{{}}}", reference)
}

/// Strip inline comments (` ; comment` or ` # comment`).
/// Be careful not to strip hash/semicolons that are part of values.
fn strip_inline_comment(s: &str) -> String {
    // Only strip comments that are preceded by whitespace
    // This avoids stripping "#include" or URLs with "#"
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i - 1] == b' ' && (bytes[i] == b';' || bytes[i] == b'#') {
            return s[..i].trim().to_string();
        }
    }
    s.trim().to_string()
}

/// Parse build flags string into a list.
///
/// Handles:
/// - Space-separated flags: `-DFOO -DBAR`
/// - Multi-line: one flag per line
/// - `-D FLAG` → `-DFLAG` normalization
/// - Preserves arguments for `-include`, `-I`, `-L`, etc.
fn parse_flags(flags_str: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in flags_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let chars = trimmed.chars();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut quote_char = ' ';

        for c in chars {
            match c {
                '"' | '\'' if !in_quotes => {
                    in_quotes = true;
                    quote_char = c;
                    current.push(c);
                }
                c if in_quotes && c == quote_char => {
                    in_quotes = false;
                    current.push(c);
                }
                ' ' | '\t' if !in_quotes => {
                    if !current.is_empty() {
                        result.push(current.clone());
                        current.clear();
                    }
                }
                _ => {
                    current.push(c);
                }
            }
        }

        if !current.is_empty() {
            result.push(current);
        }
    }

    // Normalize `-D FLAG` → `-DFLAG`
    let mut normalized = Vec::new();
    let mut i = 0;
    while i < result.len() {
        if result[i] == "-D" && i + 1 < result.len() {
            normalized.push(format!("-D{}", result[i + 1]));
            i += 2;
        } else {
            normalized.push(result[i].clone());
            i += 1;
        }
    }

    normalized
}

/// Parse library dependencies from a multi-line or comma-separated string.
fn parse_lib_deps(deps_str: &str) -> Vec<String> {
    let mut result = Vec::new();

    for line in deps_str.lines() {
        for dep in line.split(',') {
            let trimmed = dep.trim();
            if !trimmed.is_empty() {
                result.push(trimmed.to_string());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_ini(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_init_with_valid_file() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(config.get_environments(), vec!["uno"]);
    }

    #[test]
    fn test_init_with_nonexistent_file() {
        let result = PlatformIOConfig::from_path(Path::new("/nonexistent/platformio.ini"));
        assert!(result.is_err());
    }

    #[test]
    fn test_get_environments_multiple() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:esp32]
platform = espressif32
board = esp32dev
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let envs = config.get_environments();
        assert_eq!(envs.len(), 2);
        assert!(envs.contains(&"uno"));
        assert!(envs.contains(&"esp32"));
    }

    #[test]
    fn test_get_environments_empty() {
        let f = write_ini("[platformio]\ndefault_envs = \n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert!(config.get_environments().is_empty());
    }

    #[test]
    fn test_get_env_config_valid() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let env = config.get_env_config("uno").unwrap();
        assert_eq!(env.get("platform").unwrap(), "atmelavr");
        assert_eq!(env.get("board").unwrap(), "uno");
    }

    #[test]
    fn test_get_env_config_nonexistent() {
        let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert!(config.get_env_config("nonexistent").is_err());
    }

    #[test]
    fn test_get_env_config_with_base_env_inheritance() {
        let f = write_ini(
            "\
[env]
framework = arduino

[env:uno]
platform = atmelavr
board = uno
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let env = config.get_env_config("uno").unwrap();
        assert_eq!(env.get("framework").unwrap(), "arduino");
        assert_eq!(env.get("platform").unwrap(), "atmelavr");
    }

    #[test]
    fn test_get_build_flags_present() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -DFOO -DBAR
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let flags = config.get_build_flags("uno").unwrap();
        assert_eq!(flags, vec!["-DFOO", "-DBAR"]);
    }

    #[test]
    fn test_get_build_flags_absent() {
        let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let flags = config.get_build_flags("uno").unwrap();
        assert!(flags.is_empty());
    }

    #[test]
    fn test_get_build_flags_multiline() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags =
    -DFOO
    -DBAR
    -DBAZ
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let flags = config.get_build_flags("uno").unwrap();
        assert_eq!(flags, vec!["-DFOO", "-DBAR", "-DBAZ"]);
    }

    #[test]
    fn test_get_build_flags_d_space_normalization() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
build_flags = -D FOO -D BAR
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let flags = config.get_build_flags("uno").unwrap();
        assert_eq!(flags, vec!["-DFOO", "-DBAR"]);
    }

    #[test]
    fn test_get_lib_deps_present() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    FastLED
    ArduinoJson
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let deps = config.get_lib_deps("uno").unwrap();
        assert_eq!(deps, vec!["FastLED", "ArduinoJson"]);
    }

    #[test]
    fn test_get_lib_deps_comma_separated() {
        let f = write_ini(
            "\
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps = FastLED, ArduinoJson
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let deps = config.get_lib_deps("uno").unwrap();
        assert_eq!(deps, vec!["FastLED", "ArduinoJson"]);
    }

    #[test]
    fn test_get_lib_deps_absent() {
        let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let deps = config.get_lib_deps("uno").unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_has_environment() {
        let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert!(config.has_environment("uno"));
        assert!(!config.has_environment("esp32"));
    }

    #[test]
    fn test_get_default_environment_explicit() {
        let f = write_ini(
            "\
[platformio]
default_envs = esp32

[env:uno]
platform = atmelavr
board = uno
framework = arduino

[env:esp32]
platform = espressif32
board = esp32dev
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(config.get_default_environment(), Some("esp32"));
    }

    #[test]
    fn test_get_default_environment_first_fallback() {
        let f = write_ini(
            "\
[env:alpha]
platform = atmelavr
board = uno
framework = arduino

[env:beta]
platform = espressif32
board = esp32dev
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        // Should return first alphabetically
        assert_eq!(config.get_default_environment(), Some("alpha"));
    }

    #[test]
    fn test_get_default_environment_none() {
        let f = write_ini("[platformio]\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(config.get_default_environment(), None);
    }

    #[test]
    fn test_extends_inheritance() {
        let f = write_ini(
            "\
[env:base]
platform = atmelavr
framework = arduino
build_flags = -DBASE

[env:child]
extends = env:base
board = uno
build_flags = -DCHILD
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        let env = config.get_env_config("child").unwrap();
        assert_eq!(env.get("platform").unwrap(), "atmelavr");
        assert_eq!(env.get("framework").unwrap(), "arduino");
        assert_eq!(env.get("board").unwrap(), "uno");
        // Child overrides parent's build_flags
        assert_eq!(env.get("build_flags").unwrap(), "-DCHILD");
    }

    #[test]
    fn test_get_src_dir_from_ini() {
        let f = write_ini(
            "\
[platformio]
src_dir = custom_src

[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(
            config.get_src_dir("uno").unwrap(),
            Some("custom_src".to_string())
        );
    }

    #[test]
    fn test_get_src_dir_returns_none_when_not_set() {
        let f = write_ini("[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n");
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(config.get_src_dir("uno").unwrap(), None);
    }

    #[test]
    fn test_get_src_dir_with_inline_comment() {
        let f = write_ini(
            "\
[platformio]
src_dir = custom_src ; this is the source dir

[env:uno]
platform = atmelavr
board = uno
framework = arduino
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();
        assert_eq!(
            config.get_src_dir("uno").unwrap(),
            Some("custom_src".to_string())
        );
    }

    #[test]
    fn test_real_world_config() {
        let f = write_ini(
            "\
[platformio]
default_envs = esp32dev

[env]
framework = arduino

[env:esp32dev]
platform = espressif32
board = esp32dev
build_flags =
    -DFASTLED_ESP32
    -DCORE_DEBUG_LEVEL=0
lib_deps =
    FastLED
    ArduinoJson

[env:uno]
platform = atmelavr
board = uno
build_flags = -DFASTLED_AVR
",
        );
        let config = PlatformIOConfig::from_path(f.path()).unwrap();

        // Default env
        assert_eq!(config.get_default_environment(), Some("esp32dev"));

        // ESP32 config
        let esp = config.get_env_config("esp32dev").unwrap();
        assert_eq!(esp.get("platform").unwrap(), "espressif32");
        assert_eq!(esp.get("framework").unwrap(), "arduino"); // inherited from [env]

        let esp_flags = config.get_build_flags("esp32dev").unwrap();
        assert_eq!(esp_flags, vec!["-DFASTLED_ESP32", "-DCORE_DEBUG_LEVEL=0"]);

        let esp_deps = config.get_lib_deps("esp32dev").unwrap();
        assert_eq!(esp_deps, vec!["FastLED", "ArduinoJson"]);

        // Uno config
        let uno = config.get_env_config("uno").unwrap();
        assert_eq!(uno.get("platform").unwrap(), "atmelavr");
        assert_eq!(uno.get("framework").unwrap(), "arduino"); // inherited from [env]

        let uno_flags = config.get_build_flags("uno").unwrap();
        assert_eq!(uno_flags, vec!["-DFASTLED_AVR"]);
    }

    #[test]
    fn test_strip_inline_comment() {
        assert_eq!(strip_inline_comment("value ; comment"), "value");
        assert_eq!(strip_inline_comment("value # comment"), "value");
        assert_eq!(strip_inline_comment("value"), "value");
        assert_eq!(strip_inline_comment("#include <foo>"), "#include <foo>");
    }

    #[test]
    fn test_parse_flags() {
        assert_eq!(parse_flags("-DFOO -DBAR"), vec!["-DFOO", "-DBAR"]);
        assert_eq!(parse_flags("-D FOO -D BAR"), vec!["-DFOO", "-DBAR"]);
        assert_eq!(
            parse_flags("-DFOO\n-DBAR\n-DBAZ"),
            vec!["-DFOO", "-DBAR", "-DBAZ"]
        );
    }

    #[test]
    fn test_parse_lib_deps() {
        assert_eq!(
            parse_lib_deps("FastLED, ArduinoJson"),
            vec!["FastLED", "ArduinoJson"]
        );
        assert_eq!(
            parse_lib_deps("FastLED\nArduinoJson"),
            vec!["FastLED", "ArduinoJson"]
        );
    }
}
