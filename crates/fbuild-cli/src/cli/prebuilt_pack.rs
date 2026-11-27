//! `fbuild __prebuilt-pack` — the manual prebuilt artifact builder (#1882).
//!
//! The publisher side of exact-fingerprint core-object serving, deliberately
//! hidden: it is a maintainer tool, not a user command. It packs a compiled
//! core-object tree into the decision-2 key, emits the `Catalog` shard entry the
//! client reads, and stops there. Decision 8c makes every bucket write a
//! maintainer action, so this command has no upload path at all — the receipt it
//! prints names the key and the digest and says so.
//!
//! The fingerprint is an input, not a guess: it comes from
//! `Esp32Framework::prebuilt_core_plan` for the tree being published. A packer
//! that recomputed one from a different tree would mint a key nothing serves.
use std::path::Path;

use fbuild_packages_fetch::prebuilt::{
    DEFAULT_PRODUCT, PackRequest, catalog, pack, release, tool_for,
};

use crate::output;

/// Arguments of the hidden `__prebuilt-pack` command.
pub struct PackArgs {
    pub objects: String,
    pub out: String,
    pub vendor: String,
    pub product: Option<String>,
    pub channel: String,
    pub reference: String,
    pub chip: String,
    pub framework: String,
    pub fingerprint: String,
    pub arch: String,
    pub abi: String,
    pub base_url: String,
    pub min_client_version: u32,
}

/// Pack one objects tree and write the entry that describes it.
pub fn run_prebuilt_pack(args: PackArgs) -> fbuild_core::Result<()> {
    let objects = Path::new(&args.objects);
    let out = Path::new(&args.out);
    let product = args.product.as_deref().unwrap_or(DEFAULT_PRODUCT);
    let request = PackRequest {
        objects,
        out,
        vendor: &args.vendor,
        product,
        channel: &args.channel,
        reference: &args.reference,
        chip: &args.chip,
        framework: &args.framework,
        fingerprint: &args.fingerprint,
        arch: &args.arch,
        abi: &args.abi,
        base_url: &args.base_url,
        min_client_version: args.min_client_version,
    };
    let packed = pack(&request).map_err(|error| {
        fbuild_core::FbuildError::BuildFailed(format!("prebuilt pack: {error}"))
    })?;
    let entry = release(&request, &packed);
    let tool = tool_for(&args.framework);
    let shard = catalog(&tool, vec![entry.clone()]);
    write_json(&out.join("release.json"), &entry)?;
    write_json(&out.join(format!("{tool}.catalog.json")), &shard)?;

    output::result(format!(
        "packed {} entries into {}",
        packed.entries,
        packed.archive.display_slash()
    ));
    output::result(format!(
        "key: {} ({} bytes, sha256 {})",
        packed.key, packed.size_bytes, packed.sha256
    ));
    output::result(format!(
        "entry: {} and {}",
        out.join("release.json").display(),
        out.join(format!("{tool}.catalog.json")).display()
    ));
    output::result(
        "nothing was uploaded: publishing is a maintainer action (decision 8c, FastLED/fbuild#1882)",
    );
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> fbuild_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        fbuild_core::FbuildError::BuildFailed(format!("prebuilt pack: {error}"))
    })?;
    std::fs::write(path, bytes).map_err(|error| {
        fbuild_core::FbuildError::BuildFailed(format!("prebuilt pack: {}: {error}", path.display()))
    })
}

#[cfg(test)]
#[path = "prebuilt_pack_tests.rs"]
mod tests;
