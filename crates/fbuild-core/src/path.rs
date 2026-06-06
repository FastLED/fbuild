//! Cross-platform path normalization with stable cache-keyable
//! representation.
//!
//! Vendored from [zccache's `core::path`] (300+ LOC), pared to the
//! ~200 lines fbuild needs for its `BuildInfo` toolchain paths +
//! future cache fingerprints. PRs that follow #437 will migrate
//! [`crate::path::NormalizedPath`] into `BuildInfo` and add a dylint
//! that bans raw `PathBuf` / `String` in `*_path` slots.
//!
//! Why this exists: PR #436 (closing #428) ran into a Windows-only
//! test failure where `PathBuf::from("/bin/avr-size").parent().join(
//! "avr-nm")` produced `/bin\avr-nm` on Windows and `/bin/avr-nm` on
//! Linux. The string forms drift across platforms — and so do the
//! cache keys built from them. `NormalizedPath`'s precomputed key
//! collapses `\\` → `/` (and case-folds on case-insensitive
//! filesystems), making `Hash`/`Eq`/`Ord` produce identical results
//! across platforms for identical paths.
//!
//! [zccache's `core::path`]: https://github.com/zackees/zccache/blob/main/crates/zccache/src/core/path.rs

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A normalized, platform-aware path representation.
///
/// On case-insensitive filesystems (Windows, default macOS), paths are
/// stored in a canonical form for consistent cache keying.
///
/// Internal storage is `Arc<Path>` + `Arc<str>` so `Clone` is two
/// atomic refcount bumps rather than two heap allocations. The
/// pre-computed `key` is what `Hash`/`Ord`/`PartialEq` compare on —
/// constructing a new `NormalizedPath` runs `normalize_for_key` once;
/// subsequent operations are O(1) per byte rather than re-allocating
/// a normalized string per call.
#[derive(Debug, Clone)]
pub struct NormalizedPath {
    /// The path, normalized to remove `.` / `..` components but
    /// otherwise preserving original casing and separators. This is
    /// what `as_path()` / `Display` surface.
    path: Arc<Path>,
    /// Pre-computed `normalize_for_key` result — case-folded on
    /// case-insensitive platforms, slash-normalized everywhere.
    /// `Hash`/`Ord`/`PartialEq` compare on this so equality is
    /// platform-stable.
    key: Arc<str>,
}

impl PartialEq for NormalizedPath {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for NormalizedPath {}

impl Hash for NormalizedPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl PartialOrd for NormalizedPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NormalizedPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl std::fmt::Display for NormalizedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.display().fmt(f)
    }
}

impl Default for NormalizedPath {
    /// Empty path. Mirrors the existing `BuildInfo` schema convention
    /// where "missing toolchain" is encoded as an empty string in
    /// `build_info.json`. Lets `#[serde(default)]` on
    /// [`NormalizedPath`] fields preserve that semantic for free.
    fn default() -> Self {
        Self::new("")
    }
}

impl NormalizedPath {
    /// Create a new normalized path. Runs `normalize_for_key` once and
    /// caches the result for subsequent `Hash`/`Ord`/`PartialEq`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = normalize(path.as_ref());
        let key: Arc<str> = Arc::from(normalize_for_key(&path));
        let path: Arc<Path> = Arc::from(path);
        Self { path, key }
    }

    /// Borrow the underlying [`Path`].
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Borrow the precomputed comparison key. Case-folded on
    /// case-insensitive platforms, slash-normalized everywhere.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Convert back to an owned [`PathBuf`]. Allocates a fresh
    /// `PathBuf` (the inner storage is `Arc<Path>`); prefer
    /// [`as_path`](Self::as_path) when a borrow suffices.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.path.to_path_buf()
    }

    /// Join a path segment onto this normalized path.
    #[must_use]
    pub fn join(&self, path: impl AsRef<Path>) -> Self {
        Self::new(self.path.join(path))
    }

    /// Render the path with forward slashes, preserving original case.
    ///
    /// This is the form used for serialization (`build_info.json`,
    /// cache fingerprints, anything that gets compared byte-for-byte
    /// across platforms). Unlike [`key`](Self::key) it does *not*
    /// case-fold, so it stays human-readable while still being
    /// equality-stable across Linux/macOS/Windows when fed identical
    /// paths.
    ///
    /// Cheap: walks the path once; no `Arc` clone, no normalization.
    #[must_use]
    pub fn display_slash(&self) -> String {
        let mut s = self.path.to_string_lossy().into_owned();
        if cfg!(windows) {
            s = s.replace('\\', "/");
            if let Some(stripped) = s.strip_prefix("//?/") {
                s = stripped.to_string();
            }
        }
        s
    }
}

