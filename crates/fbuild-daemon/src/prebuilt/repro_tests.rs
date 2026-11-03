//! Real compiler regression for producer/consumer path invariance.
use super::{
    Service,
    tests::{asset, server},
};
use fbuild_build::prebuilt_fp::{
    FrameworkBuildPlan, PackageIdentity, PathMapping, canonical_argv, fingerprint, mapped_path,
};
use fbuild_core::path::NormalizedPath;
use fbuild_packages::prebuilt::{PrebuiltOutcome, Query, Reader, Resolution, sha256};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

const CORE_SOURCE: &str = r#"#include "config.h"
#include "project.h"
#include "sdk.h"
struct Config {int value; Config():value(VALUE){}};
static Config initialized;
extern "C" {
const char *core_file=__FILE__; const char *base_file=__BASE_FILE__;
int core_value(void){return initialized.value;}
const char *header_file(void){return header_name;}
const char *project_file(void){return project_name;}
const char *sdk_file(void){return sdk_name;}
}
"#;

async fn compile_fixture(
    framework: &Path,
    project: &Path,
    output: &Path,
    lto: bool,
) -> (FrameworkBuildPlan, Vec<u8>) {
    std::fs::create_dir_all(framework).unwrap();
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(
        framework.join("config.h"),
        "#define VALUE 17\nstatic const char header_name[] = __FILE__;\n",
    )
    .unwrap();
    std::fs::write(framework.join("core.c"), CORE_SOURCE).unwrap();
    let farm = framework
        .parent()
        .unwrap()
        .join("include-farms")
        .join(&sha256(framework.to_string_lossy().as_bytes())[..16]);
    std::fs::create_dir_all(&farm).unwrap();
    std::fs::write(
        farm.join("sdk.h"),
        "static const char sdk_name[]=__FILE__;\n",
    )
    .unwrap();
    let mappings = vec![
        PathMapping::new(framework, "/fbuild/framework"),
        PathMapping::new(project, "/fbuild/project"),
        PathMapping::new(output.parent().unwrap(), "/fbuild/build"),
        PathMapping::new(&farm, "/fbuild/sdk-includes"),
    ];
    let mut flags = vec![
        "-Os".into(),
        "-g".into(),
        "-x".into(),
        "c++".into(),
        "-frandom-seed=fixture-core".into(),
    ];
    if lto {
        flags.push("-flto".into());
    }
    if fbuild_core::platform::host::is_windows() {
        flags.push("--target=x86_64-unknown-linux-gnu".into());
    }
    flags.extend(
        mappings
            .iter()
            .map(|m| format!("-ffile-prefix-map={}={}", m.root.display_slash(), m.prefix)),
    );
    let mut includes = vec![
        format!("-I{}", NormalizedPath::new(framework).display_slash()),
        format!("-I{}", NormalizedPath::new(project).display_slash()),
    ];
    includes.push(format!("-I{}", NormalizedPath::new(&farm).display_slash()));
    let (raw, cwd) = fbuild_build::compiler::compile_argv_at_root(
        &framework.join("core.c"),
        output,
        &flags,
        &includes,
        &[],
        Some(framework),
    );
    let compiler = if fbuild_core::platform::host::is_windows() {
        "clang"
    } else {
        "cc"
    };
    let mut command = vec![compiler];
    command.extend(raw.iter().map(String::as_str));
    let result = fbuild_core::subprocess::run_command(&command, cwd.as_deref(), None, None)
        .await
        .unwrap();
    assert!(result.success(), "{}", result.stderr);
    let mut recorded = vec![compiler.to_owned()];
    recorded.extend(raw);
    let argv = canonical_argv(&recorded, framework, &mappings, 0).unwrap();
    let files = [
        framework.join("core.c"),
        framework.join("config.h"),
        project.join("project.h"),
        farm.join("sdk.h"),
    ];
    let digest = fbuild_build::build_fingerprint::hash_named_files(
        files
            .iter()
            .map(|p| (mapped_path(p, framework, &mappings), p.as_path())),
    )
    .unwrap();
    let package = PackageIdentity {
        id: "fixture".into(),
        version: "1".into(),
        sha256: "a".repeat(64),
    };
    let plan = FrameworkBuildPlan {
        abi_version: 1,
        toolchain: package.clone(),
        framework: package,
        mcu: "esp32".into(),
        variant_dir: "/fbuild/framework/variants/esp32".into(),
        argv: vec![argv],
        config_sha256: BTreeMap::from([("files".into(), digest)]),
        path_map_prefix: "/fbuild".into(),
    };
    (plan, std::fs::read(output).unwrap())
}

