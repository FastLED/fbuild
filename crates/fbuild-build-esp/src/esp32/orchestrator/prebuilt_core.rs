//! The consumer of the exact-fingerprint prebuilt ladder (#1882, #2196 cell 3).
//!
//! `#1882` delivered a serving ladder — identity, `prebuilt::obtain`/`tree_root`,
//! `prebuilt::serve`, the LRU, staging GC, `prebuilt::pack` — and none of it was
//! reachable from a build. This module is the call site the ladder was built
//! for: it turns an installed, stamped framework into one [`Query`], asks the
//! ladder for the tree, and — only on a hit — hydrates the compiled core objects
//! into the project's core build directory, where the compiler's ordinary
//! incremental check then finds them up to date and compiles nothing.
//!
//! Three rules from `docs/design/artifact-serving-decisions.md` §8 are load
//! bearing here:
//!
//! - **Opt-in only.** §8b makes core objects a hidden research opt-in
//!   ([`PrebuiltPolicy::serves_core_objects`]); the default build never touches
//!   this module's network path.
//! - **A failure is never a build failure.** Every rung — no identity, a
//!   catalog miss, a transfer error, a hash mismatch, a foreign marker, a
//!   manifest that does not cover this tree — ends in exactly one notice line
//!   and a source build. Nothing here returns `Err` to the build.
//! - **No identity, no serving.** A framework with no stamps yields no
//!   fingerprint, and the module says nothing: that is the ordinary legacy tree
//!   every install made before §8a landed looks like.
//!
//! ## The unit contract
//!
//! A tree is only usable if hydration can name the object file the compiler
//! expects *on this host*. Object names are not portable — [`CompilerBase::object_path`]
//! derives its disambiguating suffix from the source's absolute path, which
//! contains the user's home directory. So a packed tree carries
//! `objects.json`:
//!
//! ```json
//! { "schema": 1, "units": { "cores/esp32/Arduino.cpp": "Arduino_3f.cpp.o" } }
//! ```
//!
//! keyed by the **canonical unit id** this module computes (the source path
//! relative to the framework root, `/`-separated). Hydration resolves each id
//! to the local source, computes this host's object path, and copies the
//! archive's entry there. A tree without a manifest, or with one that names a
//! unit this build does not compile, is not served.
//!
//! ## Fingerprint agreement
//!
//! The publisher and the consumer must hash the same thing. This module pins the
//! client side of that contract, documented next to the code that builds it so
//! the packer side can match it field for field: the config set
//! ([`config_digests`]), the canonical argv ([`canonical_argv`]), and the exact
//! [`Query`] — `framework = arduino-esp32`, `version = <core version>`,
//! `arch = <mcu>`, `abi = <sdk variant>`, `fp = <fingerprint>`. A packer whose
//! `--arch`/`--abi` differ from the chip and SDK variant publishes an entry no
//! build ever asks for.

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_packages::library::esp32_framework::Esp32Framework;
use fbuild_packages::library::esp32_framework::prebuilt_core::{
    CompileUnit, ConfigDigest, CoreBuildInputs, PrebuiltCorePlan,
};
use fbuild_packages_fetch::prebuilt::{
    Asset, Fetcher, Obtained, Query, Reader, Resolution, Served, serve,
};
use serde::Deserialize;

/// The framework family a prebuilt core object is published under. The packer's
/// `--framework` must be this value: [`Query::framework`] is part of the exact
/// identity, never a channel.
pub(super) const FRAMEWORK: &str = "arduino-esp32";

/// The manifest a usable tree carries, naming each compiled unit's object file.
pub(super) const OBJECTS_MANIFEST: &str = "objects.json";

/// Schema of [`OBJECTS_MANIFEST`]; a different value is a tree from a newer
/// publisher and is refused rather than guessed at.
const MANIFEST_SCHEMA: u32 = 1;

#[derive(Deserialize)]
struct ObjectsManifest {
    schema: u32,
    units: std::collections::BTreeMap<String, String>,
}

