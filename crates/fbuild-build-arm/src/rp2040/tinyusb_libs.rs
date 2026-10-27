//! Arduino-Pico's TinyUSB-only framework library.
//!
//! Arduino-Pico bundles `Adafruit_TinyUSB_Arduino`, whose API header refuses
//! to compile unless the board's USB-stack menu selected TinyUSB:
//!
//! ```c
//! #if !defined(USE_TINYUSB) && (defined(TINYUSB_HAS_BUITLTIN_CORE_SUPPORT) && ...)
//! #error TinyUSB is not selected, please select it in "Tools->Menu->USB Stack"
//! #endif
//! ```
//!
//! The core's own `SPI`, `SD` and `LittleFS` sources include
//! `<Adafruit_TinyUSB.h>` behind `#ifdef USE_TINYUSB`. The reconciliation
//! pass re-walks a dependent library's full source set and over-selects on
//! guards, so a FastLED 3.10.8 Pico build in `lib/` selected TinyUSB and
//! died on that `#error` (FastLED/fbuild#1719; a bare `SPI` sketch does
//! not reach it). Like the Bluetooth libraries in
//! [`super::bluetooth_libs`], a library the framework declares unbuildable in
//! this configuration is not a candidate unless `USE_TINYUSB` is defined; a
//! `lib_deps` entry still selects it.

use std::collections::HashMap;
use std::path::Path;

use fbuild_packages::library::FrameworkLibrary;

/// How deep under an include dir to look for the guard. The API header sits
/// one level down (`src/arduino/Adafruit_TinyUSB_API.h`).
const MAX_HEADER_DEPTH: usize = 2;

/// Drop framework libraries that require TinyUSB when the build does not
/// select it, keeping any library `lib_deps` names.
pub(crate) fn exclude_tinyusb_libraries_when_disabled(
    libraries: Vec<FrameworkLibrary>,
    defines: &HashMap<String, String>,
    declared: &[String],
) -> Vec<FrameworkLibrary> {
    // The guard tests `defined(USE_TINYUSB)`, so any value counts.
    if defines.contains_key("USE_TINYUSB") {
        return libraries;
    }
    let declared: Vec<String> = declared
        .iter()
        .filter_map(|entry| fbuild_library_select::declared_dep_name(entry))
        .collect();
    libraries
        .into_iter()
        .filter(|library| {
            if declared.contains(&library.name.to_ascii_lowercase()) || !needs_tinyusb(library) {
                return true;
            }
            tracing::info!(
                library = %library.name,
                "skipping framework library: it #errors unless USE_TINYUSB is defined and the \
                 build does not select TinyUSB; select the TinyUSB USB stack or name it in lib_deps"
            );
            false
        })
        .collect()
}

/// Whether any header of `library` errors out when `USE_TINYUSB` is undefined.
fn needs_tinyusb(library: &FrameworkLibrary) -> bool {
    library
        .include_dirs
        .iter()
        .any(|dir| dir_has_tinyusb_guard(dir, MAX_HEADER_DEPTH))
}

fn dir_has_tinyusb_guard(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().map(|entry| entry.path()).any(|path| {
        if path.is_dir() {
            depth > 0 && dir_has_tinyusb_guard(&path, depth - 1)
        } else {
            is_header(&path)
                && std::fs::read_to_string(&path).is_ok_and(|text| errors_without_tinyusb(&text))
        }
    })
}

fn is_header(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("h" | "hh" | "hpp" | "hxx")
    )
}

/// An `#error` directly inside an `#if`/`#ifndef` whose condition requires
/// `USE_TINYUSB` to be undefined. Backslash-continued conditions are joined.
fn errors_without_tinyusb(text: &str) -> bool {
    let mut condition: Option<String> = None;
    let mut continuing = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if continuing {
            if let Some(cond) = condition.as_mut() {
                cond.push(' ');
                cond.push_str(trimmed.trim_end_matches('\\'));
            }
            continuing = trimmed.ends_with('\\');
            continue;
        }
        let Some(directive) = trimmed.strip_prefix('#').map(str::trim_start) else {
            continue;
        };
        if directive.starts_with("if") || directive.starts_with("elif") {
            condition = Some(directive.trim_end_matches('\\').to_string());
            continuing = trimmed.ends_with('\\');
        } else if directive.starts_with("error") {
            if condition.as_deref().is_some_and(requires_tinyusb_undefined) {
                return true;
            }
        } else if directive.starts_with("else") || directive.starts_with("endif") {
            condition = None;
        }
    }
    false
}

