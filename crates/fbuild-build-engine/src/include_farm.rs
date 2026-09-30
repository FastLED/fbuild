//! Collapse a framework's block of `-I` directories into one merged directory.
//!
//! GCC resolves every `#include` by probing each `-I` directory in order, so a
//! toolchain header such as `<vector>` first fails one `open` per framework
//! directory. On ESP32-S3 that is ~31k failed opens per translation unit and
//! about a third of all compile CPU (FastLED/fbuild#1537). A header farm is a
//! single directory whose entries symlink into the original directories, so a
//! lookup costs one probe instead of ~200.
//!
//! The farm resolves every include to the same file the original list did:
//!
//! * Only header paths held by exactly one directory are farmed. A directory
//!   holding a duplicated path keeps its own `-I`, in its original order, right
//!   after the farm, so first-match-wins and `#include_next` are unchanged.
//! * A subtree owned by one directory is a single directory symlink, so quoted
//!   and `..`-relative includes inside it resolve physically, exactly as before.
//!   Only directories that several sources contribute to are real. A header in
//!   one of those whose quoted include would resolve differently there keeps
//!   its directory out of the farm.
//!
//! Anything unexpected yields no farm; callers then keep the original list.
//! Callers skip farms on Windows, where symlinks usually need elevated rights,
//! and when `FBUILD_INCLUDE_FARM=0`. The text-level checks above cannot see
//! macro includes; the real-SDK parity test in
//! `fbuild-build/tests/env_isolated/esp32_include_farm_parity.rs` compares GCC's own
//! depfiles with and without the farm (FastLED/fbuild#1588).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Component, Path};

use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};

/// Bumped whenever the farm layout or its planning rules change.
const FORMAT_VERSION: u32 = 4;
const MANIFEST: &str = "farm.json";

/// A materialized farm plus the block directories that stay separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeFarm {
    /// The merged directory that replaces the farmed block entries.
    pub dir: NormalizedPath,
    /// Block directories kept as their own `-I`, in original order, after `dir`.
    pub kept: Vec<NormalizedPath>,
}

impl IncludeFarm {
    /// The `-I` directories that replace the block, in search order.
    pub fn replacement(&self) -> Vec<NormalizedPath> {
        std::iter::once(self.dir.clone())
            .chain(self.kept.iter().cloned())
            .collect()
    }

    /// Maps the farm's prefix out of `__FILE__`, so headers reached through it
    /// do not embed the cache path in flash.
    pub fn macro_prefix_map(&self, label: &str) -> String {
        format!("-fmacro-prefix-map={}={label}", self.dir.display())
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    before: Vec<String>,
    block: Vec<String>,
    kept: Vec<String>,
}

/// Return the farm for `block`, creating it under `farms_root` on first use.
///
/// `before` lists the `-I` directories that precede the block; they can shadow
/// a header a farmed file would otherwise find beside itself, so they take part
/// in the equivalence check and in the farm's key. The farm directory is a pure
/// function of both lists, which keeps depfiles and cache keys that name it
/// valid when it is deleted and rebuilt.
pub fn ensure_farm(
    farms_root: &Path,
    before: &[NormalizedPath],
    block: &[NormalizedPath],
) -> io::Result<IncludeFarm> {
    let before_s = path_strings(before)?;
    let block_s = path_strings(block)?;
    let dir = NormalizedPath::new(farms_root.join(farm_key(&before_s, &block_s)));
    if let Some(farm) = read_manifest(dir.as_path(), &before_s, &block_s) {
        return Ok(farm);
    }
    let plan = plan_farm(before, block)?;
    std::fs::create_dir_all(farms_root)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = NormalizedPath::new(farms_root.join(format!(
        "tmp-{}-{}-{nanos}",
        dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        std::process::id()
    )));
    let built = materialize(&plan, staging.as_path()).and_then(|()| {
        let manifest = Manifest {
            version: FORMAT_VERSION,
            before: before_s.clone(),
            block: block_s.clone(),
            kept: plan.kept.iter().map(|&i| block_s[i].clone()).collect(),
        };
        std::fs::write(
            staging.join(MANIFEST).as_path(),
            serde_json::to_vec(&manifest).map_err(io::Error::other)?,
        )
    });
    if let Err(error) = built {
        let _ = std::fs::remove_dir_all(staging.as_path());
        return Err(error);
    }
    if std::fs::rename(staging.as_path(), dir.as_path()).is_err() {
        // Another build published the same farm first; theirs is identical.
        let _ = std::fs::remove_dir_all(staging.as_path());
    }
    read_manifest(dir.as_path(), &before_s, &block_s)
        .ok_or_else(|| io::Error::other(format!("include farm {} is unreadable", dir.display())))
}

fn path_strings(dirs: &[NormalizedPath]) -> io::Result<Vec<String>> {
    dirs.iter()
        .map(|d| {
            d.to_str().map(str::to_string).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 include directory")
            })
        })
        .collect()
}

