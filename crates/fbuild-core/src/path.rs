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

    /// Return this path relative to `base` when `base` is a component-boundary
    /// prefix. The returned path keeps normalized separators and casing.
    #[must_use]
    pub fn relative_to(&self, base: &Self) -> Option<Self> {
        self.path.strip_prefix(base.as_path()).ok().map(Self::new)
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

/// Strip Windows extended-length / UNC prefix from a path.
///
/// On Unix this is a no-op (returns the path unchanged). On Windows
/// it strips `\\?\` (extended-length) and `\\?\UNC\` (UNC normalized)
/// prefixes that `std::fs::canonicalize` injects, so cache keys and
/// log lines stay readable. See FastLED/fbuild#844 "Bridge pair 5".
#[must_use]
pub fn strip_unc_prefix(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        // `\\?\UNC\server\share\...` → `\\server\share\...`
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            let mut out = String::from(r"\\");
            out.push_str(rest);
            return PathBuf::from(out);
        }
        // `\\?\C:\...` → `C:\...`
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        PathBuf::from(s.into_owned())
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

/// Canonicalize an existing path, stripping the Windows UNC prefix and
/// wrapping in [`NormalizedPath`].
///
/// FastLED/fbuild#844 (bridge sweep, "Bridge pair 5"). All
/// `std::fs::canonicalize` / `tokio::fs::canonicalize` call sites
/// migrate to this so the workspace has one source of truth for
/// "canonical form" — `\\?\` prefix stripped, slashes normalized,
/// case-folded on case-insensitive platforms.
///
/// Errors if the path does not exist or cannot be canonicalized
/// (matches `std::fs::canonicalize` semantics).
pub async fn canonicalize_existing(p: impl AsRef<Path>) -> std::io::Result<NormalizedPath> {
    let canon = tokio::fs::canonicalize(p.as_ref()).await?;
    let stripped = strip_unc_prefix(&canon);
    Ok(NormalizedPath::from(stripped))
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

// ---------------------------------------------------------------------------
// Compile-CWD workspace relativization (FastLED/fbuild#952).
//
// These live here (rather than in fbuild-build) so both `fbuild-build`
// (sketch/core compiles) and `fbuild-packages` (library compiles) can
// relativize compile args to the workspace root. Keeping compile args
// workspace-relative is what lets zccache keys stay stable across
// different project directories — see agents/docs/path-conventions.md.
// ---------------------------------------------------------------------------

/// Return the workspace root to use as the CWD for zccache compiles.
///
/// fbuild object files live under `<workspace>/.fbuild/...`, so running
/// the compile from `<workspace>` (and relativizing args to it) lets
/// identically-shaped workspaces at different absolute paths share per-TU
/// cache keys even when the raw args contain absolute paths.
#[must_use]
pub fn compile_cwd_from_output(output: &Path) -> Option<PathBuf> {
    let mut dir = output.parent()?;
    loop {
        if dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".fbuild"))
        {
            return dir.parent().map(|workspace| {
                canonicalize_lexical(workspace).unwrap_or_else(|| workspace.to_path_buf())
            });
        }
        dir = dir.parent()?;
    }
}

/// Return a path argument that is stable relative to the zccache compile CWD.
///
/// Absolute args under the compile CWD are stripped to a workspace-relative
/// tail; everything is finally routed through
/// [`NormalizedPath::display_slash`], which owns the Windows `\` → `/`
/// rewrite GCC's spec-file pass requires (FastLED/fbuild#875, #885, #890,
/// #912). Do NOT hand-roll the slash rewrite — `ban_manual_slash_normalize`
/// flags that.
#[must_use]
pub fn path_arg_for_compile_cwd(path: &Path, cwd: &Path) -> String {
    let relative: PathBuf = if !path.is_absolute() {
        path.to_path_buf()
    } else {
        let stable_path = canonicalize_lexical(path).unwrap_or_else(|| path.to_path_buf());
        let stable_cwd = canonicalize_lexical(cwd).unwrap_or_else(|| strip_unc_prefix(cwd));
        stable_path
            .strip_prefix(&stable_cwd)
            .map(|tail| tail.to_path_buf())
            .unwrap_or(stable_path)
    };
    let arg = NormalizedPath::from(relative).display_slash();
    if arg.is_empty() {
        ".".to_string()
    } else {
        arg
    }
}

