//! Exact-fingerprint prebuilt framework resolution.
mod reader;
pub use reader::Reader;
use serde::{Deserialize, Serialize};

pub use crate::PrebuiltOutcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissReason(pub String);
impl std::fmt::Display for MissReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Exact local identity; channels and fingerprint prefixes never authorize a hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub framework: String,
    pub arch: String,
    pub abi: String,
    pub version: String,
    pub fp: String,
}

#[derive(Debug, Clone)]
pub enum Resolution {
    Hit(Asset),
    Miss(MissReason),
}

/// Pilot v1 accepts only single-stream assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub urls: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub parts: Vec<serde_json::Value>,
}

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// CLI overrides the kill switch; experiment remains disabled by default.
pub fn enabled(
    cli_on: bool,
    cli_off: bool,
    no_prebuilt: Option<&str>,
    experiment: Option<&str>,
) -> bool {
    if cli_off {
        return false;
    }
    if cli_on {
        return true;
    }
    if truthy(no_prebuilt) {
        return false;
    }
    truthy(experiment)
}

// The v1 contract deliberately treats invalid values as on; reuse the
// canonical parser for recognized spellings and override only its None case.
fn truthy(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    fbuild_core::env_flag::parse(value).unwrap_or_else(|| {
        tracing::warn!("invalid prebuilt env value {value:?}; treating as on");
        true
    })
}

#[cfg(test)]
mod tests;

/// Explicit operator mirrors precede published mirrors by default. An empty
/// override disables published mirrors; prebuilt archives have no vendor URL.
pub fn operator_urls(
    asset: &Asset,
    query: &Query,
    overrides: Option<&str>,
    mirror_first: bool,
) -> Vec<String> {
    let Some(value) = overrides else {
        return asset.urls.clone();
    };
    if value.trim().is_empty() {
        return Vec::new();
    }
    let Some(prefix) = query.fp.get(..16) else {
        return Vec::new();
    };
    let path = format!(
        "prebuilt/{}/{}/{}/fp-{}/{}",
        query.framework, query.version, query.arch, prefix, asset.filename
    );
    let mut operators: Vec<_> = value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|base| {
            if base.contains("{path}") || base.contains("{filename}") {
                base.replace("{path}", &path)
                    .replace("{filename}", &asset.filename)
            } else {
                format!("{}/{}", base.trim_end_matches('/'), asset.filename)
            }
        })
        .collect();
    let mut published = asset.urls.clone();
    if mirror_first {
        operators.extend(published);
        operators
    } else {
        if published.is_empty() {
            return operators;
        }
        let first = published.remove(0);
        operators.extend(published);
        operators.insert(0, first);
        operators
    }
}

pub mod archive;
pub mod gc;
pub mod partial;

/// Reuse the package cache's symlink-safe size walker for LRU accounting.
pub fn installed_size(path: &std::path::Path) -> u64 {
    crate::dir_size(path)
}

/// Canonical JSON independent of serde_json's optional preserve_order feature.
pub fn canonical_json(value: &serde_json::Value) -> Option<Vec<u8>> {
    fn ordered(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> = map
                    .iter()
                    .map(|(key, value)| (key.clone(), ordered(value)))
                    .collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(ordered).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_vec(&ordered(value)).ok()
}
