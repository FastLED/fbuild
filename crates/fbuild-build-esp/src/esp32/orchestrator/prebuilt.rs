//! Pilot framework planning and installation from daemon-verified entries.
use super::super::esp32_compiler::Esp32Compiler;
use crate::{
    BuildParams,
    compiler::{Compiler, CompilerBase, apply_compile_unflags, compile_argv},
    flag_overlay::LanguageExtraFlags,
    prebuilt_fp::{FrameworkBuildPlan, PREBUILT_ABI_VERSION, PackageIdentity, fingerprint},
};
use fbuild_packages::{
    PackageInfo,
    prebuilt::{PrebuiltOutcome, sha256, valid_sha},
};
use std::{collections::BTreeMap, path::Path};

pub(super) struct Inputs<'a, S: AsRef<Path>> {
    pub params: &'a BuildParams,
    pub compiler: &'a Esp32Compiler,
    pub toolchain: &'a PackageInfo,
    pub framework: &'a PackageInfo,
    pub board: &'a str,
    pub mcu: &'a str,
    pub variant: &'a str,
    pub sources: &'a [S],
    pub core_dir: &'a Path,
    pub overlay: &'a LanguageExtraFlags,
}

fn package(info: &PackageInfo) -> Option<PackageIdentity> {
    let checksum = info
        .checksum
        .as_deref()?
        .strip_prefix("sha256:")
        .unwrap_or(info.checksum.as_deref()?);
    valid_sha(checksum).then(|| PackageIdentity {
        id: info.name.clone(),
        version: info.version.clone(),
        sha256: checksum.into(),
    })
}

fn plan<S: AsRef<Path>>(input: &Inputs<'_, S>) -> Option<FrameworkBuildPlan> {
    let boards: Vec<String> = serde_json::from_str(include_str!("prebuilt-pilot.json")).ok()?;
    if !boards.iter().any(|board| board == input.board) {
        return None;
    }
    let mut argv = Vec::new();
    for source in input.sources {
        let source = source.as_ref();
        let output = CompilerBase::object_path(source, input.core_dir);
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
        let extra = input.overlay.for_source(source);
        let (flags, extra) = apply_compile_unflags(flags, &extra, input.compiler.build_unflags());
        let (mut args, _) = compile_argv(
            source,
            &output,
            &flags,
            &input.compiler.base.build_include_flags(),
            &extra,
        );
        args.insert(
            0,
            if cpp {
                input.compiler.gxx_path()
            } else {
                input.compiler.gcc_path()
            }
            .to_string_lossy()
            .into_owned(),
        );
        argv.push(args);
    }
    let configs = config_digests(input)?;
    Some(FrameworkBuildPlan {
        abi_version: PREBUILT_ABI_VERSION,
        toolchain: package(input.toolchain)?,
        framework: package(input.framework)?,
        mcu: input.mcu.into(),
        variant_dir: input.variant.into(),
        argv,
        config_sha256: configs,
        path_map_prefix: "/fbuild".into(),
    })
}

pub(super) async fn hydrate<S: AsRef<Path> + Sync>(input: Inputs<'_, S>) -> bool {
    let Some(plan) = plan(&input) else {
        return false;
    };
    // Export the actual resolved plan for the manual builder; it never derives
    // flags or fingerprints independently of the build engine.
    let metadata = serde_json::json!({"fp":fingerprint(&plan),"plan":plan,
        "core_dir":input.core_dir,"include_dirs":input.compiler.base.include_dirs,
        "framework_dir":input.framework.install_path,"board":input.board});
    let _ = std::fs::write(
        input.params.build_dir.join("prebuilt-plan.json"),
        serde_json::to_vec(&metadata).unwrap_or_default(),
    );
    let Some(client) = &input.params.prebuilt else {
        return false;
    };
    let PrebuiltOutcome::Hit(path) = client.resolve(plan).await else {
        return false;
    };
    match install(&path, &input) {
        Ok(()) => true,
        Err(error) => {
            tracing::info!("prebuilt: {error}; building from source");
            false
        }
    }
}

fn install<S: AsRef<Path>>(entry: &Path, input: &Inputs<'_, S>) -> std::io::Result<()> {
    // Check completeness before writing any build object. The daemon already
    // checked every file's hash and the full local fingerprint.
    let objects: Vec<_> = input
        .sources
        .iter()
        .map(|source| {
            let source = source.as_ref();
            let target = CompilerBase::object_path(source, input.core_dir);
            let from = entry
                .join("obj")
                .join(target.file_name().unwrap_or_default());
            (source, target, from)
        })
        .collect();
    if objects.iter().any(|(_, _, from)| !from.is_file()) {
        return Err(std::io::Error::other("missing framework objects"));
    }
    std::fs::create_dir_all(input.core_dir)?;
    let headers = entry.join("include");
    for entry in walkdir::WalkDir::new(&headers) {
        let entry = entry.map_err(std::io::Error::other)?;
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(&headers)
                .map_err(std::io::Error::other)?;
            let target = input
                .params
                .build_dir
                .join("prebuilt/include")
                .join(relative);
            std::fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| std::io::Error::other("invalid include path"))?,
            )?;
            std::fs::copy(entry.path(), target)?;
        }
    }
    for (source, target, from) in objects {
        std::fs::copy(from, &target)?;
        let signature =
            input
                .compiler
                .rebuild_signature(source, &input.overlay.for_source(source), &target);
        std::fs::write(target.with_extension("cmdhash"), signature)?;
        match std::fs::remove_file(target.with_extension("d")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(super) fn prefix_maps(framework: &Path, toolchain: &Path) -> Vec<String> {
    [
        (framework, "/fbuild/framework"),
        (toolchain, "/fbuild/toolchain"),
    ]
    .into_iter()
    .map(|(path, prefix)| {
        format!(
            "-ffile-prefix-map={}={prefix}",
            fbuild_core::path::NormalizedPath::new(path).display_slash()
        )
    })
    .collect()
}

fn config_digests<S: AsRef<Path>>(input: &Inputs<'_, S>) -> Option<BTreeMap<String, String>> {
    let mut files: Vec<_> = input
        .sources
        .iter()
        .map(|source| source.as_ref().to_path_buf())
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
                path.to_string_lossy().into_owned(),
                sha256(&std::fs::read(path).ok()?),
            );
        }
    }
    files.sort();
    files.dedup();
    configs.insert(
        "framework-source-and-header-tree".into(),
        crate::build_fingerprint::hash_files(&files).ok()?,
    );
    Some(configs)
}