fn bundle(plan: &FrameworkBuildPlan, object: &[u8]) -> Vec<u8> {
    let manifest=serde_json::to_vec(&json!({"fp":fingerprint(plan),"abi_version":1,"inputs":plan,"builder_version":"fixture",
        "fbuild_version":"fixture","source":{"repo_url":"fixture","ref":"fixture"},"files":{"obj/0.o":sha256(object)}})).unwrap();
    let mut tar = tar::Builder::new(Vec::new());
    for (name, data) in [
        ("fbuild-prebuilt.json", manifest.as_slice()),
        ("obj/0.o", object),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, data).unwrap();
    }
    zstd::encode_all(tar.into_inner().unwrap().as_slice(), 1).unwrap()
}

#[tokio::test]
async fn producer_custom_output_and_normal_consumer_have_equal_fp_object_bytes_and_a_catalog_hit() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("project.h"),
        "static const char project_name[]=__FILE__;\n",
    )
    .unwrap();
    let producer_root = temp.path().join("builder-cache/framework");
    let consumer_root = temp.path().join("consumer-cache/framework");
    let (producer, producer_object) = compile_fixture(
        &producer_root,
        &project,
        &temp.path().join("random-builder-output/core_old_hash.o"),
        false,
    )
    .await;
    let (consumer, consumer_object) = compile_fixture(
        &consumer_root,
        &project,
        &project
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join("build/release/core_new_hash.o"),
        false,
    )
    .await;
    assert_eq!(producer, consumer);
    assert_eq!(
        producer_object, consumer_object,
        "debug and __FILE__/__BASE_FILE__ bytes must match"
    );
    let fp = fingerprint(&producer);
    let bytes = bundle(&producer, &producer_object);
    let (url, _, artifact_server) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let mut object = asset(&bytes, vec![url]);
    object.provides = vec![format!("fbuild-fp:{fp}")];
    let catalog=serde_json::to_vec(&json!({"kind":"Catalog","schema_version":1,"tool":"prebuilt-arduino-esp32","channels":{"1":"1"},
        "releases":[{"tool":"prebuilt-arduino-esp32","version":"1","platforms":[{"platform":{"os":"none","arch":"esp32","abi":"fixture@1"},
            "variant":{"flavor":format!("fp-{}",&fp[..16])},"asset":object}]}]})).unwrap();
    let (url, _, catalog_server) = server(catalog.clone(), axum::http::StatusCode::OK).await;
    let index = serde_json::to_vec(
        &json!({"kind":"Index","schema_version":1,"tools":{"prebuilt-arduino-esp32":{"descriptor":{
        "url":url,"sha256":sha256(&catalog),"size_bytes":catalog.len()}}}}),
    )
    .unwrap();
    let (url, _, index_server) = server(index, axum::http::StatusCode::OK).await;
    let cache = temp.path().join("client-cache");
    let reader = Reader::new(&cache, vec![url], reqwest::Client::new());
    let query = Query {
        framework: "arduino-esp32".into(),
        arch: "esp32".into(),
        abi: "fixture@1".into(),
        version: "1".into(),
        fp: fingerprint(&consumer),
    };
    let Resolution::Hit(asset) = reader.resolve(&query).await else {
        panic!("consumer must resolve producer Catalog")
    };
    let service = Service::new(&cache);
    let PrebuiltOutcome::Hit(entry) = service.fetch(&asset, &query.fp).await else {
        panic!("consumer must fetch")
    };
    assert_eq!(
        std::fs::read(entry.join("obj/0.o")).unwrap(),
        consumer_object
    );
    artifact_server.abort();
    catalog_server.abort();
    index_server.abort();
}