fn farm_key(before: &[String], block: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(format!("include-farm-v{FORMAT_VERSION}\n").as_bytes());
    for dir in before {
        hasher.update(dir.as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(b"--\n");
    for dir in block {
        hasher.update(dir.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex()[..32].to_string()
}

fn read_manifest(dir: &Path, before: &[String], block: &[String]) -> Option<IncludeFarm> {
    let bytes = std::fs::read(dir.join(MANIFEST)).ok()?;
    let manifest: Manifest = serde_json::from_slice(&bytes).ok()?;
    (manifest.version == FORMAT_VERSION && manifest.before == before && manifest.block == block)
        .then(|| IncludeFarm {
            dir: NormalizedPath::new(dir),
            kept: manifest.kept.iter().map(NormalizedPath::from).collect(),
        })
}

/// What to build: symlinks (farm-relative path, target) and the block indices
/// that stay as their own `-I`.
#[derive(Debug)]
struct Plan {
    links: Vec<(String, NormalizedPath)>,
    real_dirs: Vec<String>,
    kept: Vec<usize>,
}

#[derive(Default, Debug)]
struct Node {
    children: BTreeMap<String, Node>,
    /// Owning block index when this node is a file.
    file: Option<usize>,
    /// The single block index every file below this node comes from, if any.
    owner: Option<usize>,
}

impl Node {
    fn sole_owner(&self) -> Option<usize> {
        self.owner
    }

    fn annotate(&mut self) -> Option<usize> {
        self.owner = match self.file {
            Some(owner) => Some(owner),
            None => {
                let mut owners = self.children.values_mut().map(Node::annotate);
                let first = owners.next().flatten();
                // Drain the iterator so every child is annotated.
                let all_same = owners.fold(first.is_some(), |same, o| same && o == first);
                first.filter(|_| all_same)
            }
        };
        self.owner
    }
}

/// Every block and `before` directory's files, for lookups without stats.
struct Index<'a> {
    before: &'a [NormalizedPath],
    before_files: Vec<std::collections::HashSet<String>>,
    block: &'a [NormalizedPath],
    block_files: Vec<std::collections::HashSet<&'a str>>,
}

impl Index<'_> {
    /// `dir/rel`, as a path string, if that file exists.
    fn file_in(&self, dir: &Path, has: impl Fn(&str) -> bool, rel: &str) -> Option<String> {
        match normalize_rel(rel) {
            Some(rel) => has(&rel).then(|| format!("{}/{rel}", dir.display())),
            // Escapes the directory: rare, so ask the filesystem.
            None => lexical_join(dir, rel).filter(|p| Path::new(p).is_file()),
        }
    }

    fn in_block(&self, index: usize, rel: &str) -> Option<String> {
        self.file_in(
            self.block[index].as_path(),
            |r| self.block_files[index].contains(r),
            rel,
        )
    }

    fn in_before(&self, index: usize, rel: &str) -> Option<String> {
        self.file_in(
            self.before[index].as_path(),
            |r| self.before_files[index].contains(r),
            rel,
        )
    }
}

/// `rel` with `.`/`..` applied, or `None` if it climbs out of its root.
fn normalize_rel(rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn plan_farm(before: &[NormalizedPath], block: &[NormalizedPath]) -> io::Result<Plan> {
    let files: Vec<Vec<String>> = block
        .iter()
        .map(|dir| list_files(dir.as_path()))
        .collect::<io::Result<_>>()?;
    let mut farmed = vec![true; block.len()];
    let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, rels) in files.iter().enumerate() {
        for rel in rels {
            owners.entry(rel.as_str()).or_default().push(index);
        }
    }
    for dirs in owners.values().filter(|dirs| dirs.len() > 1) {
        for &index in dirs {
            farmed[index] = false;
        }
    }
    // `#include_next` continues after the directory a header was found in,
    // but GCC restarts it when the header was found beside a quoted includer.
    // In a merged farm directory that can bring the header back to itself
    // (and `#pragma once` then drops the next one), so a directory holding any
    // `#include_next` header keeps its own `-I` (FastLED/fbuild#1566).
    for (index, rels) in files.iter().enumerate() {
        if farmed[index]
            && rels
                .iter()
                .any(|rel| uses_include_next(&block[index].join(rel)))
        {
            farmed[index] = false;
        }
    }
    // A path that is a file in one directory and a directory in another cannot
    // be merged; keep both directories out.
    for rel in owners.keys() {
        let mut prefix = String::new();
        for part in rel.split('/').take(rel.split('/').count() - 1) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(file_owners) = owners.get(prefix.as_str()) {
                for &index in file_owners.iter().chain(&owners[rel]) {
                    farmed[index] = false;
                }
            }
        }
    }
    let index = Index {
        before,
        before_files: before
            .iter()
            .map(|dir| list_files(dir.as_path()).map(|rels| rels.into_iter().collect()))
            .collect::<io::Result<_>>()?,
        block,
        block_files: files
            .iter()
            .map(|rels| rels.iter().map(String::as_str).collect())
            .collect(),
    };
    loop {
        let mut root = build_tree(&files, &farmed);
        root.annotate();
        let conflicts = quoted_include_conflicts(&root, &index, &files, &farmed);
        if conflicts.is_empty() {
            let mut plan = Plan {
                links: Vec::new(),
                real_dirs: Vec::new(),
                kept: (0..block.len()).filter(|&i| !farmed[i]).collect(),
            };
            collect_links(&root, "", block, &mut plan);
            return Ok(plan);
        }
        for index in conflicts {
            farmed[index] = false;
        }
    }
}

fn list_files(dir: &Path) -> io::Result<Vec<String>> {
    let mut rels = Vec::new();
    // GCC ignores a missing `-I`; it contributes nothing to the farm.
    if dir.is_dir() {
        collect_files(dir, "", 0, &mut rels)?;
    }
    Ok(rels)
}

/// Relative (`/`-joined) paths of the regular files under `dir`, following
/// symlinks as `open` does. Dangling links are skipped, as GCC skips them.
fn collect_files(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other(format!(
            "include tree too deep (symlink loop?) at {}",
            dir.display()
        )));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 header path"))?;
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            collect_files(&entry.path(), &rel, depth + 1, out)?;
        } else if meta.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