/// What one consult produced, for the caller to log exactly once and to
/// record in `build_info.json` (#2196 cell 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outcome {
    /// The honest line to print, if this build compiled from source.
    pub notice: Option<String>,
    /// How many core objects a served tree hydrated; `0` for a source build.
    pub hydrated: usize,
    /// The identity that was asked for, when the framework had one.
    pub fingerprint: Option<String>,
    /// Where the served artifact came from (a catalog URL or its filename).
    pub served_from: Option<String>,
    /// The served artifact's identity key, when the ladder served a tree.
    pub key: Option<String>,
    /// SHA-256 of the served archive, when the ladder served a tree.
    pub sha256: Option<String>,
    /// Size in bytes of the published archive behind a served tree; `None` for
    /// a source build (decision 6's `bytes_downloaded`, per arm).
    pub archive_bytes: Option<u64>,
    /// Whether this build transferred that archive rather than reusing a tree
    /// already in the local cache; `None` for a source build.
    pub downloaded: Option<bool>,
}

impl Outcome {
    /// The lines this consult contributes to the build log: exactly one for a
    /// source build, exactly one for a served tree, none for a silence case.
    pub(super) fn log_lines(&self) -> Vec<String> {
        match (&self.notice, self.hydrated) {
            (Some(line), _) => vec![line.clone()],
            (None, hydrated) if hydrated > 0 => {
                vec![format!(
                    "prebuilt core: hydrated {hydrated} compiled object(s)"
                )]
            }
            _ => Vec::new(),
        }
    }

    fn source(notice: String, fingerprint: Option<String>) -> Self {
        Self {
            notice: Some(notice),
            hydrated: 0,
            fingerprint,
            served_from: None,
            key: None,
            sha256: None,
            archive_bytes: None,
            downloaded: None,
        }
    }

    /// The "prebuilt was never considered" outcome: no line, no record.
    fn silent() -> Self {
        Self {
            notice: None,
            hydrated: 0,
            fingerprint: None,
            served_from: None,
            key: None,
            sha256: None,
            archive_bytes: None,
            downloaded: None,
        }
    }

    /// Push this consult's single log line, if it has one, onto `log`.
    pub(super) fn log_into(&self, log: &std::sync::Mutex<fbuild_core::BuildLog>) {
        let mut log = log.lock().unwrap_or_else(|error| error.into_inner());
        for line in self.log_lines() {
            log.push(line);
        }
    }

    /// The `prebuilt_libs` block for `build_info.json` (decision 1, #2196 cell
    /// 4): what this build linked against, and — for a source build — why.
    pub(super) fn record(&self) -> crate::prebuilt_record::PrebuiltLibsRecord {
        let record = if self.hydrated > 0 {
            crate::prebuilt_record::PrebuiltLibsRecord::served(
                self.served_from
                    .clone()
                    .unwrap_or_else(|| "prebuilt".to_string()),
                self.key.clone(),
                self.sha256.clone(),
            )
        } else {
            crate::prebuilt_record::PrebuiltLibsRecord::source(self.notice.clone())
        };
        // The exact identity, not just the `fp-<sha16>` path in `key`: a
        // publisher must upload under `fbuild-fp:<fingerprint>` (#1882).
        record
            .with_fingerprint(self.fingerprint.clone())
            // Decision 6's `bytes_downloaded`, per arm: a served tree either
            // transferred the archive or reused one, and the two must not
            // average together in the benchmark.
            .with_download(self.archive_bytes, self.downloaded)
    }
}