/// Normalize common path-bearing compiler flags (`-I`, `-isystem`,
/// `--sysroot`, ...) for a zccache compile CWD.
#[must_use]
pub fn normalize_flags_for_compile_cwd(flags: &[String], cwd: &Path) -> Vec<String> {
    let mut normalized = Vec::with_capacity(flags.len());
    let mut next_is_path = false;

    for flag in flags {
        if next_is_path {
            normalized.push(path_arg_for_compile_cwd(Path::new(flag), cwd));
            next_is_path = false;
            continue;
        }
        if flag_takes_path_argument(flag) {
            normalized.push(flag.clone());
            next_is_path = true;
            continue;
        }
        if let Some(value) = flag.strip_prefix("--sysroot=") {
            normalized.push(format!(
                "--sysroot={}",
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }
        if let Some((prefix, value)) = split_joined_path_flag(flag) {
            normalized.push(format!(
                "{}{}",
                prefix,
                path_arg_for_compile_cwd(Path::new(value), cwd)
            ));
            continue;
        }
        normalized.push(flag.clone());
    }

    normalized
}

/// Canonicalize an existing path (stripping the Windows `\\?\` prefix),
/// falling back to canonicalizing the parent + rejoining the file name when
/// the full path does not yet exist. Returns `None` if neither resolves.
fn canonicalize_lexical(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(strip_unc_prefix(&canonical));
    }
    let parent = path.parent()?.canonicalize().ok()?;
    let joined = match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    };
    Some(strip_unc_prefix(&joined))
}

fn flag_takes_path_argument(flag: &str) -> bool {
    matches!(
        flag,
        "-I" | "-isystem"
            | "-iquote"
            | "-idirafter"
            | "-include"
            | "-imacros"
            | "-isysroot"
            | "--sysroot"
    )
}

