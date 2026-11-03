//! Canonical framework build identity, re-exported by the fbuild-build facade.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const PREBUILT_ABI_VERSION: u32 = 1;
pub type Fp = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
    pub sha256: String,
}

/// Ordered translation-unit argv from the compiler, including define/include order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameworkBuildPlan {
    pub abi_version: u32,
    pub toolchain: PackageIdentity,
    pub framework: PackageIdentity,
    pub mcu: String,
    pub variant_dir: String,
    pub argv: Vec<Vec<String>>,
    pub config_sha256: BTreeMap<String, String>,
    pub path_map_prefix: String,
}

pub fn fingerprint(plan: &FrameworkBuildPlan) -> Fp {
    serde_json::to_value(plan)
        .ok()
        .and_then(|value| fbuild_packages::prebuilt::canonical_json(&value))
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .unwrap_or_default()
}

pub use crate::prebuilt_paths::{PathMapping, canonical_argv, mapped_path, unambiguous_roots};