/// Consult the ladder for this build's framework core objects and, on a hit,
/// hydrate them into `core_build_dir`.
///
/// Returns an [`Outcome`]: its log lines (none when the policy does not opt in
/// or the framework has no identity — a prebuilt core object is not even
/// considered — one for a source build with its reason, one for a served tree)
/// plus the `prebuilt_libs` record the caller writes to `build_info.json`.
pub(super) async fn consult_and_hydrate(
    params: &crate::BuildParams,
    board: &fbuild_config::BoardConfig,
    framework: &Esp32Framework,
    compiler: &crate::esp32::esp32_compiler::Esp32Compiler,
    toolchain: &fbuild_packages::PackageInfo,
    sources: impl IntoIterator<Item = NormalizedPath>,
    core_build_dir: &Path,
) -> Outcome {
    if !params.prebuilt_libs.serves_core_objects() {
        return Outcome::silent();
    }
    let sdk_variant = board.sdk_variant().to_string();
    let units: Vec<(String, NormalizedPath)> = sources
        .into_iter()
        .map(|source| {
            let id = unit_id(&source, framework, &board.core, &board.variant);
            (id, source)
        })
        .collect();
    let Some(plan) = plan(
        framework,
        compiler,
        toolchain,
        (&board.mcu, &board.core, &board.variant),
        &sdk_variant,
        &units,
    ) else {
        return Outcome::silent();
    };
    let cache_root = fbuild_paths::get_cache_root();
    consult(
        &cache_root,
        &plan,
        &board.mcu,
        &sdk_variant,
        &units,
        core_build_dir,
    )
    .await
}

/// Build the fingerprint for this framework, or `None` when it has no identity.
fn plan(
    framework: &Esp32Framework,
    compiler: &crate::esp32::esp32_compiler::Esp32Compiler,
    toolchain: &fbuild_packages::PackageInfo,
    board: (&str, &str, &str),
    sdk_variant: &str,
    units: &[(String, NormalizedPath)],
) -> Option<PrebuiltCorePlan> {
    let (mcu, core, variant) = board;
    let toolchain_name = toolchain.name.as_str();
    let toolchain_version = toolchain.version.as_str();
    let toolchain_path = toolchain.install_path.as_path();
    let canonical: Vec<(String, Vec<String>)> = units
        .iter()
        .map(|(id, source)| {
            (
                id.clone(),
                canonical_argv(compiler, toolchain_path, id, source),
            )
        })
        .collect();
    let argv: Vec<CompileUnit<'_>> = canonical
        .iter()
        .map(|(unit, argv)| CompileUnit {
            unit,
            argv: argv.as_slice(),
        })
        .collect();
    let config = config_digests(framework, core, variant, sdk_variant);
    let digests: Vec<ConfigDigest<'_>> = config
        .iter()
        .map(|(id, sha)| ConfigDigest { id, sha256: sha })
        .collect();
    framework.prebuilt_core_plan(&CoreBuildInputs {
        compiler_abi: &format!("{toolchain_name} {toolchain_version}"),
        toolchain_name,
        toolchain_version,
        mcu,
        sdk_variant,
        units: &argv,
        config: &digests,
    })
}

/// The canonical command line of one compiled core unit.
///
/// Absolute paths are the part that differs between two hosts that compile
/// byte-identical objects, so the cache root (which holds both the framework and
/// the toolchain) and the toolchain's own install path are replaced by
/// placeholders. The object path is deliberately absent: it carries the source's
/// absolute path through [`CompilerBase::object_path`]'s hash.
fn canonical_argv(
    compiler: &crate::esp32::esp32_compiler::Esp32Compiler,
    toolchain_path: &Path,
    unit_id: &str,
    source: &NormalizedPath,
) -> Vec<String> {
    use crate::compiler::Compiler as _;
    let is_c = source
        .as_path()
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("c"));
    let mut argv = Vec::new();
    let program = if is_c {
        compiler.gcc_path()
    } else {
        compiler.gxx_path()
    };
    argv.push(file_name(program));
    let flags = if is_c {
        compiler.c_flags()
    } else {
        compiler.cpp_flags()
    };
    argv.extend(flags);
    argv.extend(compiler.base.build_define_flags());
    argv.extend(compiler.base.build_include_flags());
    argv.push("-c".to_string());
    argv.push(unit_id.to_string());
    argv.into_iter()
        .map(|arg| canonicalize(&arg, toolchain_path))
        .collect()
}

