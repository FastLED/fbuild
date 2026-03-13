//! PlatformIO INI parser with environment inheritance and variable substitution.

use std::collections::HashMap;
use std::path::Path;

/// Parsed platformio.ini configuration.
pub struct PlatformIOConfig {
    environments: HashMap<String, HashMap<String, String>>,
}

impl PlatformIOConfig {
    /// Parse a platformio.ini file.
    pub fn from_path(path: &Path) -> fbuild_core::Result<Self> {
        let _content = std::fs::read_to_string(path)?;
        // TODO: implement INI parsing with inheritance
        Ok(Self {
            environments: HashMap::new(),
        })
    }

    /// List all environment names.
    pub fn get_environments(&self) -> Vec<&str> {
        self.environments.keys().map(|s| s.as_str()).collect()
    }

    /// Get resolved config for an environment (with inheritance applied).
    pub fn get_env_config(&self, env_name: &str) -> Option<&HashMap<String, String>> {
        self.environments.get(env_name)
    }
}