fn build_tree(files: &[Vec<String>], farmed: &[bool]) -> Node {
    let mut root = Node::default();
    for (index, rels) in files.iter().enumerate() {
        if !farmed[index] {
            continue;
        }
        for rel in rels {
            let mut node = &mut root;
            for part in rel.split('/') {
                node = node.children.entry(part.to_string()).or_default();
            }
            node.file = Some(index);
        }
    }
    root
}

fn collect_links(node: &Node, rel: &str, block: &[NormalizedPath], plan: &mut Plan) {
    for (name, child) in &node.children {
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        match child.sole_owner() {
            Some(owner) => plan
                .links
                .push((child_rel.clone(), block[owner].join(&child_rel))),
            None => {
                plan.real_dirs.push(child_rel.clone());
                collect_links(child, &child_rel, block, plan);
            }
        }
    }
}

/// Block indices whose farmed headers would resolve a quoted include to a
/// different file from inside a merged farm directory.
fn quoted_include_conflicts(
    root: &Node,
    index: &Index<'_>,
    files: &[Vec<String>],
    farmed: &[bool],
) -> Vec<usize> {
    let block = index.block;
    let mut conflicts = Vec::new();
    for (owner, rels) in files.iter().enumerate() {
        if !farmed[owner] {
            continue;
        }
        let conflicting = rels.iter().any(|rel| {
            // `<../x.h>` resolves through the -I chain, where a `..` from the
            // farm root climbs out of the farm instead of to a sibling dir.
            let file = block[owner].join(rel);
            if angle_includes_climbing(&file).iter().any(|name| {
                chain_lookup(index, name) != farm_chain_lookup(root, index, farmed, name)
            }) {
                return true;
            }
            let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
            // Inside a symlinked subtree, lookups resolve physically in the
            // owning directory, exactly as before.
            if !in_merged_dir(root, parent) {
                return false;
            }
            quoted_includes(&block[owner].join(rel)).iter().any(|name| {
                // A miss beside the file falls through to the `-I` chain, which
                // after farming searches the farm in place of the farmed dirs.
                let chain = || chain_lookup(index, name);
                let farm_chain = || farm_chain_lookup(root, index, farmed, name);
                // A top-level header has no parent; a leading `/` would make the
                // include absolute and hide a `..` that climbs out of the farm.
                let beside = if parent.is_empty() {
                    name.clone()
                } else {
                    format!("{parent}/{name}")
                };
                let original = index.in_block(owner, &beside).or_else(chain);
                let farm = beside_farm(root, index, &beside).or_else(farm_chain);
                original != farm
            })
        });
        if conflicting {
            conflicts.push(owner);
        }
    }
    conflicts
}

