//! Default-pilot planning from the actual reproducible compiler invocation.
use super::super::esp32_compiler::Esp32Compiler;
use crate::{
    BuildParams,
    compiler::{Compiler, CompilerBase, apply_compile_unflags, compile_argv_at_root},
    flag_overlay::LanguageExtraFlags,
    prebuilt_fp::{
        FrameworkBuildPlan, PREBUILT_ABI_VERSION, PREBUILT_FINGERPRINT_SCHEMA, PackageIdentity,
        PathMapping, canonical_argv, fingerprint, mapped_path,
    },
};
use fbuild_core::path::NormalizedPath;
use fbuild_packages::{
    PackageInfo,
    prebuilt::{PrebuiltOutcome, sha256, valid_sha},
};
use std::{collections::BTreeMap, path::Path};

#[cfg(test)]
#[path = "prebuilt_tests.rs"]
mod tests;

pub(super) struct Inputs<'a, S: AsRef<Path>> {
    pub params: &'a BuildParams,
    pub compiler: &'a Esp32Compiler,
    pub toolchain: &'a PackageInfo,
    pub framework: &'a PackageInfo,
    pub framework_root: &'a Path,
    pub toolchain_root: &'a Path,
    pub farm: Option<&'a Path>,
    pub eligible: bool,
    pub board: &'a str,
    pub mcu: &'a str,
    pub variant: &'a str,
    pub sources: &'a [S],
    pub core_dir: &'a Path,
    pub overlay: &'a LanguageExtraFlags,
}

pub(super) fn eligible(params: &BuildParams, ctx: &crate::pipeline::BuildContext) -> bool {
    let boards: Vec<String> =
        serde_json::from_str(include_str!("prebuilt-pilot.json")).unwrap_or_default();
    NormalizedPath::new(&ctx.src_dir) == NormalizedPath::new(&params.project_dir).join("src")
        && boards.contains(&ctx.board.board_id)
        && params.extra_build_flags.is_empty()
        && ctx
            .config
            .get_build_flags(&params.env_name)
            .is_ok_and(|v| v.is_empty())
        && ctx
            .config
            .get_build_unflags(&params.env_name)
            .is_ok_and(|v| v.is_empty())
        && ctx
            .config
            .get_extra_scripts(&params.env_name)
            .is_ok_and(|v| v.is_empty())
        && ctx
            .config
            .get_board_overrides(&params.env_name)
            .is_ok_and(|v| v.is_empty())
}

fn package(info: &PackageInfo) -> Option<PackageIdentity> {
    let identity = if let Some(checksum) = info.checksum.as_deref() {
        let checksum = checksum.strip_prefix("sha256:").unwrap_or(checksum);
        if !valid_sha(checksum) {
            return None;
        }
        format!("registry:{}", checksum.to_ascii_lowercase())
    } else {
        let Some(id) =
            fbuild_packages::content_manifest::load_verified_content_id(&info.install_path, info)
        else {
            tracing::info!(package = %info.name, root = %info.install_path.display(),
                "prebuilt: skipped: package has no verified content manifest");
            return None;
        };
        format!("tree:{id}")
    };
    Some(PackageIdentity {
        id: info.name.clone(),
        version: info.version.clone(),
        sha256: identity,
    })
}

fn roots(
    params: &BuildParams,
    framework: &Path,
    toolchain: &Path,
    farm: Option<&Path>,
) -> Vec<PathMapping> {
    let mut roots = vec![
        PathMapping::new(&fbuild_paths::get_cache_root(), "/fbuild/cache"),
        PathMapping::new(&params.project_dir, "/fbuild/project"),
        PathMapping::new(&params.build_dir, "/fbuild/build"),
        PathMapping::new(toolchain, "/fbuild/toolchain"),
        PathMapping::new(framework, "/fbuild/framework"),
    ];
    if let Some(farm) = farm {
        roots.push(PathMapping::new(farm, "/fbuild/sdk-includes"));
    }
    roots
}

pub(super) fn prefix_maps(
    params: &BuildParams,
    framework: &Path,
    toolchain: &Path,
    farm: Option<&Path>,
) -> Vec<String> {
    roots(params, framework, toolchain, farm)
        .iter()
        .map(|root| {
            format!(
                "-ffile-prefix-map={}={}",
                root.root.display_slash(),
                root.prefix
            )
        })
        .collect()
}