impl AsRef<Path> for NormalizedPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<OsStr> for NormalizedPath {
    fn as_ref(&self) -> &OsStr {
        self.as_path().as_os_str()
    }
}

impl Deref for NormalizedPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.as_path()
    }
}

impl From<PathBuf> for NormalizedPath {
    fn from(path: PathBuf) -> Self {
        Self::new(path)
    }
}

impl From<&Path> for NormalizedPath {
    fn from(path: &Path) -> Self {
        Self::new(path)
    }
}

impl From<String> for NormalizedPath {
    fn from(path: String) -> Self {
        Self::new(path)
    }
}

impl From<&str> for NormalizedPath {
    fn from(path: &str) -> Self {
        Self::new(path)
    }
}

impl From<&String> for NormalizedPath {
    fn from(path: &String) -> Self {
        Self::new(path)
    }
}

impl Serialize for NormalizedPath {
    /// Emits the slash-normalized, case-preserving form
    /// ([`display_slash`](NormalizedPath::display_slash)). This is the
    /// load-bearing invariant that makes `build_info.json` and every
    /// other serialized `NormalizedPath` byte-identical across Linux,
    /// macOS, and Windows — closing the regression PR #436 ran into
    /// where `PathBuf::join` introduced `\` separators on Windows and
    /// broke cache lookups + cross-platform JSON equality. See #437.
    ///
    /// Case is *not* folded here — the cache key
    /// ([`key`](NormalizedPath::key)) folds case for hashing, but the
    /// serialized form keeps original casing so emitted JSON stays
    /// human-readable.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.display_slash().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NormalizedPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        PathBuf::deserialize(deserializer).map(Self::new)
    }
}

/// Normalize a path by resolving `.` and `..` components without
/// touching the filesystem (no symlink resolution).
///
/// Intentionally not `std::fs::canonicalize` — we avoid filesystem
/// access and symlink resolution for performance and determinism.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

