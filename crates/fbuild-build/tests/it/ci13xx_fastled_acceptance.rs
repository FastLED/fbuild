//! Explicit real FastLED consumer acceptance for native CI1306 (#1914).
use fbuild_build::{BuildParams, compile_backend, provision, provision_env};
use fbuild_core::path::{NormalizedPath, canonicalize_existing};
use fbuild_core::{BuildProfile, Platform};
use object::{Object, ObjectSection, ObjectSymbol};
use sha2::{Digest, Sha256};
use std::path::Path;

const PROBE: &str = r#"
#include <FastLED.h>
#include "platforms/shared/spi_bitbang/generic_software_spi.h"
using SPI = fl::GenericSoftwareSPIOutput<2, 3, DATA_RATE_MHZ(1)>;
SPI spi;
volatile int ci13xx_fastled_count;
extern "C" __attribute__((noinline, used, externally_visible)) void ci13xx_spi_scalar(unsigned char v) {
    SPI::writeByte(v);
    SPI::writeWord(static_cast<unsigned short>(v) * 257);
}
extern "C" __attribute__((noinline, used, externally_visible)) void ci13xx_spi_bulk(unsigned char v) {
    unsigned char bytes[3] = {v, static_cast<unsigned char>(~v), 0x81};
    spi.writeBytes(bytes, 3);
    spi.writeBytesValue(v, 3);
}
extern "C" {
void (*volatile ci13xx_spi_scalar_entry)(unsigned char) = ci13xx_spi_scalar;
void (*volatile ci13xx_spi_bulk_entry)(unsigned char) = ci13xx_spi_bulk;
}
__attribute__((noinline)) void setup() {
    spi.init(); ci13xx_spi_scalar_entry(0x81); ci13xx_spi_bulk_entry(0x55);
    ci13xx_fastled_count = FastLED.count();
}
__attribute__((noinline)) void loop() { ci13xx_spi_scalar_entry(static_cast<unsigned char>(millis())); }
"#;

pub(super) fn params(project: &Path) -> BuildParams {
    BuildParams {
        project_dir: project.into(),
        env_name: "ci1306".into(),
        build_dir: project
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join("build/ci1306/release"),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        verbose: true,
        jobs: Some(4),
        generate_compiledb: true,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        shrink: Default::default(),
        caller_path: None,
    }
}

async fn input_identity(source: &Path) -> serde_json::Value {
    let root = source.to_str().expect("UTF-8 FastLED root");
    let head = fbuild_core::subprocess::run_command(
        &["git", "-C", root, "rev-parse", "HEAD"],
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        head.success(),
        "explicit FastLED root must be a Git checkout"
    );
    let files = fbuild_core::subprocess::run_command(
        &[
            "git",
            "-C",
            root,
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "src",
        ],
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(files.success());
    let mut inputs = std::collections::BTreeMap::new();
    for name in files.stdout.lines() {
        let bytes = std::fs::read(source.join(name)).expect("read actual FastLED source");
        inputs.insert(name.to_owned(), format!("{:x}", Sha256::digest(bytes)));
    }
    serde_json::json!({"root":source,"head":head.stdout.trim(),"src_sha256":inputs})
}

// Capture real build outputs before either success/error assertions can unwind
// and remove the temporary project. Keep nested object paths for disassembly.
fn retain_build_attempt(
    project: &Path,
    identity: &serde_json::Value,
    mode: &str,
    outcome: &Result<fbuild_core::Result<fbuild_build::BuildResult>, tokio::time::error::Elapsed>,
) {
    let Some(root) = std::env::var_os("FBUILD_CI13XX_ARTIFACT_DIR") else {
        return;
    };
    let parent = NormalizedPath::new(root).join(mode);
    std::fs::create_dir_all(&parent).unwrap();
    let output = tempfile::Builder::new()
        .prefix("attempt-")
        .tempdir_in(parent)
        .unwrap()
        .keep();
    eprintln!("retained CI13xx build attempt at {}", output.display());
    for entry in walkdir::WalkDir::new(project).follow_links(false) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(project).unwrap();
        let target = output.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(entry.path(), target).unwrap();
    }
    let status = match outcome {
        Err(error) => format!("timeout: {error}"),
        Ok(Err(error)) => format!("build error: {error}"),
        Ok(Ok(result)) => format!("build returned success={}", result.success),
    };
    std::fs::write(output.join("build-outcome.txt"), status).unwrap();
    std::fs::write(
        output.join("input-provenance.json"),
        serde_json::to_vec_pretty(identity).unwrap(),
    )
    .unwrap();
    if let Ok(Ok(result)) = outcome {
        std::fs::write(
            output.join("size.json"),
            serde_json::to_vec_pretty(&result.size_info).unwrap(),
        )
        .unwrap();
    }
}

