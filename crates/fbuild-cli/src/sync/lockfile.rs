//! Deterministic JSON `platformio.lock` schema + I/O (FastLED/fbuild#618).
//!
//! Every field is deterministic given the same input: env names sorted, deps
//! within each env sorted by (name, source_type, raw), timestamp trimmed to
//! seconds. Write goes through `fbuild_core::fs::write_atomic_sync` (from
//! #865) so the file cannot be observed half-written.
//!
//! Schema is JSON v1 per the issue's decisions:
//! - Package records are duplicated under each env instead of hoisted to a
//!   top-level table. Optimizing for auditability over disk size.
//! - Only fields with a documented consumer are recorded — no decorative
//!   registry metadata.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::source::{ClassifiedDep, LockStatus, SourceType};

/// Fixed schema version. Bump when the on-disk shape changes; readers
/// refuse to load a lockfile whose version is unrecognized.
pub const LOCKFILE_VERSION: u32 = 1;

/// The whole lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    /// ISO-8601 UTC timestamp when the lockfile was written. Trimmed to
    /// seconds so identical resolutions round-trip to identical bytes.
    pub generated_at: String,
    /// Envs sorted by name. Package records within each env are
    /// duplicated (per issue decision — auditability > disk).
    pub envs: BTreeMap<String, LockEnv>,
}

impl Lockfile {
    /// Build a lockfile from classified deps per env. Sorts everything.
    #[must_use]
    pub fn from_classified(
        generated_at: String,
        envs: BTreeMap<String, Vec<ClassifiedDep>>,
    ) -> Self {
        let mut out_envs = BTreeMap::new();
        for (env_name, deps) in envs {
            let mut packages: Vec<LockPackage> =
                deps.into_iter().map(LockPackage::from_dep).collect();
            packages.sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
                    .then_with(|| format!("{:?}", a.source_type).cmp(&format!("{:?}", b.source_type)))
                    .then_with(|| a.raw.cmp(&b.raw))
            });
            out_envs.insert(env_name, LockEnv { packages });
        }
        Self {
            version: LOCKFILE_VERSION,
            generated_at,
            envs: out_envs,
        }
    }

    /// Serialize to pretty JSON with a trailing newline. Determinism:
    /// `BTreeMap` ⇒ envs sorted alphabetically; `Vec<LockPackage>` was
    /// pre-sorted by [`Self::from_classified`]; `serde_json::to_string_pretty`
    /// preserves struct-field order.
    pub fn to_json_string(&self) -> Result<String, LockfileError> {
        let mut s = serde_json::to_string_pretty(self)
            .map_err(|e| LockfileError::Serialize(e.to_string()))?;
        s.push('\n');
        Ok(s)
    }

    /// Write atomically. Uses `fbuild_core::fs::write_atomic_sync` (#865)
    /// so no half-written file is ever observable.
    pub fn write_atomic(&self, path: &Path) -> Result<(), LockfileError> {
        let json = self.to_json_string()?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        fbuild_core::fs::write_atomic_sync(path, json.as_bytes())
            .map_err(|e| LockfileError::Write(e.to_string()))
    }

    /// Read + parse. Errors on missing file, bad JSON, or unrecognized
    /// `version`.
    pub fn read(path: &Path) -> Result<Self, LockfileError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| LockfileError::Read(format!("{}: {e}", path.display())))?;
        let parsed: Self = serde_json::from_str(&raw)
            .map_err(|e| LockfileError::Parse(format!("{}: {e}", path.display())))?;
        if parsed.version != LOCKFILE_VERSION {
            return Err(LockfileError::UnsupportedVersion(parsed.version));
        }
        Ok(parsed)
    }

    /// Compare against a newly-classified snapshot. Ignores
    /// `generated_at`. Returns [`LockDiff::Fresh`] if the classified
    /// package lists match byte-for-byte after re-sorting, otherwise
    /// [`LockDiff::Stale`] with a short human-readable reason.
    #[must_use]
    pub fn compare_to_classified(
        &self,
        classified: &BTreeMap<String, Vec<ClassifiedDep>>,
    ) -> LockDiff {
        // Envs match?
        let lock_env_names: Vec<&String> = self.envs.keys().collect();
        let new_env_names: Vec<&String> = classified.keys().collect();
        if lock_env_names != new_env_names {
            return LockDiff::Stale(format!(
                "envs differ: lock={lock_env_names:?}, new={new_env_names:?}"
            ));
        }
        for (env, new_deps) in classified {
            let Some(lock_env) = self.envs.get(env) else {
                return LockDiff::Stale(format!("env `{env}` missing from lock"));
            };
            // Re-classify the lock's packages to build a compare-shape.
            let mut new_pkgs: Vec<LockPackage> =
                new_deps.iter().cloned().map(LockPackage::from_dep).collect();
            new_pkgs.sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
                    .then_with(|| format!("{:?}", a.source_type).cmp(&format!("{:?}", b.source_type)))
                    .then_with(|| a.raw.cmp(&b.raw))
            });
            if new_pkgs != lock_env.packages {
                return LockDiff::Stale(format!(
                    "packages differ in env `{env}` ({} in lock, {} in new)",
                    lock_env.packages.len(),
                    new_pkgs.len()
                ));
            }
        }
        LockDiff::Fresh
    }
}