struct ReusingCore {
    compiles: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl fbuild_build::BuildOrchestrator for ReusingCore {
    fn platform(&self) -> fbuild_core::Platform {
        fbuild_core::Platform::Espressif32
    }
    async fn build(
        &self,
        params: &fbuild_build::BuildParams,
    ) -> fbuild_core::Result<fbuild_build::BuildResult> {
        if fbuild_build::prebuilt::must_clean(
            &params.build_dir,
            params.clean,
            params.compiledb_only,
        ) && params.build_dir.exists()
        {
            std::fs::remove_dir_all(&params.build_dir)?;
        }
        std::fs::create_dir_all(&params.build_dir)?;
        let source = params.project_dir.join("core.c");
        let object = params.build_dir.join("core.o");
        let mut plan = super::tests::fixture_plan();
        plan.config_sha256.insert(
            "config.h".into(),
            sha256(&std::fs::read(params.project_dir.join("config.h"))?),
        );
        if let Some(client) = &params.prebuilt {
            if let PrebuiltOutcome::Hit(entry) = client.resolve(plan).await {
                std::fs::copy(entry.join("obj/0.o"), &object)?;
                std::fs::write(object.with_extension("cmdhash"), "fixture-command")?;
                std::fs::write(
                    params
                        .build_dir
                        .join(fbuild_build::prebuilt::INSTALL_MARKER),
                    b"installed",
                )?;
            }
        }
        let compiler = if fbuild_core::platform::host::is_windows() {
            "clang"
        } else {
            "cc"
        };
        if fbuild_build::compiler::CompilerBase::needs_rebuild_with_signature(
            &source,
            &object,
            Some("fixture-command"),
        ) {
            let result = fbuild_core::subprocess::run_command(
                &[
                    compiler,
                    "-I",
                    params.project_dir.to_str().unwrap(),
                    "-c",
                    source.to_str().unwrap(),
                    "-o",
                    object.to_str().unwrap(),
                ],
                None,
                None,
                None,
            )
            .await?;
            if !result.success() {
                return Err(fbuild_core::FbuildError::BuildFailed(result.stderr));
            }
            std::fs::write(object.with_extension("cmdhash"), "fixture-command")?;
            self.compiles
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let output = params.build_dir.join("firmware.bin");
        let result = fbuild_core::subprocess::run_command(
            &[
                compiler,
                params.project_dir.join("main.c").to_str().unwrap(),
                object.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
            ],
            None,
            None,
            None,
        )
        .await?;
        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(result.stderr));
        }
        Ok(fbuild_build::BuildResult {
            success: true,
            firmware_path: Some(output),
            elf_path: None,
            size_info: None,
            symbol_map: None,
            build_time_secs: 0.0,
            message: "real cached core fixture".into(),
            compile_database_path: None,
            build_log: fbuild_core::BuildLog::new(),
        })
    }
}

#[tokio::test]
async fn header_change_after_hit_recompiles_on_miss_and_opt_out_without_depfiles() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for opt_out in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("config.h"), "#define VALUE 17\n").unwrap();
        std::fs::write(
            root.join("core.c"),
            "#include \"config.h\"\nint value(void){return VALUE;}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.c"),
            "extern int value(void); int main(void){return value();}\n",
        )
        .unwrap();
        let compiler = if fbuild_core::platform::host::is_windows() {
            "clang"
        } else {
            "cc"
        };
        let original = root.join("original.o");
        let result = fbuild_core::subprocess::run_command(
            &[
                compiler,
                "-I",
                root.to_str().unwrap(),
                "-c",
                root.join("core.c").to_str().unwrap(),
                "-o",
                original.to_str().unwrap(),
            ],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(result.success(), "{}", result.stderr);
        let mut plan = super::tests::fixture_plan();
        plan.config_sha256.insert(
            "config.h".into(),
            sha256(&std::fs::read(root.join("config.h")).unwrap()),
        );
        let original = std::fs::read(original).unwrap();
        let bytes = bundle(&plan, &original);
        let (url, _, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
        let artifact = asset(&bytes, vec![url]);
        let service = Arc::new(Service::new(&root.join("cache")));
        let client = || {
            let service = service.clone();
            let asset = artifact.clone();
            fbuild_build::prebuilt::Client::new(
                Arc::new(move |plan| {
                    let service = service.clone();
                    let asset = asset.clone();
                    Box::pin(async move { service.fetch(&asset, &fingerprint(&plan)).await })
                }),
                Arc::new(|_| {}),
            )
        };
        let core = ReusingCore {
            compiles: AtomicUsize::new(0),
        };
        let first = super::fallback_tests::params(root, client());
        fbuild_build::prebuilt::build_with_retry(&core, first)
            .await
            .unwrap();
        assert_eq!(core.compiles.load(Ordering::SeqCst), 0);
        std::fs::write(root.join("config.h"), "#define VALUE 29\n").unwrap();
        let mut next = super::fallback_tests::params(root, client());
        if opt_out {
            next.prebuilt = None;
        }
        let directory = next.build_dir.clone();
        fbuild_build::BuildOrchestrator::build(&core, &next)
            .await
            .unwrap();
        assert_eq!(core.compiles.load(Ordering::SeqCst), 1);
        assert_ne!(std::fs::read(directory.join("core.o")).unwrap(), original);
        task.abort();
    }
}