fn inspect_and_retain(
    result: &fbuild_build::BuildResult,
    project: &Path,
    identity: &serde_json::Value,
) {
    let elf_path = result.elf_path.as_ref().expect("real consumer ELF");
    let bytes = std::fs::read(elf_path).unwrap();
    let elf = object::File::parse(bytes.as_slice()).unwrap();
    assert_eq!(elf.architecture(), object::Architecture::Riscv32);
    assert!(
        elf.symbols()
            .any(|s| s.is_definition()
                && s.name().is_ok_and(|name| name.contains("CFastLED5countE"))),
        "missing linked out-of-line CFastLED::count definition"
    );
    for symbol in ["ci13xx_spi_scalar", "ci13xx_spi_bulk"] {
        assert!(
            elf.symbols()
                .any(|s| s.name() == Ok(symbol) && s.is_definition()),
            "missing retained {symbol}"
        );
    }
    assert!(
        !elf.symbols()
            .any(|s| s.is_undefined() && !s.is_weak() && s.name().is_ok_and(|n| !n.is_empty())),
        "consumer ELF contains unresolved strong symbols"
    );
    let database = result.compile_database_path.as_ref().unwrap();
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(database).unwrap()).unwrap();
    assert!(
        records
            .iter()
            .any(|r| r["file"].as_str().unwrap_or("").ends_with("/src/main.cpp"))
    );
    for record in &records {
        let arguments = record["arguments"].as_array().expect("native GNU argv");
        assert!(arguments.iter().any(|a| a.as_str() == Some("-fno-lto")));
        assert!(!arguments.iter().any(|a| a.as_str() == Some("-flto")));
        assert!(arguments[0].as_str().unwrap().contains("riscv-nuclei-elf-"));
    }
    let link_command =
        NormalizedPath::new(elf_path.parent().unwrap()).join("native_link_command.json");
    let archives = inspect_linked_archives(&link_command);
    if let Some(output) = std::env::var_os("FBUILD_CI13XX_ARTIFACT_DIR") {
        let output = NormalizedPath::new(output).join("fastled-ci1306");
        std::fs::create_dir_all(&output).unwrap();
        let mut artifacts = vec![
            NormalizedPath::new(elf_path),
            NormalizedPath::new(database),
            NormalizedPath::new(project).join("platformio.ini"),
            NormalizedPath::new(project).join("src/main.cpp"),
            link_command,
            NormalizedPath::new(elf_path.with_extension("map")),
        ];
        artifacts.extend(archives);
        for path in artifacts {
            std::fs::copy(&path, output.join(path.file_name().unwrap())).unwrap();
        }
        std::fs::write(
            output.join("input-provenance.json"),
            serde_json::to_vec_pretty(identity).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("size.json"),
            serde_json::to_vec_pretty(&result.size_info).unwrap(),
        )
        .unwrap();
    }
}