/// Comparison verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockDiff {
    /// Lock matches the current `platformio.ini`.
    Fresh,
    /// Lock doesn't match — reason string is user-facing.
    Stale(String),
}

/// One env's worth of packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEnv {
    pub packages: Vec<LockPackage>,
}

/// One package entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockPackage {
    pub name: String,
    #[serde(with = "source_type_serde")]
    pub source_type: SourceType,
    pub raw: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version_spec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub local_path: Option<String>,
    pub status: LockStatus,
    /// Resolved commit SHA (Phase 2 network resolution). `None` in
    /// Phase 1 output.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_sha: Option<String>,
    /// Resolved archive URL (Phase 2). `None` in Phase 1.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_url: Option<String>,
    /// Archive `sha256` (Phase 2). `None` in Phase 1.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
    /// Resolved concrete version (Phase 2). `None` in Phase 1.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_version: Option<String>,
}

impl LockPackage {
    #[must_use]
    pub fn from_dep(d: ClassifiedDep) -> Self {
        let status = d.phase1_lock_status();
        Self {
            name: d.name,
            source_type: d.source_type,
            raw: d.raw,
            owner: d.owner,
            version_spec: d.version_spec,
            url: d.url,
            local_path: d.local_path,
            status,
            resolved_sha: None,
            resolved_url: None,
            sha256: None,
            resolved_version: None,
        }
    }
}

/// Errors from lockfile I/O + comparison.
#[derive(Debug)]
pub enum LockfileError {
    Read(String),
    Write(String),
    Parse(String),
    Serialize(String),
    UnsupportedVersion(u32),
}

impl std::fmt::Display for LockfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(m) => write!(f, "lockfile read: {m}"),
            Self::Write(m) => write!(f, "lockfile write: {m}"),
            Self::Parse(m) => write!(f, "lockfile parse: {m}"),
            Self::Serialize(m) => write!(f, "lockfile serialize: {m}"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported lockfile version {v}, expected {LOCKFILE_VERSION}")
            }
        }
    }
}

impl std::error::Error for LockfileError {}

