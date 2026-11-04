//! Strict archive extraction: regular files only, no unlisted or duplicate paths.
use super::{sha256, valid_sha};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Write},
    path::{Component, Path},
};

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub fp: String,
    pub abi_version: u32,
    pub inputs: serde_json::Value,
    pub builder_version: String,
    pub fbuild_version: String,
    pub source: serde_json::Value,
    pub files: BTreeMap<String, String>,
}

fn checked_path(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
        && matches!(path.components().next(),Some(Component::Normal(p)) if p=="obj" || p=="lib" || p=="include")
}

/// Extract into a fresh private staging directory owned by the daemon.
pub fn extract(
    reader: impl Read,
    stage: &Path,
    fp: &str,
    write_limit: Option<u64>,
) -> Result<(), String> {
    if std::fs::read_dir(stage)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("staging directory must be empty".into());
    }
    let decoder = zstd::stream::read::Decoder::new(reader).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().map_err(|e| e.to_string())?;
    let mut first = entries
        .next()
        .ok_or("missing first member")?
        .map_err(|e| e.to_string())?;
    if first.path().map_err(|e| e.to_string())?.as_ref() != Path::new("fbuild-prebuilt.json")
        || !first.header().entry_type().is_file()
        || first.size() > 8 * 1024 * 1024
    {
        return Err("invalid first member".into());
    }
    let mut bytes = Vec::new();
    first.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&manifest, fp)?;
    let mut seen = HashSet::new();
    let mut written = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let name = path.to_str().ok_or("non-UTF8 path")?.to_owned();
        if !checked_path(&path)
            || !entry.header().entry_type().is_file()
            || !seen.insert(name.clone())
        {
            return Err("unsafe, nonregular, or duplicate member".into());
        }
        let expected = manifest.files.get(&name).ok_or("unlisted file")?;
        let target = stage.join(path);
        std::fs::create_dir_all(target.parent().ok_or("invalid parent")?)
            .map_err(|e| e.to_string())?;
        // The stage is private. Check filesystem aliases as well as raw tar names
        // before creating a member, without using a lock-file primitive.
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("member destination already exists".into()),
            Err(error) => return Err(error.to_string()),
        }
        let mut file = std::fs::File::create(target).map_err(|e| e.to_string())?;
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            written = written.saturating_add(n as u64);
            if write_limit.is_some_and(|limit| written > limit) {
                return Err(std::io::Error::from(std::io::ErrorKind::StorageFull).to_string());
            }
            file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        if format!("{:x}", digest.finalize()) != *expected {
            return Err("member sha256 mismatch".into());
        }
    }
    if seen.len() != manifest.files.len() {
        return Err("missing archive members".into());
    }
    // Consume the zstd trailer as well: a complete tar can precede a truncated zstd frame.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink()).map_err(|e| e.to_string())?;
    std::fs::write(stage.join("fbuild-prebuilt.json"), bytes).map_err(|e| e.to_string())?;
    Ok(())
}

fn validate(manifest: &Manifest, fp: &str) -> Result<(), String> {
    if manifest.fp != fp || !valid_sha(fp) || manifest.abi_version != 1 {
        return Err("fingerprint or ABI mismatch".into());
    }
    let canonical =
        super::canonical_json(&manifest.inputs).ok_or("invalid canonical fingerprint inputs")?;
    if sha256(&canonical) != fp {
        return Err("canonical fingerprint inputs do not match local fingerprint".into());
    }
    if manifest.files.is_empty()
        || manifest
            .files
            .iter()
            .any(|(p, s)| !checked_path(Path::new(p)) || !valid_sha(s))
    {
        return Err("invalid file manifest".into());
    }
    Ok(())
}

pub fn verify_entry(path: &Path, fp: &str) -> Result<(), String> {
    let bytes = std::fs::read(path.join("fbuild-prebuilt.json")).map_err(|e| e.to_string())?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&manifest, fp)?;
    for (name, expected) in manifest.files {
        let target = path.join(name);
        if std::fs::symlink_metadata(&target)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
            || sha256(&std::fs::read(target).map_err(|e| e.to_string())?) != expected
        {
            return Err("cached member checksum mismatch".into());
        }
    }
    Ok(())
}
