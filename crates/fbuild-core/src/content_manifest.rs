//! Portable package tree identity, excluding installation bookkeeping.
//!
//! V1 verification checks paths, sizes, symlink targets and executable bits.
//! In-place tampering preserving size and exec bits is not detected client-side.
//! The publisher hashes the tree it actually packages, so a published artefact
//! always matches its own content. Full re-hash-on-verify is a future option,
//! deliberately outside v1.
//! Hosts without observable executable permission bits cannot verify a tree
//! identity or backfill missing archive modes; callers must use a local build.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Component, Path},
};

pub const SIDECAR: &str = "content-manifest.json";
pub const SCHEMA: u32 = 1;

/// Validated lowercase SHA-256 tree identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentId(String);

impl TryFrom<String> for ContentId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("content id must contain 64 hex digits");
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}
impl From<ContentId> for String {
    fn from(id: ContentId) -> Self {
        id.0
    }
}
impl std::fmt::Display for ContentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub exec: bool,
    pub symlink_target: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentManifest {
    pub schema: u32,
    pub entries: Vec<Entry>,
}

impl ContentManifest {
    /// Field order is part of schema 1. Paths sort by UTF-8 bytes; no whitespace.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut sorted = self.clone();
        sorted
            .entries
            .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        serde_json::to_vec(&sorted).expect("manifest contains only JSON primitives")
    }
    pub fn content_id(&self) -> ContentId {
        ContentId(format!("{:x}", Sha256::digest(self.canonical_bytes())))
    }
    /// Walk metadata only: regular file bytes are never read on the build path.
    pub fn verify_tree(&self, root: &Path) -> io::Result<bool> {
        if crate::platform::fs::executable_bit(&fs::metadata(root)?).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "filesystem cannot verify executable bits",
            ));
        }
        let actual = scan(root, false)?;
        let mut expected = self.entries.clone();
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(self.schema == SCHEMA
            && expected.len() == actual.entries.len()
            && expected.iter().zip(actual.entries).all(|(a, b)| {
                a.path == b.path
                    && a.size == b.size
                    && a.exec == b.exec
                    && a.symlink_target == b.symlink_target
            }))
    }
}

/// Incrementally hashes the same bytes copied from an archive to its output.
#[derive(Default)]
pub struct ManifestBuilder {
    entries: BTreeMap<String, Entry>,
    stamps: BTreeMap<String, String>,
}
impl ManifestBuilder {
    /// Capture the installed file after archive permissions have been applied.
    pub fn record_metadata(&mut self, path: &str, metadata: &fs::Metadata) -> io::Result<()> {
        self.stamps.insert(
            path.into(),
            crate::platform::fs::executable_memo_identity(metadata)?,
        );
        Ok(())
    }
    pub fn copy(
        &mut self,
        path: &str,
        exec: bool,
        input: &mut impl Read,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            size += n as u64;
        }
        self.entries.insert(
            path.into(),
            Entry {
                path: path.into(),
                size,
                sha256: format!("{:x}", hash.finalize()),
                exec,
                symlink_target: None,
            },
        );
        Ok(())
    }
    /// Reconcile installer-generated files and links after submodule preparation.
    /// Already streamed files with unchanged metadata keep their streamed digest.
    pub fn finish(self, root: &Path) -> io::Result<ContentManifest> {
        let mut tree = scan(root, false)?;
        for entry in &mut tree.entries {
            if let Some(streamed) = self.entries.get(&entry.path).filter(|streamed| {
                streamed.size == entry.size
                    && fs::symlink_metadata(root.join(&entry.path)).is_ok_and(|metadata| {
                        crate::platform::fs::executable_memo_identity(&metadata)
                            .ok()
                            .as_ref()
                            == self.stamps.get(&entry.path)
                            && crate::platform::fs::executable_bit(&metadata)
                                .is_none_or(|exec| streamed.exec == exec)
                    })
                    && entry.symlink_target.is_none()
            }) {
                entry.sha256.clone_from(&streamed.sha256);
                entry.exec = streamed.exec;
            } else {
                if entry.symlink_target.is_none()
                    && crate::platform::fs::executable_bit(&fs::metadata(root.join(&entry.path))?)
                        .is_none()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "missing portable archive executable bits",
                    ));
                }
                entry.sha256 = hash_entry(root, entry)?;
            }
        }
        Ok(tree)
    }
}

