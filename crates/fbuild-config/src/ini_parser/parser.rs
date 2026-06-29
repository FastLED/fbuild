//! Raw INI parsing and `[env:*]` inheritance resolution.

use std::collections::HashMap;
use std::collections::HashSet;

use regex::Regex;

use super::values::strip_inline_comment;
use super::variables::resolve_variable;

/// Parse raw INI content into sections.
pub(super) fn parse_ini(
    content: &str,
) -> fbuild_core::Result<HashMap<String, HashMap<String, String>>> {
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
pub(super) fn resolve_all_envs(
    sections: &HashMap<String, HashMap<String, String>>,
) -> fbuild_core::Result<HashMap<String, HashMap<String, String>>> {
    let mut resolved = HashMap::new();

    // Find all env:* sections
    let env_names: Vec<String> = sections
        .keys()
        .filter_map(|k| k.strip_prefix("env:").map(|s| s.to_string()))
        .collect();

    for env_name in &env_names {
        let config = resolve_env(sections, env_name, &mut HashSet::new())?;
        // Apply variable substitution
        let substituted = substitute_vars(sections, &config);
        resolved.insert(env_name.clone(), substituted);
    }

    Ok(resolved)
}

/// Resolve a single environment's config, following `extends` chains.
pub(super) fn resolve_env(
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
                let parent_config = resolve_env(sections, parent_name, visited)?;
                // Parent values go first, child overrides later
                for (k, v) in parent_config {
                    config.insert(k, v);
                }
            } else if sections.contains_key(parent_name) {
                // Direct section reference (non-env section) — resolve its
                // extends chain so inherited keys like lib_deps propagate.
                let parent_config = resolve_section(sections, parent_name, visited)?;
                for (k, v) in parent_config {
                    config.insert(k, v);
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

/// Resolve a non-env section's config, following `extends` chains.
pub(super) fn resolve_section(
    sections: &HashMap<String, HashMap<String, String>>,
    section_name: &str,
    visited: &mut HashSet<String>,
) -> fbuild_core::Result<HashMap<String, String>> {
    if !visited.insert(format!("section:{}", section_name)) {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "circular extends dependency for section '{}'",
            section_name
        )));
    }

    let section = sections.get(section_name).cloned().unwrap_or_default();
    let mut config = HashMap::new();

    // Follow extends chain
    if let Some(extends) = section.get("extends") {
        for parent_ref in extends.split(',') {
            let parent_ref = parent_ref.trim();
            if sections.contains_key(&format!("env:{}", parent_ref)) {
                let parent_config = resolve_env(sections, parent_ref, visited)?;
                for (k, v) in parent_config {
                    config.insert(k, v);
                }
            } else if sections.contains_key(parent_ref) {
                let parent_config = resolve_section(sections, parent_ref, visited)?;
                for (k, v) in parent_config {
                    config.insert(k, v);
                }
            }
        }
    }

    // Apply this section's own values (override parents)
    for (k, v) in &section {
        if k != "extends" {
            config.insert(k.clone(), v.clone());
        }
    }

    Ok(config)
}

/// Apply `${section.key}` variable substitution.
pub(super) fn substitute_vars(
    sections: &HashMap<String, HashMap<String, String>>,
    config: &HashMap<String, String>,
) -> HashMap<String, String> {
    let re = Regex::new(r"\$\{([^}]+)\}")
        .expect("fbuild-config: ini variable substitution regex is a valid pattern");
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
