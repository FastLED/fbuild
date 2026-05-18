//! STM32duino `boards.txt` property loader.
//!
//! Extracted from `orchestrator.rs` (see [`super`]).
//!
//! Parses the Arduino-flavored `boards.txt` shipped with STM32duino so the
//! orchestrator can resolve a board id or variant path to a flat property map
//! (variant header, extra ldflags, etc.) while respecting parent `menu.*` scopes.

use std::collections::HashMap;
use std::path::Path;

pub(super) fn load_stm32_framework_props(
    board_or_variant: &str,
    boards_txt: &Path,
) -> Option<HashMap<String, String>> {
    let content = std::fs::read_to_string(boards_txt).ok()?;
    let preferred_key = if board_or_variant.contains('/') {
        ".build.variant"
    } else {
        ".build.board"
    };
    let prefix = find_stm32_prop_prefix(&content, preferred_key, board_or_variant)
        .or_else(|| find_stm32_prop_prefix(&content, ".build.variant", board_or_variant))?;

    let mut props = HashMap::new();
    for scope in stm32_property_scopes(&prefix) {
        let line_prefix = format!("{scope}.");
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some(rest) = trimmed.strip_prefix(&line_prefix) else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let normalized = key
                .strip_prefix("build.")
                .or_else(|| key.strip_prefix("upload."))
                .unwrap_or(key);
            props.insert(normalized.to_string(), value.trim().to_string());
            if normalized != key {
                props.insert(key.to_string(), value.trim().to_string());
            }
        }
    }

    let substitutions = [
        (
            "{build.board}",
            props.get("board").cloned().unwrap_or_default(),
        ),
        (
            "{build.variant}",
            props.get("variant").cloned().unwrap_or_default(),
        ),
    ];
    for value in props.values_mut() {
        for (needle, replacement) in &substitutions {
            if !replacement.is_empty() {
                *value = value.replace(needle, replacement);
            }
        }
    }

    Some(props)
}

fn find_stm32_prop_prefix(content: &str, suffix: &str, value: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let (key, actual) = trimmed.split_once('=')?;
        if key.ends_with(suffix) && actual.trim() == value {
            Some(key.trim_end_matches(suffix).to_string())
        } else {
            None
        }
    })
}

pub(super) fn stm32_property_scopes(prefix: &str) -> Vec<String> {
    let segments = prefix.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Vec::new();
    }

    let mut scopes = vec![segments[0].to_string()];
    let mut idx = 1;
    while idx + 2 < segments.len() {
        if segments[idx] != "menu" {
            break;
        }
        idx += 3;
        scopes.push(segments[..idx].join("."));
    }

    if scopes.last().map_or(true, |scope| scope != prefix) {
        scopes.push(prefix.to_string());
    }

    scopes
}