pub fn portable_path(path: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| io::Error::other("non-UTF-8 manifest path"))?,
            ),
            _ => return Err(io::Error::other("unsafe manifest path")),
        }
    }
    Ok(parts.join("/"))
}

pub fn hash_tree(root: &Path) -> io::Result<ContentManifest> {
    if crate::platform::fs::executable_bit(&fs::metadata(root)?).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cannot reconstruct archive executable bits",
        ));
    }
    scan(root, true)
}

fn scan(root: &Path, hash: bool) -> io::Result<ContentManifest> {
    let mut entries = Vec::new();
    walk(root, Path::new(""), hash, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ContentManifest {
        schema: SCHEMA,
        entries,
    })
}

fn walk(root: &Path, dir: &Path, hash: bool, entries: &mut Vec<Entry>) -> io::Result<()> {
    for child in fs::read_dir(root.join(dir))? {
        let child = child?;
        let relative = dir.join(child.file_name());
        let path = child.path();
        if relative == Path::new(SIDECAR) || relative == Path::new(".install_complete") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            entries.push(Entry {
                path: format!("{}/", portable_path(&relative)?),
                size: 0,
                sha256: if hash {
                    format!("{:x}", Sha256::digest([]))
                } else {
                    String::new()
                },
                exec: executable(&metadata),
                symlink_target: None,
            });
            walk(root, &relative, hash, entries)?;
            continue;
        }
        let target = if metadata.is_symlink() {
            Some(
                fs::read_link(&path)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("non-UTF-8 symlink target"))?
                    .to_owned(),
            )
        } else if metadata.is_file() {
            None
        } else {
            return Err(io::Error::other("unsupported package file type"));
        };
        let mut entry = Entry {
            path: portable_path(&relative)?,
            size: target.as_ref().map_or(metadata.len(), |s| s.len() as u64),
            exec: target.is_none() && executable(&metadata),
            symlink_target: target,
            sha256: String::new(),
        };
        if hash {
            entry.sha256 = hash_entry(root, &entry)?;
        }
        entries.push(entry);
    }
    Ok(())
}