fn split_joined_path_flag(flag: &str) -> Option<(&'static str, &str)> {
    for prefix in [
        "-I",
        "-isystem",
        "-iquote",
        "-idirafter",
        "-include",
        "-imacros",
        "-isysroot",
    ] {
        if let Some(value) = flag.strip_prefix(prefix).filter(|value| !value.is_empty()) {
            return Some((prefix, value));
        }
    }
    None
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
        // Forward-slash input survives round-trip unchanged on every
        // platform. This is the load-bearing invariant for
        // `build_info.json` cross-platform byte equality.
        let p = NormalizedPath::new("/bin/avr-nm");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"/bin/avr-nm\"");
    }

    /// On Windows the platform separator is `\`, so any input
    /// constructed via `Path::join` carries backslashes. Serialization
    /// must convert them all to forward slashes — that's the original
    /// regression from PR #436 / #437 this filter exists to fix.
    ///
    /// Unix-cfg note: on Linux/macOS the backslash is a valid filename
    /// character, *not* a separator. Stripping it would mangle real
    /// paths, so the conversion is deliberately Windows-only and this
    /// test is cfg-gated to match.
    #[test]
    #[cfg(windows)]
    fn serialize_converts_backslash_input_to_forward_slashes_on_windows() {
        let mixed = NormalizedPath::new(r"C:\Users\zach\bin\avr-nm");
        let json = serde_json::to_string(&mixed).unwrap();
        assert!(
            !json.contains('\\'),
            "serialized JSON must not contain `\\` on Windows: {json}",
        );
    }

    /// Mirror of the Windows case: on Unix, `\` is content, so the
    /// serialized form must keep the backslashes intact (otherwise
    /// real filenames containing `\` would be corrupted).
    #[test]
    #[cfg(not(windows))]
    fn serialize_preserves_backslashes_as_content_on_unix() {
        // A filename whose actual bytes contain `\` — legal on Linux,
        // unusual but supported on macOS.
        let unix = NormalizedPath::new(r"/tmp/weird\name");
        let json = serde_json::to_string(&unix).unwrap();
        // JSON encodes a literal `\` as `\\`, so the raw string
        // representation has two characters per backslash. Just check
        // that the underlying bytes survived.
        let parsed: String = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, r"/tmp/weird\name");
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

    #[test]
    fn relative_to_handles_equal_nested_and_non_prefix_paths() {
        let base = NormalizedPath::new("cache/archive");
        assert_eq!(
            base.relative_to(&base),
            Some(NormalizedPath::new("")),
            "an equal path is relative to itself"
        );

        let nested = NormalizedPath::new("cache/archive/tool/esptool");
        assert_eq!(
            nested.relative_to(&base),
            Some(NormalizedPath::new("tool/esptool")),
            "a component-boundary prefix produces the nested relative path"
        );

        let similar_but_not_prefix = NormalizedPath::new("cache/archives/tool");
        assert_eq!(similar_but_not_prefix.relative_to(&base), None);
    }

    /// On non-Windows `strip_unc_prefix` is a no-op.
    #[test]
    #[cfg(not(windows))]
    fn strip_unc_prefix_is_noop_on_unix() {
        assert_eq!(
            strip_unc_prefix(Path::new("/usr/bin/nm")),
            PathBuf::from("/usr/bin/nm")
        );
    }

    /// On Windows the extended-length prefix is stripped.
    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_strips_extended_length_on_windows() {
        assert_eq!(
            strip_unc_prefix(Path::new(r"\\?\C:\Users\test")),
            PathBuf::from(r"C:\Users\test")
        );
    }

    /// On Windows the UNC-normalized prefix collapses to plain UNC.
    #[test]
    #[cfg(windows)]
    fn strip_unc_prefix_collapses_unc_form_on_windows() {
        assert_eq!(
            strip_unc_prefix(Path::new(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
    }

    /// `canonicalize_existing` returns a `NormalizedPath` that matches
    /// the canonical form of an existing file. The temp dir trick is
    /// the smallest "existing path" available in a unit test.
    #[tokio::test]
    async fn canonicalize_existing_returns_normalized_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.txt");
        std::fs::write(&file, b"hi").unwrap();
        let canon = canonicalize_existing(&file).await.unwrap();
        // Round-trip through `as_path` and back to `NormalizedPath`
        // produces the same key.
        assert_eq!(canon.key(), NormalizedPath::new(canon.as_path()).key());
    }

    /// `canonicalize_existing` propagates a `NotFound` for a missing
    /// path — same contract as `tokio::fs::canonicalize`.
    #[tokio::test]
    async fn canonicalize_existing_errors_on_missing() {
        let err = canonicalize_existing("/no/such/path/__fbuild_test_missing__")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // --- compile-CWD relativization (moved from zccache.rs, #952) ---

    #[test]
    fn compile_cwd_from_output_uses_workspace_before_fbuild() {
        let output = Path::new("/work/project/.fbuild/build/env/release/src/main.o");
        assert_eq!(
            compile_cwd_from_output(output).as_deref(),
            Some(Path::new("/work/project"))
        );
    }

    #[test]
    fn compile_cwd_from_output_returns_none_without_fbuild_component() {
        let output = Path::new("/work/project/build/env/main.o");
        assert!(compile_cwd_from_output(output).is_none());
    }

    #[test]
    fn compile_cwd_from_output_canonicalizes_existing_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        let output = workspace.join(".fbuild/build/main.o");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        let expected = strip_unc_prefix(&workspace.canonicalize().unwrap());
        assert_eq!(
            compile_cwd_from_output(&output).as_deref(),
            Some(expected.as_path())
        );
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_workspace_relative_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let source = cwd.join("src/main.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        // Forward-slash spelling is the invariant on every platform.
        assert_eq!(path_arg_for_compile_cwd(&source, &cwd), "src/main.cpp");
    }

    #[test]
    #[cfg(unix)]
    fn path_arg_for_compile_cwd_canonicalizes_symlinked_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real");
        let link = tmp.path().join("link");
        let cwd = link.join("project");
        let source = cwd.join("src/main.cpp");
        std::fs::create_dir_all(real.join("project/src")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();

        assert_eq!(path_arg_for_compile_cwd(&source, &cwd), "src/main.cpp");
    }

    #[test]
    #[cfg(windows)]
    fn path_arg_for_compile_cwd_forces_forward_slashes_on_windows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let nested = cwd.join("src").join("sketch");
        let source = nested.join("main.cpp");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let arg = path_arg_for_compile_cwd(&source, &cwd);
        assert!(
            !arg.contains('\\'),
            "compile arg must not contain backslashes: {arg}"
        );
        assert_eq!(arg, "src/sketch/main.cpp");
    }

    #[test]
    fn path_arg_for_compile_cwd_returns_dot_for_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        assert_eq!(path_arg_for_compile_cwd(&cwd, &cwd), ".");
    }

    #[test]
    fn normalize_flags_for_compile_cwd_rewrites_include_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("project");
        let include = cwd.join("include");
        let vendor = cwd.join("vendor");
        let sysroot = cwd.join("sysroot");
        std::fs::create_dir_all(&include).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(&sysroot).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let flags = vec![
            "-I".to_string(),
            include.to_string_lossy().to_string(),
            "-I".to_string(),
            cwd.to_string_lossy().to_string(),
            format!("-I{}", vendor.display()),
            format!("-I{}", cwd.display()),
            format!("--sysroot={}", sysroot.display()),
        ];
        assert_eq!(
            normalize_flags_for_compile_cwd(&flags, &cwd),
            vec![
                "-I".to_string(),
                "include".to_string(),
                "-I".to_string(),
                ".".to_string(),
                "-Ivendor".to_string(),
                "-I.".to_string(),
                "--sysroot=sysroot".to_string(),
            ]
        );
    }
}
