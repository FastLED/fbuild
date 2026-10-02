//! Arduino-Pico's Bluetooth-only framework libraries.
//!
//! Arduino-Pico marks every library that needs its BTstack configuration with
//! `#include <_needsbt.h>`, a core header that does
//! `static_assert(ENABLE_CLASSIC, "This library needs Bluetooth enabled...")`.
//! `ENABLE_CLASSIC` comes from the board's `ipbtstack` menu (`-DENABLE_CLASSIC=1`
//! on the Bluetooth entries only), so on a build without Bluetooth such a
//! library cannot compile at all.
//!
//! The library finder over-selects on purpose: a guard on a macro the project
//! derives in a header (`#if FL_BLE_AVAILABLE`) is undecidable to the scanner,
//! so every arm is scanned. Since FastLED/fbuild#1473 seeds every translation
//! unit a local library compiles, FastLED's unity TUs reach its RP2350W BLE
//! transport and its guarded `#include <BTstackLib.h>`, and a plain Pico 2
//! build picked `BTstackLib` and died on that static assertion. Neither
//! arduino-cli (which preprocesses) nor PlatformIO's `chain` LDF (which does
//! not scan every library TU) selects it there.
//!
//! An inferred selection of a library the framework itself declares
//! unbuildable in this configuration can never be right, so those libraries
//! are not candidates unless Bluetooth is enabled. A `lib_deps` declaration
//! still selects one, and the framework's own error explains the missing menu
//! setting.

use std::collections::HashMap;
use std::path::Path;

use fbuild_packages::library::FrameworkLibrary;

/// Drop framework libraries that require Arduino-Pico Bluetooth when the
/// build does not enable it, keeping any library `lib_deps` names.
pub(crate) fn exclude_bluetooth_libraries_when_disabled(
    libraries: Vec<FrameworkLibrary>,
    defines: &HashMap<String, String>,
    declared: &[String],
) -> Vec<FrameworkLibrary> {
    if bluetooth_enabled(defines) {
        return libraries;
    }
    let declared: Vec<String> = declared
        .iter()
        .filter_map(|entry| fbuild_library_select::declared_dep_name(entry))
        .collect();
    libraries
        .into_iter()
        .filter(|library| {
            if declared.contains(&library.name.to_ascii_lowercase()) || !needs_bluetooth(library) {
                return true;
            }
            tracing::info!(
                library = %library.name,
                "skipping framework library: it includes <_needsbt.h> and the build does not \
                 enable Bluetooth (ENABLE_CLASSIC); select a Bluetooth `ipbtstack` menu or name \
                 it in lib_deps"
            );
            false
        })
        .collect()
}

/// `_needsbt.h` asserts `ENABLE_CLASSIC`, so that is the switch that decides.
fn bluetooth_enabled(defines: &HashMap<String, String>) -> bool {
    defines
        .get("ENABLE_CLASSIC")
        .map(|value| !matches!(value.trim(), "" | "0"))
        .unwrap_or(false)
}

/// Whether any public header of `library` includes `_needsbt.h`.
fn needs_bluetooth(library: &FrameworkLibrary) -> bool {
    library
        .include_dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .map(|entry| entry.path())
        .filter(|path| is_header(path))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|text| text.lines().any(includes_needsbt))
}

fn is_header(path: &Path) -> bool {
    path.is_file()
        && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("h" | "hh" | "hpp" | "hxx")
        )
}

fn includes_needsbt(line: &str) -> bool {
    let Some(directive) = line.trim_start().strip_prefix('#') else {
        return false;
    };
    let Some(target) = directive.trim_start().strip_prefix("include") else {
        return false;
    };
    let target = target.trim();
    target == "<_needsbt.h>" || target == "\"_needsbt.h\""
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(root: &Path, name: &str, header: &str) -> FrameworkLibrary {
        let dir = root.join("libraries").join(name);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(format!("{name}.h")), header).unwrap();
        let source = src.join(format!("{name}.cpp"));
        std::fs::write(&source, format!("#include \"{name}.h\"\n")).unwrap();
        FrameworkLibrary {
            name: name.to_string(),
            dir,
            include_dirs: vec![src],
            source_files: vec![source],
        }
    }

    /// The real shapes from Arduino-Pico 5.7.0: `BTstackLib.h` includes the
    /// core's `_needsbt.h`; `SPI.h` does not.
    fn fixture(root: &Path) -> Vec<FrameworkLibrary> {
        vec![
            library(
                root,
                "BTstackLib",
                "#pragma once\n#include <Arduino.h>\n#include <_needsbt.h>\n",
            ),
            library(root, "SPI", "#pragma once\n#include <Arduino.h>\n"),
        ]
    }

    fn names(libraries: &[FrameworkLibrary]) -> Vec<&str> {
        libraries.iter().map(|lib| lib.name.as_str()).collect()
    }

    fn defines(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn drops_bluetooth_library_when_bluetooth_is_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_bluetooth_libraries_when_disabled(
            fixture(tmp.path()),
            &defines(&[("LWIP_IPV4", "1")]),
            &[],
        );
        assert_eq!(names(&kept), ["SPI"]);
    }

    #[test]
    fn drops_bluetooth_library_when_enable_classic_is_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_bluetooth_libraries_when_disabled(
            fixture(tmp.path()),
            &defines(&[("ENABLE_CLASSIC", "0")]),
            &[],
        );
        assert_eq!(names(&kept), ["SPI"]);
    }

    #[test]
    fn keeps_bluetooth_library_when_the_menu_enables_bluetooth() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_bluetooth_libraries_when_disabled(
            fixture(tmp.path()),
            &defines(&[("ENABLE_CLASSIC", "1"), ("ENABLE_BLE", "1")]),
            &[],
        );
        assert_eq!(names(&kept), ["BTstackLib", "SPI"]);
    }

    #[test]
    fn keeps_declared_bluetooth_library_even_when_disabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = exclude_bluetooth_libraries_when_disabled(
            fixture(tmp.path()),
            &HashMap::new(),
            &["btstacklib@^1.0".to_string()],
        );
        assert_eq!(names(&kept), ["BTstackLib", "SPI"]);
    }

    #[test]
    fn a_commented_out_include_does_not_mark_a_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        let libs = vec![library(
            tmp.path(),
            "Quiet",
            "#pragma once\n// #include <_needsbt.h>\n",
        )];
        let kept = exclude_bluetooth_libraries_when_disabled(libs, &HashMap::new(), &[]);
        assert_eq!(names(&kept), ["Quiet"]);
    }
}
