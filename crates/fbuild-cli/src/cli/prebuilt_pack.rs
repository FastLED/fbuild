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
    DEFAULT_PRODUCT, PackRequest, UnitRef, catalog, pack, release, tool_for,
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
    pub units: String,
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
    // Object names are not portable (they fold the source's own path in), so the
    // publisher never writes them by hand: each unit line names the source and
    // the same `CompilerBase::object_path` the client hydrates to derives the
    // file name, which `pack` then requires to exist in the tree.
    let units = read_units(Path::new(&args.units), objects)?;
    let unit_refs: Vec<UnitRef<'_>> = units
        .iter()
        .map(|(unit, object)| UnitRef { unit, object })
        .collect();
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
        units: &unit_refs,
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
        "manifest: {} names {} compiled unit(s)",
        fbuild_packages_fetch::prebuilt::OBJECTS_MANIFEST,
        packed.units
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

/// Read the unit manifest: one `<unit-id><TAB><source-path>` line per compiled
/// unit, blank lines and `#` comments ignored.
///
/// The object file each unit produced is derived, never written by hand, so the
/// manifest cannot disagree with what a real compile of that tree produced. The
/// objects directory must be the one those objects were compiled into: the name
/// carries a hash of the source's path relative to it.
fn read_units(path: &Path, objects: &Path) -> fbuild_core::Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "prebuilt pack: units {}: {error}",
            path.display()
        ))
    })?;
    let root = fbuild_core::path::NormalizedPath::new(objects);
    let mut units = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((unit, source)) = line.split_once('\t') else {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "prebuilt pack: units {}:{}: expected `<unit-id><TAB><source-path>`",
                path.display(),
                index + 1
            )));
        };
        let object =
            fbuild_build::compiler::CompilerBase::object_path(Path::new(source.trim()), objects);
        let object = fbuild_core::path::NormalizedPath::new(&object)
            .relative_to(&root)
            .map(|relative| relative.display_slash())
            .ok_or_else(|| {
                fbuild_core::FbuildError::BuildFailed(format!(
                    "prebuilt pack: units {}:{}: {source} does not compile into the objects tree",
                    path.display(),
                    index + 1
                ))
            })?;
        units.push((unit.trim().to_string(), object));
    }
    if units.is_empty() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "prebuilt pack: units {}: names no unit",
            path.display()
        )));
    }
    Ok(units)
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
