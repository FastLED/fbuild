//! STM32duino variant file selection.
//!
//! Extracted from `orchestrator.rs` (see [`super`]). The variant directory
//! ships sources for many boards (MAPLEMINI, AFROFLIGHT, MALYAN, ...) plus
//! several startup files. We pick exactly one `variant_*.{h,cpp}` and one
//! `PeripheralPins_*.c` per build so the linker doesn't see duplicate symbols.

use std::path::Path;

#[derive(Debug, Clone)]
pub(super) struct SelectedVariantFiles {
    pub(super) header: String,
    pub(super) source_stem: Option<String>,
    pub(super) peripheral_stem: Option<String>,
}

pub(super) fn select_variant_files(
    variant_dir: &Path,
    variant_name: &str,
    preferred_header: Option<&str>,
) -> SelectedVariantFiles {
    let entries = std::fs::read_dir(variant_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();

    let header = preferred_header
        .and_then(|name| find_entry_case_insensitive(&entries, name))
        .or_else(|| pick_variant_file(&entries, variant_name, "variant_", ".h"))
        .unwrap_or_else(|| "variant_generic.h".to_string());

    let header_suffix = Path::new(&header)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("variant_"));
    let source_stem = header_suffix
        .and_then(|suffix| {
            find_entry_case_insensitive(&entries, &format!("variant_{suffix}.cpp"))
                .map(|name| stem_lower(&name))
        })
        .or_else(|| {
            pick_variant_file(&entries, variant_name, "variant_", ".cpp")
                .map(|name| stem_lower(&name))
        });
    let peripheral_stem = header_suffix
        .and_then(|suffix| {
            find_entry_case_insensitive(&entries, &format!("PeripheralPins_{suffix}.c"))
                .or_else(|| {
                    find_entry_case_insensitive(&entries, &format!("peripheralpins_{suffix}.c"))
                })
                .map(|name| stem_lower(&name))
        })
        .or_else(|| {
            pick_variant_file(&entries, variant_name, "peripheralpins_", ".c")
                .map(|name| stem_lower(&name))
        });

    SelectedVariantFiles {
        header,
        source_stem,
        peripheral_stem,
    }
}

fn pick_variant_file(
    entries: &[String],
    variant_name: &str,
    prefix: &str,
    suffix: &str,
) -> Option<String> {
    let normalized = normalize_variant_name(variant_name);
    let exact = format!("{prefix}{normalized}{suffix}");
    if let Some(name) = find_entry_case_insensitive(entries, &exact) {
        return Some(name);
    }

    let generic = format!("{prefix}generic{suffix}");
    if let Some(name) = find_entry_case_insensitive(entries, &generic) {
        return Some(name);
    }

    let mut matches = entries
        .iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            lower.starts_with(prefix) && lower.ends_with(suffix)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by_key(|name| name.to_lowercase());
    matches.into_iter().next()
}

fn find_entry_case_insensitive(entries: &[String], target: &str) -> Option<String> {
    entries
        .iter()
        .find(|name| name.eq_ignore_ascii_case(target))
        .cloned()
}

pub(super) fn keep_variant_source(path: &Path, selected: &SelectedVariantFiles) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let stem = stem_lower(&name);

    if name.starts_with("startup_") {
        return false;
    }
    if name.starts_with("variant_") {
        return selected
            .source_stem
            .as_ref()
            .is_some_and(|wanted| &stem == wanted);
    }
    if name.starts_with("peripheralpins_") {
        return selected
            .peripheral_stem
            .as_ref()
            .is_some_and(|wanted| &stem == wanted);
    }

    true
}

fn normalize_variant_name(name: &str) -> String {
    name.to_lowercase()
        .replace(['/', '\\', '-', ' '], "_")
        .replace("__", "_")
}

fn stem_lower(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}
