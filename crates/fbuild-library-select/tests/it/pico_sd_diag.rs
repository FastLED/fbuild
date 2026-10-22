//! Explicit real-SDK diagnostic for FastLED's Arduino-Pico SD dependency.

use std::collections::HashMap;

use fbuild_core::path::NormalizedPath;
use fbuild_library_select::resolve_with_stats_active_declared;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use fbuild_packages::library::library_info::InstalledLibrary;

#[test]
#[ignore = "requires FASTLED_SRC, ARDUINO_PICO_ROOT and RP_DEFINES_FILE from a real compile command"]
fn real_pico_sd_selects_sdfs() {
    let (Ok(fastled), Ok(framework), Ok(define_file)) = (
        std::env::var("FASTLED_SRC"),
        std::env::var("ARDUINO_PICO_ROOT"),
        std::env::var("RP_DEFINES_FILE"),
    ) else {
        eprintln!("SKIP diagnostic: supply FASTLED_SRC, ARDUINO_PICO_ROOT and RP_DEFINES_FILE");
        return;
    };
    let fastled = NormalizedPath::from(fastled);
    let framework = NormalizedPath::from(framework);
    let defines: HashMap<String, String> = std::fs::read_to_string(define_file)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("-D"))
        .map(|line| {
            let (name, value) = line.split_once('=').unwrap_or((line, "1"));
            (name.to_string(), value.to_string())
        })
        .collect();
    assert!(defines.contains_key("ARDUINO_ARCH_RP2040"));
    let libraries = discover_framework_libraries(&framework.join("libraries"));
    assert!(libraries.iter().any(|library| library.name == "SDFS"));
    let seeds = InstalledLibrary::new(&fastled, "FastLED").get_source_files();
    assert!(seeds.iter().any(|path| path.ends_with("fl.fs.sd+.cpp")));
    // Match the current RP orchestrator's LDF search path, then compare the
    // compiler's additional core root without touching SDK or source files.
    for include_core in [false, true] {
        let mut paths = vec![fastled.clone().into_path_buf()];
        if include_core {
            paths.push(framework.join("cores/rp2040").into_path_buf());
        }
        let (selection, stats) =
            resolve_with_stats_active_declared(&seeds, &paths, &libraries, &defines, &[]);
        eprintln!(
            "include_core={include_core} passes={} files={} selected={:?}",
            stats.passes, stats.files_read, selection.required_libraries
        );
        assert!(
            selection
                .source_files
                .iter()
                .any(|path| path.ends_with("SDFS.cpp")),
            "actual FastLED SD implementation requires SDK SDFS.cpp: {:?}",
            selection.required_libraries
        );
    }
}
