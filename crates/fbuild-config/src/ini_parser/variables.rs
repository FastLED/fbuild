//! `${section.key}` variable reference resolution.

use std::collections::HashMap;
use std::collections::HashSet;

/// Resolve a variable reference like "section.key" or "env:name.key".
pub(super) fn resolve_variable(
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

        // Look up the key, following the `extends` chain if needed
        if let Some(val) = resolve_section_key(sections, &section_name, key, &mut HashSet::new()) {
            return val;
        }
    }

    // Try as a key in the current config
    if let Some(val) = current_config.get(reference) {
        return val.clone();
    }

    // Unresolved — return as-is
    format!("${{{}}}", reference)
}

/// Look up a key in a section, following `extends` chains.
fn resolve_section_key(
    sections: &HashMap<String, HashMap<String, String>>,
    section_name: &str,
    key: &str,
    visited: &mut HashSet<String>,
) -> Option<String> {
    if !visited.insert(section_name.to_string()) {
        return None; // circular extends
    }

    let section = sections.get(section_name)?;

    // Direct lookup
    if let Some(val) = section.get(key) {
        return Some(val.clone());
    }

    // Follow extends chain
    if let Some(extends) = section.get("extends") {
        for parent_ref in extends.split(',') {
            let parent_ref = parent_ref.trim();
            // Resolve parent section name (could be "env:name" or bare name)
            let parent_name = if parent_ref.starts_with("env:") || sections.contains_key(parent_ref)
            {
                parent_ref.to_string()
            } else {
                let as_env = format!("env:{}", parent_ref);
                if sections.contains_key(&as_env) {
                    as_env
                } else {
                    parent_ref.to_string()
                }
            };

            if let Some(val) = resolve_section_key(sections, &parent_name, key, visited) {
                return Some(val);
            }
        }
    }

    None
}
