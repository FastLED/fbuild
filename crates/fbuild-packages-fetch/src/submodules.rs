//! Detect a core unpacked without its git submodule contents.
//!
//! GitHub's auto-generated source archives (`archive/refs/tags/…`) omit
//! submodules by design: the directories are created, the contents are not.
//! Several Arduino cores keep libraries as submodules, so an archive-sourced
//! package extracts to something that looks complete and fails much later,
//! inside the core's own headers.
//!
//! FastLED/fbuild#1380 is the worked example. `esp8266/Arduino` carries
//! `libraries/LittleFS/lib/littlefs`, so `#include <LittleFS.h>` reached
//!
//! ```text
//! LittleFS.h:38:10: fatal error: ../lib/littlefs/lfs.h: No such file
//! ```
//!
//! and `__has_include(<LittleFS.h>)` still passed, because the header was
//! present and only the thing it includes was absent. No consumer-side guard
//! can detect that.
//!
//! The archive carries `.gitmodules` even when it drops the submodule
//! contents, which is what makes this cheap to catch: the file states exactly
//! which directories are supposed to be non-empty.

use std::path::{Path, PathBuf};

/// A declared submodule whose directory came out of the archive empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptySubmodule {
    /// Path as written in `.gitmodules`, relative to the repo root.
    pub declared_path: String,
    /// Where that landed on disk.
    pub extracted_at: PathBuf,
}

/// Parse the `path = …` entries out of a `.gitmodules` file.
///
/// Deliberately not a full INI parse. `.gitmodules` is written by git, the
/// only field this needs is `path`, and a permissive line scan cannot fail
/// closed on an unusual-but-valid file the way a strict parser can.
pub fn declared_submodule_paths(gitmodules: &str) -> Vec<String> {
    gitmodules
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() != "path" {
                return None;
            }
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

/// Whether a directory has no entries. A missing directory is *not* empty for
/// this purpose: git records the submodule directory itself in the archive, so
/// its absence means something else went wrong and this check should not
/// claim otherwise.
fn is_empty_dir(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

/// Report every declared submodule that extracted empty under `root`.
///
/// Returns an empty vec when `root` has no `.gitmodules` — most packages are
/// not git repositories at all, and their absence is the normal case rather
/// than a problem.
pub fn find_empty_submodules(root: &Path) -> Vec<EmptySubmodule> {
    let gitmodules = root.join(".gitmodules");
    let Ok(text) = std::fs::read_to_string(&gitmodules) else {
        return Vec::new();
    };

    declared_submodule_paths(&text)
        .into_iter()
        .filter_map(|declared| {
            let extracted_at = root.join(&declared);
            is_empty_dir(&extracted_at).then_some(EmptySubmodule {
                declared_path: declared,
                extracted_at,
            })
        })
        .collect()
}

/// Message for a package that unpacked without its submodule contents.
///
/// Names the empty directories and the likely cause, because the symptom this
/// prevents — a missing header several layers inside a core — gives the reader
/// nothing to work with.
pub fn empty_submodule_error(package: &str, url: &str, empty: &[EmptySubmodule]) -> String {
    let listed = empty
        .iter()
        .map(|e| format!("  - {}", e.declared_path))
        .collect::<Vec<_>>()
        .join("\n");
    let source_archive_hint = if url.contains("/archive/refs/") {
        "\n\nThe URL above is a GitHub auto-generated source archive, which \
         omits submodules by design. Use the release asset published on the \
         tag if the project provides one (that is what FastLED/fbuild#1380 \
         did for esp8266), or fetch with submodules."
    } else {
        "\n\nThe archive declares these submodules but shipped them empty."
    };
    format!(
        "{package} unpacked without its submodule contents. These directories \
         are declared in .gitmodules and came out empty:\n{listed}\n\nurl: \
         {url}{source_archive_hint}\n\nLeaving this to the compiler produces a \
         missing-header error inside the core, past any `__has_include` guard \
         a sketch could write (FastLED/fbuild#1380)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    const ESP8266_GITMODULES: &str = "\
[submodule \"libraries/LittleFS/lib/littlefs\"]
\tpath = libraries/LittleFS/lib/littlefs
\turl = https://github.com/littlefs-project/littlefs.git
[submodule \"libraries/SoftwareSerial\"]
\tpath = libraries/SoftwareSerial
\turl = https://github.com/plerup/espsoftwareserial.git
";

    #[test]
    fn declared_paths_are_read_from_gitmodules() {
        assert_eq!(
            declared_submodule_paths(ESP8266_GITMODULES),
            vec![
                "libraries/LittleFS/lib/littlefs".to_string(),
                "libraries/SoftwareSerial".to_string(),
            ]
        );
    }

    /// The exact shape FastLED/fbuild#1380 reported: directories present,
    /// contents absent.
    #[test]
    fn empty_submodule_directories_are_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write(root, ".gitmodules", ESP8266_GITMODULES);
        std::fs::create_dir_all(root.join("libraries/LittleFS/lib/littlefs")).unwrap();
        std::fs::create_dir_all(root.join("libraries/SoftwareSerial")).unwrap();
        // The vendored sources next to the empty submodule are what made the
        // real failure confusing — lfs.c present, lfs.h absent.
        write(
            root,
            "libraries/LittleFS/src/LittleFS.h",
            "#include \"../lib/littlefs/lfs.h\"",
        );

        let found = find_empty_submodules(root);
        let paths: Vec<&str> = found.iter().map(|e| e.declared_path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "libraries/LittleFS/lib/littlefs",
                "libraries/SoftwareSerial"
            ]
        );
    }

    #[test]
    fn populated_submodules_are_not_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        write(root, ".gitmodules", ESP8266_GITMODULES);
        write(root, "libraries/LittleFS/lib/littlefs/lfs.h", "// header");
        write(
            root,
            "libraries/SoftwareSerial/SoftwareSerial.h",
            "// header",
        );
        assert!(find_empty_submodules(root).is_empty());
    }

    /// Most packages are plain archives, not git checkouts. No `.gitmodules`
    /// is the normal case and must not be treated as a finding.
    #[test]
    fn a_package_without_gitmodules_is_clean() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), "cores/arduino/main.cpp", "int main(){}");
        assert!(find_empty_submodules(tmp.path()).is_empty());
    }

    /// A declared submodule whose directory is missing entirely is a
    /// different failure — an incomplete extract, not a submodule-less
    /// archive. Reporting it here would send the reader after the wrong
    /// cause.
    #[test]
    fn a_missing_submodule_directory_is_not_claimed_as_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), ".gitmodules", ESP8266_GITMODULES);
        assert!(find_empty_submodules(tmp.path()).is_empty());
    }

    #[test]
    fn the_error_names_the_directories_and_the_archive_kind() {
        let empty = vec![EmptySubmodule {
            declared_path: "libraries/LittleFS/lib/littlefs".to_string(),
            extracted_at: PathBuf::from("/cache/x/libraries/LittleFS/lib/littlefs"),
        }];
        let msg = empty_submodule_error(
            "esp8266-arduino",
            "https://github.com/esp8266/Arduino/archive/refs/tags/3.1.2.tar.gz",
            &empty,
        );
        assert!(msg.contains("libraries/LittleFS/lib/littlefs"), "{msg}");
        assert!(msg.contains("source archive"), "{msg}");

        let release = empty_submodule_error(
            "esp8266-arduino",
            "https://github.com/esp8266/Arduino/releases/download/3.1.2/esp8266-3.1.2.zip",
            &empty,
        );
        assert!(
            !release.contains("source archive"),
            "a release-asset URL must not be blamed on the archive form: {release}"
        );
    }
}