pub(super) fn metadata_hash(original: String, reproducible: bool) -> String {
    if reproducible {
        // Rebuild legacy no-op outputs once so they acquire the exported plan.
        sha256(format!("{original}:prebuilt-abi-{PREBUILT_ABI_VERSION}:plan-export-v1").as_bytes())
    } else {
        original
    }
}

fn plan<S: AsRef<Path>>(input: &Inputs<'_, S>) -> Option<FrameworkBuildPlan> {
    if !input.eligible {
        return None;
    }
    let mappings = roots(
        input.params,
        input.framework_root,
        input.toolchain_root,
        input.farm,
    );
    let mut argv = Vec::new();
    for (ordinal, source) in input.sources.iter().enumerate() {
        let source = source.as_ref();
        let output = CompilerBase::object_path(source, input.core_dir);
        let cwd = input.compiler.framework_compile_root(source).or_else(|| {
            tracing::info!(source = %source.display(), "prebuilt: skipped: source has no framework compile root");
            None
        })?;
        let cpp = !matches!(
            source
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("c" | "s")
        );
        let flags = if cpp {
            input.compiler.cpp_flags()
        } else {
            input.compiler.c_flags()
        };
        let (flags, extra) = apply_compile_unflags(
            flags,
            &input.overlay.for_source(source),
            input.compiler.build_unflags(),
        );
        let (mut raw, _) = compile_argv_at_root(
            source,
            &output,
            &flags,
            &input.compiler.invocation_pre_flags(source),
            &extra,
            Some(cwd),
        );
        raw.insert(
            0,
            NormalizedPath::new(if cpp {
                input.compiler.gxx_path()
            } else {
                input.compiler.gcc_path()
            })
            .display_slash(),
        );
        argv.push(canonical_argv(&raw, cwd, &mappings, ordinal).or_else(|| {
            tracing::info!(source = %source.display(), "prebuilt: skipped: unsupported compiler arguments");
            None
        })?);
    }
    Some(FrameworkBuildPlan {
        abi_version: PREBUILT_ABI_VERSION,
        fingerprint_schema: PREBUILT_FINGERPRINT_SCHEMA,
        toolchain: package(input.toolchain)?,
        framework: package(input.framework)?,
        mcu: input.mcu.into(),
        variant_dir: mapped_path(Path::new(input.variant), input.framework_root, &mappings),
        argv,
        config_sha256: config_digests(input, &mappings).or_else(|| {
            tracing::info!("prebuilt: skipped: cannot hash framework sources or include tree");
            None
        })?,
        path_map_prefix: "/fbuild".into(),
    })
}

pub(super) async fn hydrate<S: AsRef<Path> + Sync>(input: Inputs<'_, S>) -> bool {
    if !input.eligible {
        return skipped(&input, "unsupported configuration or compiler prefix maps");
    }
    let cache_root = fbuild_paths::get_cache_root();
    for dir in [cache_root.as_path(), input.params.build_dir.as_path()]
        .into_iter()
        .chain(input.farm)
    {
        if let Err(error) = std::fs::create_dir_all(dir) {
            return skipped(
                &input,
                &format!("cannot create output root {}: {error}", dir.display()),
            );
        }
    }
    if !crate::prebuilt_paths::unambiguous_roots(&roots(
        input.params,
        input.framework_root,
        input.toolchain_root,
        input.farm,
    ))
    .await
    {
        return skipped(
            &input,
            "ambiguous or unavailable roots (see root diagnostic)",
        );
    }
    let Some(plan) = plan(&input) else {
        return skipped(&input, "unsupported or incomplete local build plan");
    };
    let fp = fingerprint(&plan);
    let objects: BTreeMap<_, _> = input
        .sources
        .iter()
        .enumerate()
        .map(|(n, source)| {
            (
                format!("obj/{n}.o"),
                CompilerBase::object_path(source.as_ref(), input.core_dir),
            )
        })
        .collect();
    let metadata = serde_json::json!({"fp":fp,"plan":plan,"objects":objects,
        "core_dir":input.core_dir,"include_dirs":input.compiler.base.include_dirs,
        "framework_dir":input.framework_root,"board":input.board});
    if let Err(error) = fbuild_core::fs::write_atomic(
        input.params.build_dir.join("prebuilt-plan.json"),
        serde_json::to_vec(&metadata).unwrap_or_default(),
    )
    .await
    {
        return skipped(&input, &format!("cannot write prebuilt-plan.json: {error}"));
    }
    let Some(client) = &input.params.prebuilt else {
        return false;
    };
    let PrebuiltOutcome::Hit(path) = client.resolve(plan).await else {
        return false;
    };
    match install(&path, &input, &fp) {
        Ok(()) => true,
        Err(error) => {
            tracing::info!("prebuilt: {error}; building from source");
            false
        }
    }
}