fn requires_tinyusb_undefined(condition: &str) -> bool {
    let compact: String = condition.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("!defined(USE_TINYUSB)")
        || compact.contains("!definedUSE_TINYUSB")
        || compact == "ifndefUSE_TINYUSB"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(root: &Path, name: &str, files: &[(&str, &str)]) -> FrameworkLibrary {
        let dir = root.join("libraries").join(name);
        let src = dir.join("src");
        for (rel, body) in files {
            let path = src.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        FrameworkLibrary {
            name: name.to_string(),
            dir,
            include_dirs: vec![src],
            source_files: Vec::new(),
        }
    }

    /// The real shapes from Arduino-Pico 4.5.3: the TinyUSB API header guards
    /// itself with a continued `#if`, and `SPI.cpp` includes TinyUSB only
    /// under `#ifdef USE_TINYUSB` (its header never mentions it).
    fn fixture(root: &Path) -> Vec<FrameworkLibrary> {
        vec![
            library(
                root,
                "Adafruit_TinyUSB_Arduino",
                &[
                    (
                        "Adafruit_TinyUSB.h",
                        "#pragma once\n#include \"arduino/Adafruit_TinyUSB_API.h\"\n",
                    ),
                    (
                        "arduino/Adafruit_TinyUSB_API.h",
                        "#if !defined(USE_TINYUSB) && (defined(TINYUSB_HAS_BUITLTIN_CORE_SUPPORT) && \\\n     !defined(ARDUINO_ARCH_ESP32))\n#error TinyUSB is not selected, please select it in \"Tools->Menu->USB Stack\"\n#endif\n",
                    ),
                ],
            ),
            library(
                root,
                "SPI",
                &[
                    ("SPI.h", "#pragma once\n#include <Arduino.h>\n"),
                    (
                        "SPI.cpp",
                        "#ifdef USE_TINYUSB\n#include <Adafruit_TinyUSB.h>\n#endif\n",
                    ),
                ],
            ),
        ]
    }

    fn names(libraries: &[FrameworkLibrary]) -> Vec<&str> {
        libraries.iter().map(|lib| lib.name.as_str()).collect()
    }

    #[test]
    fn drops_tinyusb_when_the_usb_stack_is_not_tinyusb() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_tinyusb_libraries_when_disabled(
            fixture(tmp.path()),
            &HashMap::from([("ARDUINO_ARCH_RP2040".to_string(), "1".to_string())]),
            &[],
        );
        assert_eq!(names(&kept), ["SPI"]);
    }

    #[test]
    fn keeps_tinyusb_when_use_tinyusb_is_defined() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_tinyusb_libraries_when_disabled(
            fixture(tmp.path()),
            &HashMap::from([("USE_TINYUSB".to_string(), String::new())]),
            &[],
        );
        assert_eq!(names(&kept), ["Adafruit_TinyUSB_Arduino", "SPI"]);
    }

    #[test]
    fn keeps_declared_tinyusb_even_when_not_selected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_tinyusb_libraries_when_disabled(
            fixture(tmp.path()),
            &HashMap::new(),
            &["adafruit/Adafruit_TinyUSB_Arduino@^3".to_string()],
        );
        assert_eq!(names(&kept), ["Adafruit_TinyUSB_Arduino", "SPI"]);
    }

    #[test]
    fn an_error_under_a_positive_guard_does_not_mark_a_library() {
        assert!(!errors_without_tinyusb(
            "#ifdef USE_TINYUSB\n#error no\n#endif\n"
        ));
        assert!(!errors_without_tinyusb(
            "#if !defined(USE_TINYUSB)\n#define X\n#else\n#error no\n#endif\n"
        ));
        assert!(errors_without_tinyusb(
            "#ifndef USE_TINYUSB\n#error yes\n#endif\n"
        ));
    }
}