// `SourceType` doesn't derive its own kebab-case serde attribute in the
// public enum because we want the on-disk name to be stable independent of
// future Rust rename refactors. Manual serde bridge:
mod source_type_serde {
    use super::SourceType;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &SourceType, s: S) -> Result<S::Ok, S::Error> {
        v.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SourceType, D::Error> {
        SourceType::deserialize(d)
    }
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::source::classify;
    use tempfile::tempdir;

    fn deps(env: &str, raw: &[&str]) -> BTreeMap<String, Vec<ClassifiedDep>> {
        let mut m = BTreeMap::new();
        m.insert(env.to_string(), raw.iter().map(|s| classify(s)).collect());
        m
    }

    #[test]
    fn lockfile_shape_smoke() {
        let lock = Lockfile::from_classified(
            "2026-06-30T14:00:00Z".to_string(),
            deps("uno", &["FastLED", "./libs/local"]),
        );
        assert_eq!(lock.version, LOCKFILE_VERSION);
        assert!(lock.envs.contains_key("uno"));
        assert_eq!(lock.envs["uno"].packages.len(), 2);
    }

    #[test]
    fn packages_are_sorted_by_name() {
        let lock = Lockfile::from_classified(
            "t".into(),
            deps("uno", &["Zebra", "Alpha", "Mango"]),
        );
        let names: Vec<&str> = lock.envs["uno"]
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["Alpha", "Mango", "Zebra"]);
    }

    #[test]
    fn envs_are_sorted() {
        let mut classified = BTreeMap::new();
        classified.insert("zzz".to_string(), vec![classify("Foo")]);
        classified.insert("aaa".to_string(), vec![classify("Foo")]);
        classified.insert("mmm".to_string(), vec![classify("Foo")]);
        let lock = Lockfile::from_classified("t".into(), classified);
        let envs: Vec<&str> = lock.envs.keys().map(|s| s.as_str()).collect();
        assert_eq!(envs, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn local_dep_gets_unlocked_status() {
        let lock = Lockfile::from_classified("t".into(), deps("uno", &["./libs/local"]));
        assert_eq!(lock.envs["uno"].packages[0].status, LockStatus::Unlocked);
    }

    #[test]
    fn registry_dep_gets_unresolved_status_in_phase1() {
        let lock = Lockfile::from_classified("t".into(), deps("uno", &["FastLED"]));
        assert_eq!(lock.envs["uno"].packages[0].status, LockStatus::Unresolved);
    }

    #[test]
    fn json_is_deterministic_for_same_input() {
        let a = Lockfile::from_classified(
            "2026-06-30T14:00:00Z".into(),
            deps("uno", &["FastLED@^3.5.0", "./libs/local"]),
        )
        .to_json_string()
        .unwrap();
        let b = Lockfile::from_classified(
            "2026-06-30T14:00:00Z".into(),
            deps("uno", &["./libs/local", "FastLED@^3.5.0"]),
        )
        .to_json_string()
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn roundtrip_write_read() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("platformio.lock");
        let lock = Lockfile::from_classified(
            "2026-06-30T14:00:00Z".into(),
            deps("uno", &["FastLED", "https://github.com/foo/bar#v1.0"]),
        );
        lock.write_atomic(&path).unwrap();
        let read = Lockfile::read(&path).unwrap();
        assert_eq!(read, lock);
    }

    #[test]
    fn read_rejects_unsupported_version() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("platformio.lock");
        let bad = r#"{
  "version": 999,
  "generated_at": "t",
  "envs": {}
}
"#;
        std::fs::write(&path, bad).unwrap();
        match Lockfile::read(&path) {
            Err(LockfileError::UnsupportedVersion(999)) => (),
            other => panic!("expected UnsupportedVersion(999), got {other:?}"),
        }
    }

    #[test]
    fn compare_fresh_when_deps_match() {
        let lock = Lockfile::from_classified(
            "t".into(),
            deps("uno", &["FastLED", "./libs/local"]),
        );
        let diff = lock.compare_to_classified(&deps("uno", &["FastLED", "./libs/local"]));
        assert_eq!(diff, LockDiff::Fresh);
    }

    #[test]
    fn compare_stale_when_new_dep_added() {
        let lock = Lockfile::from_classified("t".into(), deps("uno", &["FastLED"]));
        let diff = lock.compare_to_classified(&deps("uno", &["FastLED", "./libs/new"]));
        assert!(matches!(diff, LockDiff::Stale(_)));
    }

    #[test]
    fn compare_stale_when_env_added() {
        let mut lock_deps = BTreeMap::new();
        lock_deps.insert("uno".to_string(), vec![classify("FastLED")]);
        let lock = Lockfile::from_classified("t".into(), lock_deps.clone());
        lock_deps.insert("esp32".to_string(), vec![classify("FastLED")]);
        let diff = lock.compare_to_classified(&lock_deps);
        assert!(matches!(diff, LockDiff::Stale(_)));
    }

    #[test]
    fn compare_stale_when_dep_version_changes() {
        let lock = Lockfile::from_classified("t".into(), deps("uno", &["FastLED@^3.5.0"]));
        let diff = lock.compare_to_classified(&deps("uno", &["FastLED@^3.6.0"]));
        assert!(matches!(diff, LockDiff::Stale(_)));
    }

    #[test]
    fn json_ends_with_newline() {
        // POSIX-friendly: `cat platformio.lock` doesn't leave a `%`
        // on zsh.
        let lock = Lockfile::from_classified("t".into(), deps("uno", &["FastLED"]));
        let s = lock.to_json_string().unwrap();
        assert!(s.ends_with('\n'));
    }
}