fn install<S: AsRef<Path>>(entry: &Path, input: &Inputs<'_, S>, fp: &str) -> std::io::Result<()> {
    let objects: Vec<_> = input
        .sources
        .iter()
        .enumerate()
        .map(|(n, source)| {
            let target = CompilerBase::object_path(source.as_ref(), input.core_dir);
            (source.as_ref(), target, entry.join(format!("obj/{n}.o")))
        })
        .collect();
    if objects.iter().any(|(_, _, from)| !from.is_file()) {
        return Err(std::io::Error::other("missing framework objects"));
    }
    std::fs::create_dir_all(input.core_dir)?;
    // The next request must use the ordinary clean path before fast-path or
    // depfile checks can reuse an object delivered under this fingerprint.
    fbuild_core::fs::write_atomic_sync(
        input.params.build_dir.join(crate::prebuilt::INSTALL_MARKER),
        fp.as_bytes(),
    )?;
    let headers = NormalizedPath::new(entry.join("include"));
    for file in walkdir::WalkDir::new(&headers) {
        let file = file.map_err(std::io::Error::other)?;
        if file.file_type().is_file() {
            let relative = NormalizedPath::new(file.path())
                .relative_to(&headers)
                .ok_or_else(|| std::io::Error::other("header outside include root"))?;
            let target = input
                .params
                .build_dir
                .join("prebuilt/include")
                .join(relative.as_path());
            std::fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| std::io::Error::other("invalid include path"))?,
            )?;
            std::fs::copy(file.path(), target)?;
        }
    }
    for (source, target, from) in objects {
        std::fs::copy(from, &target)?;
        std::fs::write(
            target.with_extension("cmdhash"),
            input
                .compiler
                .rebuild_signature(source, &input.overlay.for_source(source), &target),
        )?;
        match std::fs::remove_file(target.with_extension("d")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn config_digests<S: AsRef<Path>>(
    input: &Inputs<'_, S>,
    mappings: &[PathMapping],
) -> Option<BTreeMap<String, String>> {
    let mut files: Vec<_> = input
        .sources
        .iter()
        .map(|s| s.as_ref().to_path_buf())
        .collect();
    let mut configs = BTreeMap::new();
    for dir in &input.compiler.base.include_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(dir).follow_links(true) {
            let entry = entry.ok()?;
            if entry.file_type().is_file()
                && entry.path().extension().is_none_or(|ext| {
                    matches!(
                        ext.to_str(),
                        Some("h" | "hpp" | "hxx" | "hh" | "inc" | "inl" | "tpp")
                    )
                })
            {
                files.push(entry.into_path());
            }
        }
        let path = dir.join("sdkconfig.h");
        if path.is_file() {
            configs.insert(
                mapped_path(&path, input.framework_root, mappings),
                sha256(&std::fs::read(path).ok()?),
            );
        }
    }
    files.sort();
    files.dedup();
    configs.insert(
        "framework-source-and-header-tree".into(),
        crate::build_fingerprint::hash_named_files(files.iter().map(|path| {
            (
                mapped_path(path, input.framework_root, mappings),
                path.as_path(),
            )
        }))
        .ok()?,
    );
    Some(configs)
}

fn skipped<S: AsRef<Path>>(_input: &Inputs<'_, S>, reason: &str) -> bool {
    tracing::info!("prebuilt: skipped: {reason}; building from source");
    false
}
