//! PlatformIO cold-build phase breakdown from `pio run -v` output.
//!
//! PlatformIO has no phase timer, but `-v` makes SCons print every command
//! just before it runs. Stamping each line on arrival gives the start time of
//! every compile, archive, link and image step, which is enough to split a
//! build into the same phases fbuild's perf log reports, so the two tools can
//! be compared phase by phase (FastLED/fbuild#1537).

use std::collections::BTreeMap;

/// What a verbose PlatformIO output line starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Compile,
    Link,
    Image,
}

/// Classify one `pio run -v` line; `None` for everything that is not a
/// compile, the firmware link, or the firmware image/size step.
pub fn classify(line: &str) -> Option<Step> {
    let line = line.trim();
    let writes = |suffix: &str| {
        line.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair[0] == "-o" && pair[1].trim_matches('"').ends_with(suffix))
    };
    if writes("firmware.elf") {
        return Some(Step::Link);
    }
    let image_tool = line.contains("elf2image") || line.contains("objcopy");
    if image_tool && (line.contains("firmware.bin") || line.contains("firmware.hex")) {
        return Some(Step::Image);
    }
    if line.starts_with("Checking size") {
        return Some(Step::Image);
    }
    if line.contains(" -c ") && writes(".o") {
        return Some(Step::Compile);
    }
    None
}

/// Phase durations (ms) from `(arrival_ms, line)` pairs and the build's total.
///
/// * `pre-compile`: start until the first compile starts
/// * `compile`: first compile until the firmware link starts (link waits for
///   every compile and archive)
/// * `link`: link start until the image step starts
/// * `convert-size`: image step until the build exits
///
/// `None` when the output shows no compile or no link (e.g. a no-op build).
pub fn phases(lines: &[(f64, String)], total_ms: f64) -> Option<BTreeMap<String, f64>> {
    let first = |step: Step, after: f64| {
        lines
            .iter()
            .find(|(at, line)| *at >= after && classify(line) == Some(step))
            .map(|(at, _)| *at)
    };
    let compile = first(Step::Compile, 0.0)?;
    let link = first(Step::Link, compile)?;
    let image = first(Step::Image, link).unwrap_or(total_ms);
    let round = |ms: f64| (ms * 1000.0).round() / 1000.0;
    Some(BTreeMap::from([
        ("pre-compile".to_string(), round(compile)),
        ("compile".to_string(), round(link - compile)),
        ("link".to_string(), round(image - link)),
        (
            "convert-size".to_string(),
            round((total_ms - image).max(0.0)),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPILE: &str = "xtensa-esp32s3-elf-g++ -o .pio/build/esp32s3/FrameworkArduino/Esp.cpp.o -c -std=gnu++11 -Os Esp.cpp";
    const ARCHIVE: &str = "xtensa-esp32s3-elf-ar rc .pio/build/esp32s3/libFrameworkArduino.a a.o";
    const LINK: &str = "xtensa-esp32s3-elf-g++ -o .pio/build/esp32s3/firmware.elf -T memory.ld a.o";
    const IMAGE: &str = "esptool.py --chip esp32s3 elf2image --flash_mode dio -o .pio/build/esp32s3/firmware.bin .pio/build/esp32s3/firmware.elf";

    #[test]
    fn classifies_verbose_platformio_commands() {
        assert_eq!(classify(COMPILE), Some(Step::Compile));
        assert_eq!(classify(ARCHIVE), None);
        assert_eq!(classify(LINK), Some(Step::Link));
        assert_eq!(classify(IMAGE), Some(Step::Image));
        assert_eq!(
            classify(
                "avr-objcopy -O ihex -R .eeprom .pio/build/uno/firmware.elf .pio/build/uno/firmware.hex"
            ),
            Some(Step::Image)
        );
        assert_eq!(
            classify("Checking size .pio/build/uno/firmware.elf"),
            Some(Step::Image)
        );
        assert_eq!(
            classify("Processing esp32s3 (platform: espressif32@6.13.0)"),
            None
        );
    }

    #[test]
    fn bootloader_elf2image_before_compiles_is_not_the_image_phase() {
        let boot = "esptool.py --chip esp32s3 elf2image -o .pio/build/esp32s3/bootloader.bin bootloader.elf";
        assert_eq!(classify(boot), None);
    }

    #[test]
    fn splits_a_build_into_phases() {
        let lines = vec![
            (100.0, "Processing esp32s3".to_string()),
            (800.0, COMPILE.to_string()),
            (1500.0, COMPILE.to_string()),
            (2400.0, ARCHIVE.to_string()),
            (2500.0, LINK.to_string()),
            (3800.0, IMAGE.to_string()),
        ];
        let phases = phases(&lines, 4200.0).unwrap();
        assert_eq!(phases["pre-compile"], 800.0);
        assert_eq!(phases["compile"], 1700.0);
        assert_eq!(phases["link"], 1300.0);
        assert_eq!(phases["convert-size"], 400.0);
    }

    #[test]
    fn no_link_means_no_breakdown() {
        let lines = vec![(10.0, COMPILE.to_string())];
        assert!(phases(&lines, 50.0).is_none());
    }
}