fn inspect_linked_archives(link_command: &Path) -> Vec<NormalizedPath> {
    let argv: Vec<String> = serde_json::from_slice(&std::fs::read(link_command).unwrap()).unwrap();
    let archives: Vec<_> = argv
        .iter()
        .filter(|arg| arg.ends_with(".a"))
        .map(NormalizedPath::new)
        .collect();
    assert!(
        archives.iter().any(|p| p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("libfastled.a")),
        "actual native link must include FastLED archive: {argv:?}"
    );
    for path in &archives {
        let data = std::fs::read(path).expect("read archive actually passed to linker");
        let archive = object::read::archive::ArchiveFile::parse(data.as_slice()).unwrap();
        let mut objects = 0;
        for member in archive.members() {
            let member = member.unwrap();
            let bytes = member.data(data.as_slice()).unwrap();
            let object =
                object::File::parse(bytes).expect("linked archive member must be a real object");
            assert_eq!(object.architecture(), object::Architecture::Riscv32);
            for section in object.sections() {
                assert!(
                    !section.name().unwrap().starts_with(".gnu.lto_"),
                    "no-LTO archive {} contains LTO member {:?}",
                    path.display(),
                    member.name()
                );
            }
            objects += 1;
        }
        assert!(objects > 0, "empty library archive {}", path.display());
    }
    archives
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: requires FBUILD_FASTLED_SOURCE checkout and Nuclei GCC/Chipintelli downloads; compiles actual FastLED"]
async fn ci1306_real_fastled_software_spi_no_lto() {
    let source = canonicalize_existing(std::env::var_os("FBUILD_FASTLED_SOURCE")
        .expect("set FBUILD_FASTLED_SOURCE to the actual FastLED checkout; no substitute fixture is allowed"))
        .await.unwrap();
    assert!(
        source
            .join("src/platforms/shared/spi_bitbang/generic_software_spi.h")
            .is_file()
    );
    let identity = input_identity(&source).await;
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(NormalizedPath::new(project).join("platformio.ini"), format!(
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto\nlib_deps = FastLED=symlink://{}\n", source.display())).unwrap();
    std::fs::write(NormalizedPath::new(project).join("src/main.cpp"), PROBE).unwrap();
    let report = provision_env(project, "ci1306", provision::ProvisionMode::Install)
        .await
        .expect("native CI13xx must resolve real local FastLED lib_deps");
    assert!(!report.failed());
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1200),
        fbuild_build::get_orchestrator(Platform::Ci13xx)
            .unwrap()
            .build(&params(project)),
    )
    .await;
    retain_build_attempt(project, &identity, "fastled-ci1306", &result);
    let result = result
        .expect("consumer build timeout")
        .expect("actual FastLED native compile/link");
    assert!(result.success);
    assert_eq!(
        identity,
        input_identity(&source).await,
        "FastLED inputs changed during build"
    );
    inspect_and_retain(&result, project, &identity);
}

fn inspect_default_lto(result: &fbuild_build::BuildResult) -> Vec<NormalizedPath> {
    let elf_path = result.elf_path.as_ref().unwrap();
    let data = std::fs::read(elf_path).unwrap();
    let elf = object::File::parse(data.as_slice()).unwrap();
    assert_eq!(elf.architecture(), object::Architecture::Riscv32);
    for name in ["ci13xx_spi_scalar", "ci13xx_spi_bulk"] {
        assert!(
            elf.symbols()
                .any(|s| s.name() == Ok(name) && s.is_definition())
        );
    }
    assert!(
        !elf.symbols().any(|s| s.is_undefined()
            && !s.is_weak()
            && s.name().is_ok_and(|name| !name.is_empty()))
    );
    let database = result.compile_database_path.as_ref().unwrap();
    let records: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(database).unwrap()).unwrap();
    assert!(!records.is_empty());
    for record in records {
        let argv = record["arguments"].as_array().unwrap();
        // Vendor compile_sdk_sources.py keeps SDK calls opaque so ld's
        // scheduler --wrap can create the Arduino task. Core/app stay LTO.
        let sdk = NormalizedPath::new(record["file"].as_str().unwrap())
            .display_slash()
            .contains("/tools/sdk/src/");
        let required = if sdk { "-fno-lto" } else { "-flto" };
        let forbidden = if sdk { "-flto" } else { "-fno-lto" };
        assert!(argv.iter().any(|a| a.as_str() == Some(required)));
        assert!(!argv.iter().any(|a| a.as_str() == Some(forbidden)));
        assert!(argv[0].as_str().unwrap().contains("riscv-nuclei-elf-"));
    }
    let link_path =
        NormalizedPath::new(elf_path.parent().unwrap()).join("native_link_command.json");
    let argv: Vec<String> = serde_json::from_slice(&std::fs::read(&link_path).unwrap()).unwrap();
    assert!(argv.iter().any(|a| a == "-flto"));
    assert!(!argv.iter().any(|a| a == "-fno-lto"));
    let archive = argv
        .iter()
        .map(NormalizedPath::new)
        .find(|p| {
            p.file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("libfastled.a"))
        })
        .expect("real FastLED archive in default LTO link");
    let data = std::fs::read(&archive).unwrap();
    let members = object::read::archive::ArchiveFile::parse(data.as_slice()).unwrap();
    let mut lto_objects = 0;
    for member in members.members() {
        let member = member.unwrap();
        let object = object::File::parse(member.data(data.as_slice()).unwrap()).unwrap();
        if object
            .sections()
            .any(|s| s.name().unwrap().starts_with(".gnu.lto_"))
        {
            lto_objects += 1;
        }
    }
    assert!(
        lto_objects > 0,
        "default library build must actually emit GCC LTO objects"
    );
    vec![
        NormalizedPath::new(elf_path),
        NormalizedPath::new(database),
        link_path,
        NormalizedPath::new(elf_path.with_extension("map")),
        archive,
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: requires actual FBUILD_FASTLED_SOURCE, Nuclei GCC/Chipintelli packages and real default-LTO SDK/library link"]
async fn ci1306_real_fastled_software_spi_default_lto() {
    let source = canonicalize_existing(
        std::env::var_os("FBUILD_FASTLED_SOURCE")
            .expect("set actual FBUILD_FASTLED_SOURCE checkout"),
    )
    .await
    .unwrap();
    assert!(
        source
            .join("src/platforms/shared/spi_bitbang/generic_software_spi.h")
            .is_file()
    );
    let identity = input_identity(&source).await;
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::create_dir(project.join("src")).unwrap();
    // Intentionally no build_flags/build_unflags: exercise vendor defaults.
    std::fs::write(NormalizedPath::new(project).join("platformio.ini"), format!(
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nlib_deps = FastLED=symlink://{}\n", source.display())).unwrap();
    std::fs::write(NormalizedPath::new(project).join("src/main.cpp"), PROBE).unwrap();
    let report = provision_env(project, "ci1306", provision::ProvisionMode::Install)
        .await
        .unwrap();
    assert!(!report.failed());
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1200),
        fbuild_build::get_orchestrator(Platform::Ci13xx)
            .unwrap()
            .build(&params(project)),
    )
    .await;
    retain_build_attempt(project, &identity, "fastled-ci1306-default-lto", &result);
    let result = result
        .expect("default LTO build timeout")
        .expect("actual FastLED default-LTO build/link");
    assert!(result.success);
    assert_eq!(identity, input_identity(&source).await);
    let mut artifacts = inspect_default_lto(&result);
    artifacts.push(inspect_scheduler_reachability(&result).await);
    if let Some(output) = std::env::var_os("FBUILD_CI13XX_ARTIFACT_DIR") {
        let output = NormalizedPath::new(output).join("fastled-ci1306-default-lto");
        std::fs::create_dir_all(&output).unwrap();
        artifacts.extend([
            NormalizedPath::new(project).join("platformio.ini"),
            NormalizedPath::new(project).join("src/main.cpp"),
        ]);
        for path in artifacts {
            std::fs::copy(&path, output.join(path.file_name().unwrap())).unwrap();
        }
        std::fs::write(
            output.join("input-provenance.json"),
            serde_json::to_vec_pretty(&identity).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("size.json"),
            serde_json::to_vec_pretty(&result.size_info).unwrap(),
        )
        .unwrap();
    }
}

async fn inspect_scheduler_reachability(result: &fbuild_build::BuildResult) -> NormalizedPath {
    let elf_path = result.elf_path.as_ref().unwrap();
    let data = std::fs::read(elf_path).unwrap();
    let elf = object::File::parse(data.as_slice()).unwrap();
    let task = elf
        .symbols()
        .find(|s| s.is_definition() && s.name().is_ok_and(|name| name.contains("arduinoTask")))
        .expect("reachable real Arduino task definition")
        .name()
        .unwrap()
        .to_owned();
    let link: Vec<String> = serde_json::from_slice(
        &std::fs::read(elf_path.parent().unwrap().join("native_link_command.json")).unwrap(),
    )
    .unwrap();
    let objdump =
        NormalizedPath::new(Path::new(&link[0]).with_file_name("riscv-nuclei-elf-objdump"));
    let mut bodies = std::collections::HashMap::new();
    let mut evidence = String::new();
    for symbol in ["main", "__wrap_vTaskStartScheduler", task.as_str()] {
        let output = fbuild_core::subprocess::run_command(
            &[
                objdump.to_str().unwrap(),
                "-d",
                &format!("--disassemble={symbol}"),
                elf_path.to_str().unwrap(),
            ],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(output.success(), "native objdump failed: {}", output.stderr);
        evidence.push_str(&output.stdout);
        bodies.insert(symbol.to_owned(), output.stdout);
    }
    let path = NormalizedPath::new(elf_path.with_extension("scheduler-disassembly.txt"));
    std::fs::write(&path, &evidence).unwrap();
    if let Some(root) = std::env::var_os("FBUILD_CI13XX_ARTIFACT_DIR") {
        let output = NormalizedPath::new(root).join("fastled-ci1306-default-lto");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("scheduler-disassembly.txt"), &evidence).unwrap();
    }
    assert!(
        bodies["main"].contains("<__wrap_vTaskStartScheduler>"),
        "SDK main must actually call the Arduino scheduler wrapper"
    );
    let wrapper = &bodies["__wrap_vTaskStartScheduler"];
    assert!(
        wrapper.contains(&format!("<{task}>")),
        "wrapper must supply actual Arduino task address"
    );
    assert!(
        wrapper.contains("<xTaskCreate>"),
        "wrapper must create that task"
    );
    assert!(
        wrapper.contains("<vTaskStartScheduler>"),
        "wrapper must start real SDK scheduler"
    );
    let body = &bodies[&task];
    assert!(
        body.contains("<setup>"),
        "Arduino task must call real sketch setup"
    );
    assert!(
        body.contains("<loop>"),
        "Arduino task must call real sketch loop"
    );
    path
}

fn stage_project_library(project: &Path, source: &Path, identity: &serde_json::Value) {
    // Match FastLED's compile wrapper: contents of src/ at lib/FastLED/,
    // plus the real manifest. Never copy the checkout or generated .build.
    let library = project.join("lib/FastLED");
    std::fs::create_dir_all(&library).unwrap();
    for (name, digest) in identity["src_sha256"].as_object().unwrap() {
        let relative = name.strip_prefix("src/").expect("source-only inventory");
        let bytes = std::fs::read(source.join(name)).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            digest.as_str().unwrap()
        );
        let destination = library.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, bytes).unwrap();
    }
    let manifest = std::fs::read(source.join("library.json")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&manifest)),
        identity["library_json_sha256"].as_str().unwrap()
    );
    std::fs::write(library.join("library.json"), manifest).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "#1914: requires actual FBUILD_FASTLED_SOURCE checkout and Nuclei GCC/SDK; compiles standard project lib/FastLED layout"]
