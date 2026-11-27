//! Exact-fingerprint prebuilt framework resolution (#1882).
//!
//! A `Catalog` shard lists the prebuilt framework archives fbuild may serve
//! for one tool, keyed by the exact local build fingerprint. The reader in
//! [`reader`] is deliberately independent of the Release-only mirror reader in
//! [`crate::downloader::mirror_manifest`](crate): that reader rejects the
//! `Catalog` kind, keeps its descriptors private to tests, and falls back to a
//! stale shard without re-checking it against the current Index descriptor.
//!
//! The v1 rules that decide a hit are narrow on purpose:
//!
//! - a query is an exact identity (`framework`, `arch`, `abi`, `version` and a
//!   64-hex fingerprint) — a channel name, a version range or a fingerprint
//!   *prefix* never authorizes a hit;
//! - the Index descriptor binds the shard's exact byte count **and** SHA-256,
//!   and both are re-checked on every read, cached or freshly fetched;
//! - [`extract`] is the only way an archive becomes a tree: it verifies the
//!   descriptor again while decompressing and never writes outside the root;
//! - [`sweep_staging`] reaps the staging directories a killed process left
//!   behind, and touches nothing that is not one of them;
//! - [`sweep_staging_tree`] is the walk a daemon runs over `{cache}/prebuilt/`,
//!   because a tree is committed wherever its consumer chose to put it;
//! - [`prune`] evicts the least-recently-used tree when the prebuilt root
//!   exceeds the operator's budget, timing recency by marker modification time
//!   and keeping every tree inside the grace period;
//! - a release must name the exact tool, the exact version, a
//!   `min_client_version` this client satisfies, a `none`/`<arch>`/`<abi>`
//!   platform, the `fp-<first 16 hex>` flavor, a `fbuild-fp:<fingerprint>`
//!   `provides` entry and a single well-formed URL list — anything else is a
//!   miss and the caller builds from source.
mod extract;
mod fetch;
mod gc;
mod lru;
mod obtain;
mod reader;
pub use extract::{
    Existing, ExtractError, ExtractedTree, MARKER, MAX_EXTRACTED_BYTES, existing_tree, extract,
    extract_async, extract_with_limit,
};
pub use fetch::{Artifact, FetchError, Fetcher};
pub use gc::{
    GcError, MAX_REMOVALS, MAX_SCAN_DEPTH, MAX_SCAN_DIRS, MIN_AGE, SweepReport, sweep_staging,
    sweep_staging_async, sweep_staging_tree, sweep_staging_tree_async,
};
pub use lru::{
    BUDGET_ENV, EvictReport, LruError, TreeUse, budget_from, declared_budget, prune, prune_async,
    touch, touch_async, trees,
};
pub use obtain::{FP_DIR_PREFIX, ObtainError, Obtained, obtain, tree_root};
pub use reader::Reader;
use serde::{Deserialize, Serialize};

/// Why a query could not be resolved. Never a silent upgrade to a nearby hit.
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
