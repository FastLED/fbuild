//! Installed-package anchor for the shared portable tree manifest.
use crate::PackageInfo;
use fbuild_core::content_manifest::{
    ContentId, ContentManifest, Entry, SCHEMA, SIDECAR, hash_tree,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Serialize, Deserialize)]
struct Package {
    name: String,
    version: String,
    url: String,
    download_checksum: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Sidecar {
    schema: u32,
    package: Package,
    entries: Vec<Entry>,
    content_id: ContentId,
}

pub fn write_manifest(
    root: &Path,
    info: &PackageInfo,
    manifest: ContentManifest,
) -> std::io::Result<()> {
    let sidecar = Sidecar {
        schema: SCHEMA,
        package: Package {
            name: info.name.clone(),
            version: info.version.clone(),
            url: info.url.clone(),
            download_checksum: info.checksum.clone(),
        },
        content_id: manifest.content_id(),
        entries: manifest.entries,
    };
    // A temporary file also makes asynchronous backfill atomic to readers.
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut temporary, &sidecar)?;
    temporary.persist(root.join(SIDECAR)).map_err(|e| e.error)?;
    Ok(())
}

/// Verify metadata without hashing regular-file contents on the build path.
pub fn load_verified_content_id(root: &Path, expect: &PackageInfo) -> Option<ContentId> {
    match verify(root, expect) {
        Ok(id) => Some(id),
        Err(reason) => {
            tracing::debug!(package = %expect.name, %reason, "content manifest rejected");
            if !root.join(SIDECAR).exists() {
                let _ = queue_backfill(root, expect);
            }
            None
        }
    }
}

fn verify(root: &Path, expect: &PackageInfo) -> Result<ContentId, String> {
    let sidecar: Sidecar =
        serde_json::from_slice(&fs::read(root.join(SIDECAR)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if sidecar.schema != SCHEMA
        || sidecar.package.name != expect.name
        || sidecar.package.version != expect.version
        || (expect.checksum.is_some() && sidecar.package.download_checksum != expect.checksum)
    {
        return Err("schema or resolved package mismatch".into());
    }
    let manifest = ContentManifest {
        schema: sidecar.schema,
        entries: sidecar.entries,
    };
    if manifest.content_id() != sidecar.content_id {
        return Err("content id mismatch".into());
    }
    if !manifest.verify_tree(root).map_err(|e| e.to_string())? {
        return Err("installed path set, size, exec bits or symlink target changed".into());
    }
    Ok(sidecar.content_id)
}

/// Backfill only completed legacy installs, deduplicated for this process.
fn queue_backfill(root: &Path, expect: &PackageInfo) -> Option<std::thread::JoinHandle<()>> {
    if !root.join(".install_complete").is_file() {
        return None;
    }
    static PENDING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let pending = PENDING.get_or_init(Default::default);
    let Ok(mut set) = pending.lock() else {
        return None;
    };
    if !set.insert(root.into()) {
        return None;
    }
    drop(set);
    let root = root.to_path_buf();
    let cleanup_root = root.clone();
    let info = expect.clone();
    match std::thread::Builder::new()
        .name("package-manifest-backfill".into())
        .spawn(move || {
            let result =
                hash_tree(&root).and_then(|manifest| write_manifest(&root, &info, manifest));
            if let Err(error) = result {
                tracing::debug!(%error, "content manifest backfill failed");
            }
            if let Ok(mut set) = pending.lock() {
                set.remove(&root);
            }
        }) {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::debug!(%error, "could not start content manifest backfill");
            if let Ok(mut set) = pending.lock() {
                set.remove(&cleanup_root);
            }
            None
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn info(root: &Path) -> PackageInfo {
        PackageInfo {
            name: "framework".into(),
            version: "1".into(),
            url: "https://example.invalid/core.zip".into(),
            install_path: root.into(),
            checksum: Some("a".repeat(64)),
            content_id: None,
            installed_bytes: None,
        }
    }
    #[test]
    fn resolved_package_and_metadata_must_match() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"abc").unwrap();
        let info = info(root.path());
        write_manifest(root.path(), &info, hash_tree(root.path()).unwrap()).unwrap();
        assert!(load_verified_content_id(root.path(), &info).is_some());
        for field in ["name", "version", "checksum"] {
            let mut wrong = info.clone();
            match field {
                "name" => wrong.name.push('x'),
                "version" => wrong.version.push('x'),
                _ => wrong.checksum = Some("b".repeat(64)),
            }
            assert!(
                load_verified_content_id(root.path(), &wrong).is_none(),
                "{field}"
            );
        }
        fs::write(root.path().join("file"), b"resized").unwrap();
        assert!(load_verified_content_id(root.path(), &info).is_none());
    }
    #[test]
    fn missing_sidecar_then_backfill_then_verified() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"abc").unwrap();
        let info = info(root.path());
        assert!(load_verified_content_id(root.path(), &info).is_none());
        fs::write(root.path().join(".install_complete"), b"").unwrap();
        queue_backfill(root.path(), &info).unwrap().join().unwrap();
        assert!(load_verified_content_id(root.path(), &info).is_some());
    }
}