async fn checked_command(args: Vec<String>, cwd: &Path) {
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let result = fbuild_core::subprocess::run_command(&refs, Some(cwd), None, None)
        .await
        .unwrap();
    assert!(result.success(), "{}", result.stderr);
}

#[tokio::test]
async fn producer_lto_core_mixed_with_consumer_cpp_sketch_matches_all_consumer_elf() {
    let temp = tempfile::tempdir().unwrap();
    let producer_project = temp.path().join("producer-project");
    let consumer_project = temp.path().join("consumer-project");
    for project in [&producer_project, &consumer_project] {
        std::fs::create_dir_all(project).unwrap();
        std::fs::write(
            project.join("project.h"),
            "static const char project_name[]=__FILE__;\n",
        )
        .unwrap();
    }
    let producer_object = temp.path().join("producer-out/core.o");
    let consumer_object = consumer_project
        .join(fbuild_paths::FBUILD_DIR_NAME)
        .join("build/core.o");
    let (producer, _) = compile_fixture(
        &temp.path().join("producer-cache/framework"),
        &producer_project,
        &producer_object,
        true,
    )
    .await;
    let (consumer, _) = compile_fixture(
        &temp.path().join("consumer-cache/framework"),
        &consumer_project,
        &consumer_object,
        true,
    )
    .await;
    assert_eq!(fingerprint(&producer), fingerprint(&consumer));
    let sketch = consumer_project.join("sketch.cpp");
    std::fs::write(&sketch,r#"#include "project.h"
extern "C" int core_value(void);
extern "C" const char *header_file(void), *sdk_file(void), *project_file(void);
static int private_state=3;
static int private_value(int value){return value+private_state;}
volatile int output;
const char *volatile names[5];
extern "C" void _start(void){output=private_value(core_value()); names[0]=header_file(); names[1]=sdk_file(); names[2]=project_file(); names[3]=__FILE__; names[4]=__BASE_FILE__; for(;;){}}
"#).unwrap();
    let compiler = fixture_compiler();
    let mut flags = vec![
        "-Os".into(),
        "-g".into(),
        "-flto".into(),
        "-frandom-seed=consumer-sketch".into(),
        format!(
            "-ffile-prefix-map={}=/fbuild/project",
            NormalizedPath::new(&consumer_project).display_slash()
        ),
    ];
    if fbuild_core::platform::host::is_windows() {
        flags.push("--target=x86_64-unknown-linux-gnu".into());
    }
    let sketch_object = consumer_project
        .join(fbuild_paths::FBUILD_DIR_NAME)
        .join("build/sketch.o");
    let (raw, cwd) = fbuild_build::compiler::compile_argv_at_root(
        &sketch,
        &sketch_object,
        &flags,
        &[],
        &[],
        None,
    );
    let mut command = vec![compiler.to_owned()];
    command.extend(raw);
    checked_command(command, cwd.as_deref().unwrap()).await;
    let mut outputs = Vec::new();
    for (name, core) in [("mixed", &producer_object), ("consumer", &consumer_object)] {
        let output = consumer_project.join(format!("{name}.elf"));
        let mut args = vec![
            compiler.to_owned(),
            "-g".into(),
            "-flto".into(),
            "-nostdlib".into(),
            "-Wl,-e,_start".into(),
            "-Wl,--build-id=none".into(),
        ];
        if fbuild_core::platform::host::is_windows() {
            args.extend([
                "--target=x86_64-unknown-linux-gnu".into(),
                "-fuse-ld=lld".into(),
            ]);
        }
        args.extend([
            core.to_string_lossy().into_owned(),
            sketch_object.to_string_lossy().into_owned(),
            "-o".into(),
            output.to_string_lossy().into_owned(),
        ]);
        checked_command(args, &consumer_project).await;
        outputs.push(std::fs::read(output).unwrap());
    }
    assert_eq!(
        outputs[0], outputs[1],
        "mixed-link firmware/ELF including debug sections must be byte identical"
    );
}

fn fixture_compiler() -> &'static str {
    if fbuild_core::platform::host::is_windows() {
        "clang"
    } else {
        "cc"
    }
}