fn hash_entry(root: &Path, entry: &Entry) -> io::Result<String> {
    let mut hash = Sha256::new();
    if entry.path.ends_with('/') {
        return Ok(format!("{:x}", hash.finalize()));
    } else if let Some(target) = &entry.symlink_target {
        hash.update(target.as_bytes());
    } else {
        io::copy(&mut fs::File::open(root.join(&entry.path))?, &mut hash)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn executable(metadata: &fs::Metadata) -> bool {
    crate::platform::fs::executable_bit(metadata).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observable_modes(root: &Path) -> bool {
        let available = crate::platform::fs::executable_bit(&fs::metadata(root).unwrap()).is_some();
        if !available {
            assert_eq!(
                hash_tree(root).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        available
    }

    #[test]
    fn content_id_rejects_invalid_hex() {
        for bad in ["", "00", &"z".repeat(64)] {
            assert!(ContentId::try_from(bad.to_owned()).is_err());
            assert!(serde_json::from_value::<ContentId>(serde_json::json!(bad)).is_err());
        }
    }

    #[test]
    fn golden_three_file_tree_with_symlink_and_exec() {
        let root = tempfile::tempdir().unwrap();
        if !observable_modes(root.path()) {
            return;
        }
        fs::write(root.path().join("a"), b"a").unwrap();
        fs::write(root.path().join("run"), b"x").unwrap();
        crate::platform::fs::set_executable(&root.path().join("run")).unwrap();
        crate::platform::fs::symlink_file(Path::new("a"), &root.path().join("link")).unwrap();
        let manifest = hash_tree(root.path()).unwrap();
        let golden = concat!(
            r#"{"schema":1,"entries":[{"path":"a","size":1,"sha256":"ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","exec":false,"symlink_target":null},"#,
            r#"{"path":"link","size":1,"sha256":"ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb","exec":false,"symlink_target":"a"},"#,
            r#"{"path":"run","size":1,"sha256":"2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881","exec":true,"symlink_target":null}]}"#
        );
        assert_eq!(manifest.canonical_bytes(), golden.as_bytes());
        assert_eq!(
            manifest.content_id().to_string(),
            "b951f448a44eba40fc96dff17016c7cddec7d32e4d7ae33c2590bb120469a956"
        );
        assert!(manifest.verify_tree(root.path()).unwrap());
        fs::write(root.path().join("run"), b"y").unwrap();
        assert!(
            manifest.verify_tree(root.path()).unwrap(),
            "v1 intentionally checks metadata only"
        );
        assert_ne!(
            manifest.content_id(),
            hash_tree(root.path()).unwrap().content_id()
        );
        fs::write(root.path().join("run"), b"larger").unwrap();
        assert!(!manifest.verify_tree(root.path()).unwrap());
    }

    #[test]
    fn tree_identity_tracks_bytes_and_paths_not_creation_order() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        if !observable_modes(a.path()) {
            return;
        }
        for (root, names) in [(a.path(), ["a", "b"]), (b.path(), ["b", "a"])] {
            for name in names {
                std::fs::write(root.join(name), name).unwrap();
            }
        }
        let original = hash_tree(a.path()).unwrap().content_id();
        assert_eq!(original, hash_tree(b.path()).unwrap().content_id());
        std::fs::write(b.path().join("a"), "z").unwrap();
        assert_ne!(original, hash_tree(b.path()).unwrap().content_id());
        std::fs::write(b.path().join("a"), "a").unwrap();
        std::fs::rename(b.path().join("a"), b.path().join("c")).unwrap();
        assert_ne!(original, hash_tree(b.path()).unwrap().content_id());
    }

    #[test]
    fn installer_same_size_rewrite_invalidates_streamed_digest() {
        let root = tempfile::tempdir().unwrap();
        if !observable_modes(root.path()) {
            return;
        }
        let path = root.path().join("file");
        let mut builder = ManifestBuilder::default();
        builder
            .copy(
                "file",
                false,
                &mut &b"old"[..],
                &mut fs::File::create(&path).unwrap(),
            )
            .unwrap();
        builder
            .record_metadata("file", &fs::metadata(&path).unwrap())
            .unwrap();
        fs::write(&path, b"new").unwrap();
        assert_eq!(
            builder.finish(root.path()).unwrap().content_id(),
            hash_tree(root.path()).unwrap().content_id()
        );
    }

    #[test]
    fn directory_paths_and_executable_bits_participate() {
        let root = tempfile::tempdir().unwrap();
        if !observable_modes(root.path()) {
            return;
        }
        fs::create_dir(root.path().join("empty")).unwrap();
        let before = hash_tree(root.path()).unwrap();
        fs::rename(root.path().join("empty"), root.path().join("renamed")).unwrap();
        assert_ne!(
            before.content_id(),
            hash_tree(root.path()).unwrap().content_id()
        );
        assert!(!before.verify_tree(root.path()).unwrap());
        fs::write(root.path().join("file"), b"x").unwrap();
        let before_exec = hash_tree(root.path()).unwrap();
        crate::platform::fs::set_executable(&root.path().join("file")).unwrap();
        assert_ne!(
            before_exec.content_id(),
            hash_tree(root.path()).unwrap().content_id()
        );
        assert!(!before_exec.verify_tree(root.path()).unwrap());
    }

    #[test]
    fn unknown_exec_bits_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        if observable_modes(root.path()) {
            return;
        }
        fs::write(root.path().join("file"), b"x").unwrap();
        assert_eq!(
            hash_tree(root.path()).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let manifest = ContentManifest {
            schema: SCHEMA,
            entries: vec![],
        };
        assert_eq!(
            manifest.verify_tree(root.path()).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