/// Replace host-specific absolute prefixes with stable placeholders.
fn canonicalize(arg: &str, toolchain_path: &Path) -> String {
    let mut value = arg.to_string();
    for (prefix, placeholder) in [
        (toolchain_path, "<toolchain>"),
        (fbuild_paths::get_cache_root().as_path(), "<cache>"),
        (Path::new("/home"), "<home>"),
    ] {
        if let Some(prefix) = prefix.to_str() {
            if !prefix.is_empty() {
                value = value.replace(prefix, placeholder);
            }
        }
    }
    value
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The config/header digest set this client's v1 fingerprint covers.
///
/// Pinned deliberately: a set that varied with what happened to be on disk would
/// let two different compiles share a fingerprint. A publisher must hash exactly
/// these, and only the ones an install actually ships.
fn config_digests(
    framework: &Esp32Framework,
    core: &str,
    variant: &str,
    sdk_variant: &str,
) -> Vec<(String, String)> {
    let core_dir = NormalizedPath::new(framework.get_core_dir(core));
    let variant_dir = NormalizedPath::new(framework.get_variant_dir(variant));
    let mut files: Vec<(String, NormalizedPath)> = vec![
        (
            format!("cores/{core}/Arduino.h"),
            core_dir.join("Arduino.h"),
        ),
        (
            format!("variants/{variant}/pins_arduino.h"),
            variant_dir.join("pins_arduino.h"),
        ),
    ];
    for base in ["esp32-arduino-libs", "sdk"] {
        let setting = root_of(&core_dir)
            .join("tools")
            .join(base)
            .join(sdk_variant)
            .join("include")
            .join("sdkconfig.h");
        if setting.as_path().is_file() {
            files.push((format!("{base}/{sdk_variant}/include/sdkconfig.h"), setting));
            break;
        }
    }
    files
        .into_iter()
        .filter_map(|(id, path)| {
            let bytes = std::fs::read(path.as_path()).ok()?;
            Some((id, fbuild_packages_fetch::prebuilt::sha256(&bytes)))
        })
        .collect()
}

/// The framework root that contains `cores/` and `variants/`.
fn root_of(core_dir: &NormalizedPath) -> NormalizedPath {
    core_dir
        .as_path()
        .parent()
        .and_then(|cores| cores.parent())
        .map_or_else(|| core_dir.clone(), NormalizedPath::new)
}

/// The canonical unit id of one core source: its path relative to the framework
/// root, `/`-separated, so a tree packed on another machine addresses the same
/// translation unit.
fn unit_id(
    source: &NormalizedPath,
    framework: &Esp32Framework,
    core: &str,
    variant: &str,
) -> String {
    let core_dir = NormalizedPath::new(framework.get_core_dir(core));
    let variant_dir = NormalizedPath::new(framework.get_variant_dir(variant));
    for (prefix, base) in [
        (format!("variants/{variant}"), &variant_dir),
        (format!("cores/{core}"), &core_dir),
    ] {
        if let Some(relative) = source.relative_to(base) {
            return format!("{prefix}/{}", relative.display_slash());
        }
    }
    source.relative_to(&root_of(&core_dir)).map_or_else(
        || file_name(source.as_path()),
        |relative| relative.display_slash(),
    )
}

/// Ask the catalog, then the ladder, then hydrate. Never returns an error.
async fn consult(
    cache_root: &Path,
    plan: &PrebuiltCorePlan,
    mcu: &str,
    sdk_variant: &str,
    units: &[(String, NormalizedPath)],
    core_build_dir: &Path,
) -> Outcome {
    let fingerprint = Some(plan.fingerprint().to_string());
    let query = Query {
        framework: FRAMEWORK.to_string(),
        arch: mcu.to_string(),
        abi: sdk_variant.to_string(),
        version: plan.core_version().to_string(),
        fp: plan.fingerprint().to_string(),
    };
    let asset: Asset = match Reader::from_environment(cache_root).resolve(&query).await {
        Resolution::Hit(asset) => asset,
        Resolution::Miss(reason) => {
            return Outcome::source(
                format!("prebuilt core: no published tree for this identity ({reason})"),
                fingerprint,
            );
        }
    };
    let archives = NormalizedPath::new(cache_root).join("prebuilt-archives");
    let fetcher = Fetcher::from_environment(archives.as_path());
    match serve(cache_root, &query, &asset, &fetcher).await {
        Served::Source(fallback) => Outcome::source(fallback.notice(), fingerprint),
        Served::Prebuilt(obtained) => {
            match hydrate(obtained.tree.root.as_path(), core_build_dir, units) {
                Ok(hydrated) if hydrated > 0 => {
                    served_outcome(&asset, &obtained, hydrated, fingerprint, &query)
                }
                Ok(_) => Outcome::source(
                    "prebuilt core: the served tree covers none of this build's units".to_string(),
                    fingerprint,
                ),
                Err(reason) => Outcome::source(format!("prebuilt core: {reason}"), fingerprint),
            }
        }
    }
}

/// The [`Outcome`] for a tree the ladder served and this build hydrated.
///
/// A separate function, not an inline struct literal, so a test can exercise
/// the download accounting through the same code a build runs: the archive's
/// pinned size and whether this build actually transferred it (decision 6's
/// `bytes_downloaded`, per arm).
fn served_outcome(
    asset: &Asset,
    obtained: &Obtained,
    hydrated: usize,
    fingerprint: Option<String>,
    query: &Query,
) -> Outcome {
    Outcome {
        notice: None,
        hydrated,
        fingerprint,
        served_from: Some(
            asset
                .urls
                .first()
                .cloned()
                .unwrap_or_else(|| asset.filename.clone()),
        ),
        key: Some(identity_key(query)),
        sha256: Some(asset.sha256.clone()),
        archive_bytes: Some(asset.size_bytes),
        downloaded: Some(obtained.downloaded),
    }
}

/// The published artifact's identity key — the same `{fw}/{version}/{arch}/fp-<sha16>`
/// relative path [`fbuild_packages_fetch::prebuilt::tree_root`] extracts into and
/// the bucket key mirrors, so a `build_info.json` record names the object a
/// publisher uploaded.
fn identity_key(query: &Query) -> String {
    format!(
        "{}/{}/{}/fp-{}",
        query.framework,
        query.version,
        query.arch,
        &query.fp[..16]
    )
}

/// Copy a served tree's compiled objects to the paths this host's compiler
/// expects, and report how many units are now up to date.
///
/// Only regular files named by the manifest are written, and only to a path
/// [`crate::compiler::CompilerBase::object_path`] derives from a source this
/// build already compiles, so an archive can never steer a write.
fn hydrate(
    tree: &Path,
    core_build_dir: &Path,
    units: &[(String, NormalizedPath)],
) -> Result<usize, String> {
    let manifest_path = NormalizedPath::new(tree).join(OBJECTS_MANIFEST);
    let bytes = std::fs::read(manifest_path.as_path())
        .map_err(|error| format!("served tree has no {OBJECTS_MANIFEST}: {error}"))?;
    let manifest: ObjectsManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("served tree's {OBJECTS_MANIFEST} is invalid: {error}"))?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(format!(
            "served tree declares manifest schema {} (this client reads {MANIFEST_SCHEMA})",
            manifest.schema
        ));
    }
    let root = NormalizedPath::new(tree);
    let mut hydrated = 0;
    for (unit, source) in units {
        let Some(entry) = manifest.units.get(unit) else {
            continue;
        };
        let object = root.join(entry);
        let metadata = std::fs::symlink_metadata(object.as_path())
            .map_err(|error| format!("served tree is missing {entry}: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err(format!("served tree entry {entry} is not a regular file"));
        }
        let dest = crate::compiler::CompilerBase::object_path(source.as_path(), core_build_dir);
        std::fs::copy(object.as_path(), &dest)
            .map_err(|error| format!("could not hydrate {entry}: {error}"))?;
        hydrated += 1;
    }
    Ok(hydrated)
}

#[cfg(test)]
#[path = "prebuilt_core_tests.rs"]
mod tests;
