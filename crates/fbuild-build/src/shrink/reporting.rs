//! Shrink reporting: green `auto shrinking: …` one-liner (FastLED/fbuild#493, #498).
//!
//! Phase 1d lands the one-liner emitter. It writes a single line in green
//! enumerating the libc/runtime symbols being shadowed by `--shrink=auto` on
//! the current platform. When the platform's registry is empty (every
//! platform in Phase 1d), the line is omitted entirely — silence is the
//! default.
//!
//! `owo-colors` auto-disables on non-tty and respects `NO_COLOR`, so no
//! per-call gating is needed. The emitter writes to any `io::Write` so tests
//! can capture the output deterministically (and disable color via
//! `OwoColorize::if_supports_color` is not needed — the test takes the
//! always-color path).

use std::io::{self, Write};

use owo_colors::OwoColorize;

use super::registry::AutoShrinkEntry;

/// Emit the green `auto shrinking: <sym1>, <sym2>, …` one-liner to `out`.
///
/// When `entries` is empty, writes nothing — the line is omitted entirely.
/// This is the steady state for every platform in Phase 1d and remains the
/// expected behavior for platforms with no available shrinkers (e.g. AVR
/// when the sketch already uses `%f`, ESP-IDF 6.x where picolibc is the
/// libc, Teensy 3+/4+ where `Print.cpp` already bypasses vfprintf).
///
/// Subsequent phases (4–6) call this once at the top of every build, after
/// the auto-resolver has decided to use the `safe` path on a platform with
/// a non-empty registry.
///
/// # Errors
///
/// Forwards `io::Error` from the underlying writer.
pub fn print_auto_shrinking_line(
    out: &mut impl Write,
    entries: &[AutoShrinkEntry],
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let symbols: Vec<&str> = entries
        .iter()
        .flat_map(|e| e.symbols.iter().copied())
        .collect();
    if symbols.is_empty() {
        // Defensive: a registry entry with an empty symbol list is
        // structurally legal but semantically useless. Skip silently
        // rather than emit `auto shrinking: ` with no payload.
        return Ok(());
    }

    let prefix = "auto shrinking:".bold().green().to_string();
    let payload = symbols.join(", ").green().to_string();
    writeln!(out, "{prefix} {payload}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escape sequences from `s` so the assertion text doesn't
    /// have to embed terminal codes. Handles the small subset emitted by
    /// `owo-colors` (CSI `<params>` `m`).
    fn strip_ansi(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // Skip until the terminating letter (A–Z, a–z).
                i += 2;
                while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                i += 1; // skip the terminator itself
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn empty_registry_prints_nothing() {
        let mut buf = Vec::new();
        print_auto_shrinking_line(&mut buf, &[]).unwrap();
        assert!(
            buf.is_empty(),
            "expected silent output for empty registry, got: {:?}",
            String::from_utf8_lossy(&buf),
        );
    }

    #[test]
    fn registry_with_empty_symbols_prints_nothing() {
        let entries = [AutoShrinkEntry {
            category: "printf-thin",
            symbols: &[],
        }];
        let mut buf = Vec::new();
        print_auto_shrinking_line(&mut buf, &entries).unwrap();
        assert!(
            buf.is_empty(),
            "expected silent output when entries carry no symbols",
        );
    }

    #[test]
    fn single_entry_lists_its_symbols() {
        let entries = [AutoShrinkEntry {
            category: "printf-thin",
            symbols: &["vfprintf", "vsnprintf"],
        }];
        let mut buf = Vec::new();
        print_auto_shrinking_line(&mut buf, &entries).unwrap();
        let plain = strip_ansi(std::str::from_utf8(&buf).unwrap());
        assert_eq!(plain, "auto shrinking: vfprintf, vsnprintf\n");
    }

    #[test]
    fn multiple_entries_are_concatenated() {
        let entries = [
            AutoShrinkEntry {
                category: "printf-thin",
                symbols: &["vfprintf", "vsnprintf"],
            },
            AutoShrinkEntry {
                category: "scanf-thin",
                symbols: &["vfscanf"],
            },
        ];
        let mut buf = Vec::new();
        print_auto_shrinking_line(&mut buf, &entries).unwrap();
        let plain = strip_ansi(std::str::from_utf8(&buf).unwrap());
        assert_eq!(plain, "auto shrinking: vfprintf, vsnprintf, vfscanf\n");
    }

    #[test]
    fn output_contains_ansi_green_when_color_enabled() {
        // owo-colors honors `NO_COLOR` and tty detection at the macro level,
        // but the bare `.green().to_string()` form (used by the emitter)
        // unconditionally writes the escape sequence. This test guards
        // against accidentally switching to a gated coloring helper that
        // would silently drop color on capture-buffer writes.
        let entries = [AutoShrinkEntry {
            category: "printf-thin",
            symbols: &["vfprintf"],
        }];
        let mut buf = Vec::new();
        print_auto_shrinking_line(&mut buf, &entries).unwrap();
        let raw = std::str::from_utf8(&buf).unwrap();
        assert!(
            raw.contains("\x1b["),
            "expected ANSI escape sequence in output, got: {raw:?}",
        );
    }
}