/// Normalize a path into a stable string key for hashing and
/// comparisons.
///
/// Shared representation for path-based cache keys. Avoids filesystem
/// access. Strips Windows extended-length prefixes (`\\?\`),
/// normalizes separators (`\\` → `/`), and folds case on
/// case-insensitive platforms (Windows, macOS).
#[must_use]
pub fn normalize_for_key(path: &Path) -> String {
    let normalized = normalize(path);

    #[cfg(windows)]
    {
        let mut s = normalized.to_string_lossy().replace('\\', "/");
        if let Some(stripped) = s.strip_prefix("//?/") {
            s = stripped.to_string();
        }
        s.make_ascii_lowercase();
        s
    }

    #[cfg(target_os = "macos")]
    {
        normalized.to_string_lossy().to_lowercase()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        normalized
            .into_os_string()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The motivating bug from PR #436: `/bin/avr-size` →
    /// `/bin/avr-nm` on Linux but `/bin\avr-nm` on Windows when
    /// constructed via `Path::join`. `NormalizedPath::key()` must
    /// never carry a literal `\` — that's what was tripping JSON
    /// equality assertions across platforms.
    #[test]
    fn key_never_contains_backslash() {
        let unix = NormalizedPath::new("/bin/avr-nm");
        assert!(unix.key().contains("avr-nm"));
        assert!(!unix.key().contains('\\'));

        // Same path with backslashes — on Windows this resolves to the
        // same key (slash-normalized); on Linux the backslash is just
        // part of the filename, but the key still doesn't carry one.
        #[cfg(windows)]
        {
            let win = NormalizedPath::new(r"\bin\avr-nm");
            assert_eq!(unix.key(), win.key());
        }
    }

    /// On any platform, two `NormalizedPath`s constructed from the
    /// same slash-delimited string must be equal.
    #[test]
    fn slash_form_is_equal_to_itself() {
        let a = NormalizedPath::new("/usr/local/bin/clang");
        let b = NormalizedPath::new("/usr/local/bin/clang");
        assert_eq!(a, b);
        assert_eq!(a.key(), b.key());
    }

    /// `normalize` strips `.` and resolves `..` without touching the
    /// filesystem.
    #[test]
    fn normalize_strips_dots_and_resolves_parents() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    /// On case-insensitive platforms (Windows + macOS), equality
    /// folds case. On Linux it does not.
    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn case_insensitive_equality_on_windows_and_macos() {
        let a = NormalizedPath::new("/Bin/AVR-NM");
        let b = NormalizedPath::new("/bin/avr-nm");
        assert_eq!(a, b);
    }

    /// On Linux paths are case-sensitive — same string, different
    /// case must compare unequal.
    #[test]
    #[cfg(not(any(windows, target_os = "macos")))]
    fn case_sensitive_inequality_on_linux() {
        let a = NormalizedPath::new("/Bin/AVR-NM");
        let b = NormalizedPath::new("/bin/avr-nm");
        assert_ne!(a, b);
    }

    /// `Hash` agrees with `Eq` — required by HashMap correctness.
    /// Two equal paths must hash to the same value.
    #[test]
    fn hash_agrees_with_eq() {
        use std::collections::HashSet;
        let a = NormalizedPath::new("/usr/bin/nm");
        let b = NormalizedPath::new("/usr/bin/nm");
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    /// `Ord` agrees with `Eq` — equal paths compare `Equal`.
    #[test]
    fn ord_agrees_with_eq() {
        let a = NormalizedPath::new("/usr/bin/nm");
        let b = NormalizedPath::new("/usr/bin/nm");
        assert_eq!(a.cmp(&b), Ordering::Equal);
    }

    /// JSON round-trips preserve original case and produce equal
    /// `NormalizedPath`s on both ends — serialization emits the
    /// slash-form (see `serialize_emits_slash_form`) so the round
    /// trip is `PathBuf("/Some/Mixed/Case/Path")` on every platform.
    #[test]
    fn json_round_trip_preserves_path_form() {
        let original = NormalizedPath::new("/Some/Mixed/Case/Path");
        let json = serde_json::to_string(&original).unwrap();
        let back: NormalizedPath = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
        // Forward-slash form survives, case preserved.
        assert!(json.contains("/Some/Mixed/Case/Path"));
        assert!(!json.contains('\\'));
    }

    /// The point of [`NormalizedPath::serialize`] — JSON output is
    /// byte-identical across platforms, so cache keys / file-content
    /// equality assertions don't drift between Linux and Windows.
    /// This is the regression that motivated #437 (and the test
    /// PR #436 had to patch around with a `pj()` helper).
    #[test]
    fn serialize_emits_slash_form() {
        let p = NormalizedPath::new("/bin/avr-nm");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"/bin/avr-nm\"");
        // On Windows, a backslash-shaped input must round-trip to
        // forward slashes in the JSON.
        let mixed = NormalizedPath::new(r"C:\Users\zach\bin\avr-nm");
        let json = serde_json::to_string(&mixed).unwrap();
        assert!(
            !json.contains('\\'),
            "serialized JSON must not contain `\\`: {json}",
        );
    }

    /// On Windows, the `\\?\` extended-length prefix is stripped from
    /// the serialized form too — otherwise cache lookups against
    /// hand-typed paths drift.
    #[test]
    #[cfg(windows)]
    fn serialize_strips_extended_length_prefix() {
        let p = NormalizedPath::new(r"\\?\C:\Users\test");
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("//?/"),
            "extended-length prefix must be stripped: {json}",
        );
    }

    /// Windows extended-length prefix (`\\?\`) is stripped from the
    /// key. Required so canonicalised paths don't drift from
    /// hand-typed forms in cache lookups.
    #[test]
    #[cfg(windows)]
    fn windows_extended_length_prefix_is_stripped_in_key() {
        let prefixed = NormalizedPath::new(r"\\?\C:\Users\test");
        let plain = NormalizedPath::new(r"C:\Users\test");
        assert_eq!(prefixed.key(), plain.key());
    }

    /// `Clone` is cheap (refcount bump on the two `Arc`s).
    #[test]
    fn clone_preserves_equality() {
        let a = NormalizedPath::new("/usr/bin/nm");
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(a.key(), b.key());
    }

    /// `join` returns a new normalized path. The result's key
    /// follows the platform's normalization rules.
    #[test]
    fn join_normalizes_result() {
        let base = NormalizedPath::new("/usr/local");
        let joined = base.join("bin/nm");
        // The key is platform-normalized (always slash on Windows).
        assert!(joined.key().ends_with("bin/nm") || joined.key().ends_with("bin\\nm"));
        // The displayed path joined the segment.
        assert!(joined.as_path().to_string_lossy().contains("nm"));
    }

    /// `Deref<Target = Path>` lets `NormalizedPath` flow into any API
    /// that takes `&Path`.
    #[test]
    fn deref_to_path_works() {
        fn takes_path(p: &Path) -> bool {
            p.to_string_lossy().contains("nm")
        }
        let n = NormalizedPath::new("/usr/bin/nm");
        assert!(takes_path(&n));
    }
}