/// Whether the farm directory at `rel` is real (merged) rather than a symlink.
fn in_merged_dir(root: &Node, rel: &str) -> bool {
    let mut node = root;
    if rel.is_empty() {
        return true;
    }
    for part in rel.split('/') {
        match node.children.get(part) {
            Some(child) if child.sole_owner().is_none() => node = child,
            _ => return false,
        }
    }
    true
}

fn uses_include_next(file: &Path) -> bool {
    std::fs::read(file).is_ok_and(|bytes| {
        String::from_utf8_lossy(&bytes).lines().any(|line| {
            line.trim_start()
                .strip_prefix('#')
                .is_some_and(|rest| rest.trim_start().starts_with("include_next"))
        })
    })
}

/// `#include <...>` names containing a `..` component.
fn angle_includes_climbing(file: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix('#')?.trim_start();
            let rest = rest.strip_prefix("include")?.trim_start();
            let (name, _) = rest.strip_prefix('<')?.split_once('>')?;
            name.split('/')
                .any(|part| part == "..")
                .then(|| name.to_string())
        })
        .collect()
}

fn quoted_includes(file: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix('#')?.trim_start();
            let rest = rest.strip_prefix("include")?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let (name, _) = rest.split_once('"')?;
            (!name.is_empty() && !Path::new(name).is_absolute()).then(|| name.to_string())
        })
        .collect()
}

/// The first directory before or inside the block that holds `name`.
fn chain_lookup(index: &Index<'_>, name: &str) -> Option<String> {
    (0..index.before.len())
        .find_map(|i| index.in_before(i, name))
        .or_else(|| (0..index.block.len()).find_map(|i| index.in_block(i, name)))
}

/// `name` looked up through the post-farm `-I` chain: `before`, the farm,
/// then the block directories kept out of it.
fn farm_chain_lookup(
    root: &Node,
    index: &Index<'_>,
    farmed: &[bool],
    name: &str,
) -> Option<String> {
    (0..index.before.len())
        .find_map(|i| index.in_before(i, name))
        .or_else(|| beside_farm(root, index, name))
        .or_else(|| {
            (0..index.block.len())
                .filter(|&i| !farmed[i])
                .find_map(|i| index.in_block(i, name))
        })
}

/// `beside` (the including file's directory joined with the include name)
/// looked up in the farm, where that directory is merged.
fn beside_farm(root: &Node, index: &Index<'_>, beside: &str) -> Option<String> {
    let mut stack: Vec<&Node> = vec![root];
    let mut rel: Vec<&str> = Vec::new();
    let parts: Vec<&str> = beside
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let node = *stack.last()?;
        if *part == ".." {
            stack.pop();
            rel.pop();
            if stack.is_empty() {
                // Escapes the farm root into the cache directory.
                return None;
            }
            continue;
        }
        let child = node.children.get(*part)?;
        rel.push(part);
        if let Some(owner) = child.sole_owner() {
            // A symlink: the remainder resolves physically in the owner.
            let physical = format!("{}/{}", rel.join("/"), parts[i + 1..].join("/"));
            return index.in_block(owner, &physical);
        }
        stack.push(child);
    }
    None
}

/// `base` joined with `rel`, `..` applied lexically; `None` if not UTF-8.
fn lexical_join(base: &Path, rel: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in base.join(rel).components() {
        match component {
            Component::Prefix(p) => prefix = p.as_os_str().to_str()?.to_string(),
            Component::RootDir => prefix.push('/'),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part.to_str()?.to_string()),
        }
    }
    Some(format!("{prefix}{}", out.join("/")))
}

fn materialize(plan: &Plan, staging: &Path) -> io::Result<()> {
    std::fs::create_dir_all(staging)?;
    for dir in &plan.real_dirs {
        std::fs::create_dir_all(staging.join(dir))?;
    }
    for (rel, target) in &plan.links {
        let link = staging.join(rel);
        if target.as_path().is_dir() {
            fbuild_core::platform::fs::symlink_dir(target.as_path(), &link)?;
        } else {
            fbuild_core::platform::fs::symlink_file(target.as_path(), &link)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "include_farm_tests.rs"]
mod tests;