async fn ci1306_real_fastled_project_library_no_lto() {
    let source = canonicalize_existing(
        std::env::var_os("FBUILD_FASTLED_SOURCE")
            .expect("set actual FBUILD_FASTLED_SOURCE checkout"),
    )
    .await
    .unwrap();
    let mut identity = input_identity(&source).await;
    let manifest = std::fs::read(source.join("library.json")).expect("actual FastLED manifest");
    identity["library_json_sha256"] = format!("{:x}", Sha256::digest(&manifest)).into();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    std::fs::create_dir(project.join("src")).unwrap();
    stage_project_library(project, &source, &identity);
    // No lib_deps: ordinary project-local library discovery is the contract.
    std::fs::write(NormalizedPath::new(project).join("platformio.ini"),
        "[env:ci1306]\nplatform = ci13xx\nboard = ci1306\nframework = arduino\nbuild_unflags = -flto\nbuild_flags = -fno-lto\n").unwrap();
    std::fs::write(NormalizedPath::new(project).join("src/main.cpp"), PROBE).unwrap();
    let report = provision_env(project, "ci1306", provision::ProvisionMode::Install)
        .await
        .unwrap();
    assert!(!report.failed());
    let backend = compile_backend::CompileBackend::start().await.unwrap();
    compile_backend::install_global(backend);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1200),
        fbuild_build::get_orchestrator(Platform::Ci13xx)
            .unwrap()
            .build(&params(project)),
    )
    .await;
    retain_build_attempt(
        project,
        &identity,
        "fastled-ci1306-project-library",
        &result,
    );
    let result = result
        .expect("project library build timeout")
        .expect("native CI1306 must discover, compile and link actual project-local FastLED");
    assert!(result.success);
    let mut after = input_identity(&source).await;
    after["library_json_sha256"] = format!(
        "{:x}",
        Sha256::digest(std::fs::read(source.join("library.json")).unwrap())
    )
    .into();
    assert_eq!(
        identity, after,
        "actual source/manifest changed during build"
    );
    inspect_and_retain(&result, project, &identity);
}
